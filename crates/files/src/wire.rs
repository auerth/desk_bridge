//! One file per (uni) stream:
//! `[FileHeader frame][chunk frames ...][u32 0][FileTrailer frame]`
//!
//! A chunk frame is `u32 LE length | raw bytes` (length <= [`CHUNK`]); length 0 ends the data.
//! Memory use is bounded by one 256 KiB buffer per side regardless of the file size.

use crate::sanitize::{safe_join, unique_candidates};
use crate::scan::PlannedFile;
use crate::{CHUNK, FileError, Result};
use deskbridge_proto::frame::{read_msg, write_msg};
use deskbridge_proto::pb;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

/// Suffix of partially received files.
pub const PART_SUFFIX: &str = ".part";

/// Outcome of a successfully received (and verified) file.
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    pub transfer_id: u64,
    pub index: u32,
    pub entry: pb::FileEntry,
    /// Final location (may differ from the requested name after collision handling).
    pub path: PathBuf,
    /// Bytes received on this stream (excludes the resumed prefix).
    pub bytes: u64,
    /// BLAKE3 of the whole file.
    pub blake3: [u8; 32],
}

fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

/// Reads until `buf` is full or EOF; returns the number of bytes read.
async fn read_full<R: AsyncRead + Unpin>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = r.read(&mut buf[n..]).await?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

/// Sends one planned file (or directory entry) on `w`, starting at `offset` (resume).
///
/// Returns the BLAKE3 of the whole file (also the part below `offset`, which is re-read but not
/// sent). `progress` receives the number of payload bytes written per chunk.
pub async fn send_file<W: AsyncWrite + Unpin>(
    w: &mut W,
    transfer_id: u64,
    index: u32,
    planned: &PlannedFile,
    offset: u64,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<[u8; 32]> {
    if planned.entry.is_dir {
        return send_reader(
            w,
            transfer_id,
            index,
            &planned.entry,
            tokio::io::empty(),
            0,
            progress,
        )
        .await;
    }
    let file = tokio::fs::File::open(&planned.abs).await?;
    send_reader(
        w,
        transfer_id,
        index,
        &planned.entry,
        file,
        offset,
        progress,
    )
    .await
}

/// Same as [`send_file`] but reads the content from any `AsyncRead` (starting at byte 0; the
/// prefix below `offset` is consumed for hashing only).
pub async fn send_reader<W, R>(
    w: &mut W,
    transfer_id: u64,
    index: u32,
    entry: &pb::FileEntry,
    mut reader: R,
    offset: u64,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<[u8; 32]>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    if offset > entry.size {
        return Err(FileError::Protocol("Offset hinter dem Dateiende"));
    }
    write_msg(
        w,
        &pb::FileHeader {
            transfer_id,
            index,
            entry: Some(entry.clone()),
            offset,
        },
    )
    .await?;

    let mut hasher = blake3::Hasher::new();
    // 4 bytes of length prefix in front of the payload so each chunk goes out in a single write.
    let mut buf = vec![0u8; 4 + CHUNK];

    let data: Result<()> = async {
        let mut skipped = 0u64;
        while skipped < offset {
            let want = (offset - skipped).min(CHUNK as u64) as usize;
            let n = read_full(&mut reader, &mut buf[4..4 + want]).await?;
            if n < want {
                return Err(FileError::SourceChanged);
            }
            hasher.update(&buf[4..4 + n]);
            skipped += n as u64;
        }
        let mut remaining = entry.size - offset;
        while remaining > 0 {
            let want = remaining.min(CHUNK as u64) as usize;
            let n = read_full(&mut reader, &mut buf[4..4 + want]).await?;
            if n < want {
                return Err(FileError::SourceChanged);
            }
            hasher.update(&buf[4..4 + n]);
            buf[..4].copy_from_slice(&(n as u32).to_le_bytes());
            w.write_all(&buf[..4 + n]).await?;
            progress(n as u64);
            remaining -= n as u64;
        }
        Ok(())
    }
    .await;

    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    match data {
        Ok(()) => {
            w.write_all(&0u32.to_le_bytes()).await?;
            write_msg(
                w,
                &pb::FileTrailer {
                    blake3: hash.to_vec(),
                    ok: true,
                },
            )
            .await?;
            w.flush().await?;
            Ok(hash)
        }
        Err(e) => {
            // Best effort: tell the receiver the data is incomplete so it keeps the .part file.
            let _ = w.write_all(&0u32.to_le_bytes()).await;
            let _ = write_msg(
                w,
                &pb::FileTrailer {
                    blake3: Vec::new(),
                    ok: false,
                },
            )
            .await;
            let _ = w.flush().await;
            Err(e)
        }
    }
}

/// Number of bytes of a previous partial download that can be reused, to be sent in
/// `FileRequest.offset`. Always a multiple of [`CHUNK`] and at most `entry.size`.
#[must_use]
pub fn resume_offset(dest_root: &Path, entry: &pb::FileEntry) -> u64 {
    if entry.is_dir {
        return 0;
    }
    let Ok(target) = safe_join(dest_root, &entry.rel_path) else {
        return 0;
    };
    let Ok(meta) = std::fs::metadata(part_path(&target)) else {
        return 0;
    };
    let usable = meta.len().min(entry.size);
    usable / CHUNK as u64 * CHUNK as u64
}

/// Removes the leftover `.part` file of `entry`, if any (e.g. when the user cancels).
pub fn discard_partial(dest_root: &Path, entry: &pb::FileEntry) {
    if let Ok(target) = safe_join(dest_root, &entry.rel_path) {
        let _ = std::fs::remove_file(part_path(&target));
    }
}

fn to_system_time(ms: i64) -> Option<SystemTime> {
    if ms <= 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_millis(ms as u64))
}

/// Receives one file stream into `dest_root`.
///
/// Data goes to `<name>.part`; after the BLAKE3 of the complete content matched the trailer, the
/// modification time is applied and the file is atomically renamed. An existing file with the
/// same name is never overwritten (`name (1).ext`, ...). On a transport error the `.part` file
/// stays for [`resume_offset`]; on a verification failure it is deleted.
pub async fn receive_file<R: AsyncRead + Unpin>(
    r: &mut R,
    dest_root: &Path,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<ReceivedFile> {
    let header: pb::FileHeader = read_msg(r).await?;
    let entry = header
        .entry
        .clone()
        .ok_or(FileError::Protocol("Header ohne Dateieintrag"))?;
    let target = safe_join(dest_root, &entry.rel_path)?;

    if entry.is_dir {
        return receive_dir(r, &header, entry, target).await;
    }

    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
        ensure_inside(dest_root, parent).await?;
    }
    let part = part_path(&target);
    let res = receive_body(r, &header, &entry, &target, &part, progress).await;
    match res {
        Ok(v) => Ok(v),
        Err(e) => {
            if e.is_corrupt() {
                if let Err(rm) = tokio::fs::remove_file(&part).await {
                    if rm.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!("konnte {} nicht entfernen: {rm}", part.display());
                    }
                }
            }
            Err(e)
        }
    }
}

/// Rejects a parent directory that resolves (via symlink/junction) outside `root`.
async fn ensure_inside(root: &Path, parent: &Path) -> Result<()> {
    let (Ok(r), Ok(p)) = (
        tokio::fs::canonicalize(root).await,
        tokio::fs::canonicalize(parent).await,
    ) else {
        return Err(FileError::Protocol("Zielordner nicht auflösbar"));
    };
    if p.starts_with(&r) {
        Ok(())
    } else {
        Err(FileError::Protocol("Ziel außerhalb des Zielordners"))
    }
}

async fn read_terminator<R: AsyncRead + Unpin>(r: &mut R) -> Result<()> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    if u32::from_le_bytes(len) != 0 {
        return Err(FileError::Protocol("Daten für ein Verzeichnis"));
    }
    Ok(())
}

async fn receive_dir<R: AsyncRead + Unpin>(
    r: &mut R,
    header: &pb::FileHeader,
    entry: pb::FileEntry,
    target: PathBuf,
) -> Result<ReceivedFile> {
    read_terminator(r).await?;
    let trailer: pb::FileTrailer = read_msg(r).await?;
    if !trailer.ok {
        return Err(FileError::SourceChanged);
    }
    tokio::fs::create_dir_all(&target).await?;
    let hash: [u8; 32] = *blake3::hash(&[]).as_bytes();
    Ok(ReceivedFile {
        transfer_id: header.transfer_id,
        index: header.index,
        entry,
        path: target,
        bytes: 0,
        blake3: hash,
    })
}

async fn receive_body<R: AsyncRead + Unpin>(
    r: &mut R,
    header: &pb::FileHeader,
    entry: &pb::FileEntry,
    target: &Path,
    part: &Path,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<ReceivedFile> {
    let offset = header.offset;
    if offset > entry.size {
        return Err(FileError::Protocol("Offset hinter dem Dateiende"));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(offset == 0)
        .open(part)
        .await?;

    if offset > 0 {
        // Re-hash the already present prefix so the final hash covers the complete file.
        let mut done = 0u64;
        while done < offset {
            let want = (offset - done).min(CHUNK as u64) as usize;
            if read_full_async(&mut file, &mut buf[..want]).await? < want {
                return Err(FileError::ResumeInvalid(
                    "Teildatei ist kürzer als der Offset",
                ));
            }
            hasher.update(&buf[..want]);
            done += want as u64;
        }
        file.set_len(offset).await?;
        file.seek(SeekFrom::Start(offset)).await?;
    }

    let stream = receive_stream(
        r,
        &mut file,
        &mut hasher,
        &mut buf,
        entry.size - offset,
        progress,
    )
    .await;
    // Make sure everything queued in tokio's file buffer reached the OS, also on errors, so a
    // later resume sees all complete chunks.
    let flushed = file.flush().await;
    let received = stream?;
    flushed?;

    let trailer: pb::FileTrailer = read_msg(r).await?;
    if !trailer.ok {
        return Err(FileError::SourceChanged);
    }
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    if received != entry.size - offset {
        return Err(FileError::SizeMismatch);
    }
    if trailer.blake3.as_slice() != hash.as_slice() {
        return Err(FileError::HashMismatch);
    }

    // Apply mtime, close, then rename without ever replacing an existing file.
    let std_file = file.into_std().await;
    let mtime = to_system_time(entry.mtime_unix_ms);
    let part_owned = part.to_path_buf();
    let target_owned = target.to_path_buf();
    let final_path = tokio::task::spawn_blocking(move || -> std::io::Result<PathBuf> {
        if let Some(t) = mtime {
            if let Err(e) = std_file.set_modified(t) {
                tracing::warn!("mtime konnte nicht gesetzt werden: {e}");
            }
        }
        drop(std_file);
        finalize_rename(&part_owned, &target_owned)
    })
    .await
    .map_err(|_| FileError::Protocol("Abschluss abgebrochen"))??;

    Ok(ReceivedFile {
        transfer_id: header.transfer_id,
        index: header.index,
        entry: entry.clone(),
        path: final_path,
        bytes: received,
        blake3: hash,
    })
}

async fn read_full_async(file: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    read_full(file, buf).await
}

/// Reads chunk frames into `file`; returns the number of payload bytes.
async fn receive_stream<R: AsyncRead + Unpin>(
    r: &mut R,
    file: &mut tokio::fs::File,
    hasher: &mut blake3::Hasher,
    buf: &mut [u8],
    expected: u64,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<u64> {
    let mut received = 0u64;
    loop {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len == 0 {
            return Ok(received);
        }
        if len > CHUNK {
            return Err(FileError::Protocol("Chunk zu groß"));
        }
        if received + len as u64 > expected {
            return Err(FileError::SizeMismatch);
        }
        r.read_exact(&mut buf[..len]).await?;
        hasher.update(&buf[..len]);
        file.write_all(&buf[..len]).await?;
        received += len as u64;
        progress(len as u64);
    }
}

/// Moves `part` to `target`, picking `name (n).ext` if `target` exists. The name is reserved with
/// `create_new` first, so concurrent receivers cannot pick the same one.
fn finalize_rename(part: &Path, target: &Path) -> std::io::Result<PathBuf> {
    let candidates = std::iter::once(target.to_path_buf()).chain(unique_candidates(target));
    for cand in candidates {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cand)
        {
            Ok(placeholder) => {
                drop(placeholder);
                if let Err(e) = std::fs::rename(part, &cand) {
                    let _ = std::fs::remove_file(&cand);
                    return Err(e);
                }
                return Ok(cand);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // A directory of the same name makes Windows answer PermissionDenied.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && cand.is_dir() => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("candidate iterator is infinite")
}
