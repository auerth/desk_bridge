//! Functional tests of the transfer engine over in-memory streams.

use deskbridge_files::scan::{PlannedFile, plan};
use deskbridge_files::wire::{PART_SUFFIX, ReceivedFile, receive_file, resume_offset, send_file};
use deskbridge_files::{CHUNK, FileError};
use deskbridge_proto::pb;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

/// Deterministic test data.
fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

fn planned(path: &Path, rel: &str) -> PlannedFile {
    let meta = std::fs::metadata(path).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    PlannedFile {
        abs: path.to_path_buf(),
        entry: pb::FileEntry {
            rel_path: rel.into(),
            size: meta.len(),
            mtime_unix_ms: mtime,
            is_dir: meta.is_dir(),
        },
    }
}

/// Sends into a buffer (the whole stream as bytes).
async fn send_to_vec(p: &PlannedFile, offset: u64) -> Vec<u8> {
    let mut out = Vec::new();
    send_file(&mut out, 7, 0, p, offset, &mut |_| {})
        .await
        .unwrap();
    out
}

async fn recv_from(bytes: &[u8], root: &Path) -> Result<ReceivedFile, FileError> {
    let mut r = bytes;
    receive_file(&mut r, root, &mut |_| {}).await
}

fn part_of(root: &Path, rel: &str) -> PathBuf {
    let mut s = root.join(rel).into_os_string();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

#[tokio::test]
async fn roundtrip_small_and_multi_chunk() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    for (name, len) in [
        ("a.bin", 1usize),
        ("b.bin", CHUNK),
        ("c.bin", CHUNK + 1),
        ("d.bin", 3 * CHUNK + 12345),
    ] {
        let data = pattern(len, len as u64);
        let p = src.path().join(name);
        std::fs::write(&p, &data).unwrap();
        let pf = planned(&p, name);
        let stream = send_to_vec(&pf, 0).await;
        let got = recv_from(&stream, dst.path()).await.unwrap();
        assert_eq!(std::fs::read(&got.path).unwrap(), data, "{name}");
        assert_eq!(got.blake3, *blake3::hash(&data).as_bytes());
        assert_eq!(got.bytes, len as u64);
        assert!(!part_of(dst.path(), name).exists());
    }
}

#[tokio::test]
async fn empty_file_and_empty_dir() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let f = src.path().join("empty.txt");
    std::fs::write(&f, b"").unwrap();
    let d = src.path().join("emptydir");
    std::fs::create_dir(&d).unwrap();

    let got = recv_from(&send_to_vec(&planned(&f, "empty.txt"), 0).await, dst.path())
        .await
        .unwrap();
    assert_eq!(std::fs::metadata(&got.path).unwrap().len(), 0);

    let got = recv_from(
        &send_to_vec(&planned(&d, "x/emptydir"), 0).await,
        dst.path(),
    )
    .await
    .unwrap();
    assert!(got.path.is_dir());
    assert!(dst.path().join("x/emptydir").is_dir());
}

#[tokio::test]
async fn unicode_names_roundtrip() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let rel = "Über größe/日本語 😀.txt";
    let p = src.path().join("u.txt");
    std::fs::write(&p, "grüße").unwrap();
    let got = recv_from(&send_to_vec(&planned(&p, rel), 0).await, dst.path())
        .await
        .unwrap();
    assert_eq!(
        got.path,
        dst.path().join("Über größe").join("日本語 😀.txt")
    );
    assert_eq!(std::fs::read_to_string(got.path).unwrap(), "grüße");
}

#[tokio::test]
async fn mtime_is_preserved() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let p = src.path().join("old.txt");
    std::fs::write(&p, b"old").unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(t)
        .unwrap();
    let got = recv_from(&send_to_vec(&planned(&p, "old.txt"), 0).await, dst.path())
        .await
        .unwrap();
    assert_eq!(std::fs::metadata(got.path).unwrap().modified().unwrap(), t);
}

#[tokio::test]
async fn name_collision_never_overwrites() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(dst.path().join("doc.txt"), b"mine").unwrap();
    let p = src.path().join("doc.txt");
    std::fs::write(&p, b"theirs").unwrap();
    let pf = planned(&p, "doc.txt");
    let stream = send_to_vec(&pf, 0).await;
    let a = recv_from(&stream, dst.path()).await.unwrap();
    let b = recv_from(&stream, dst.path()).await.unwrap();
    assert_eq!(std::fs::read(dst.path().join("doc.txt")).unwrap(), b"mine");
    assert_eq!(a.path, dst.path().join("doc (1).txt"));
    assert_eq!(b.path, dst.path().join("doc (2).txt"));
    assert_eq!(std::fs::read(&b.path).unwrap(), b"theirs");
}

#[tokio::test]
async fn corruption_is_detected_and_never_renamed() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data = pattern(2 * CHUNK + 999, 5);
    let p = src.path().join("c.bin");
    std::fs::write(&p, &data).unwrap();
    let mut stream = send_to_vec(&planned(&p, "c.bin"), 0).await;
    let mid = stream.len() / 2;
    stream[mid] ^= 0x01;
    let err = recv_from(&stream, dst.path()).await.unwrap_err();
    assert!(matches!(err, FileError::HashMismatch), "{err:?}");
    assert!(
        !dst.path().join("c.bin").exists(),
        "corrupt file must not appear"
    );
    assert!(
        !part_of(dst.path(), "c.bin").exists(),
        "corrupt .part must be discarded"
    );
    assert_eq!(resume_offset(dst.path(), &planned(&p, "c.bin").entry), 0);
}

#[tokio::test]
async fn oversized_chunk_and_bad_paths_are_rejected() {
    let dst = tempfile::tempdir().unwrap();
    // A header with a traversal path.
    for rel in [
        "../evil.txt",
        "C:\\evil",
        "a/../../evil",
        "x/NUL",
        "f.txt:ads",
    ] {
        let hdr = pb::FileHeader {
            transfer_id: 1,
            index: 0,
            entry: Some(pb::FileEntry {
                rel_path: rel.into(),
                size: 3,
                mtime_unix_ms: 0,
                is_dir: false,
            }),
            offset: 0,
        };
        let mut s = deskbridge_proto::frame::encode(&hdr);
        s.extend_from_slice(&3u32.to_le_bytes());
        s.extend_from_slice(b"abc");
        let err = recv_from(&s, dst.path()).await.unwrap_err();
        assert!(matches!(err, FileError::Path(_)), "{rel}: {err:?}");
    }
    assert!(
        std::fs::read_dir(dst.path()).unwrap().next().is_none(),
        "nothing may be created"
    );
    // Chunk bigger than CHUNK.
    let hdr = pb::FileHeader {
        transfer_id: 1,
        index: 0,
        entry: Some(pb::FileEntry {
            rel_path: "big.bin".into(),
            size: u64::MAX / 2,
            mtime_unix_ms: 0,
            is_dir: false,
        }),
        offset: 0,
    };
    let mut s = deskbridge_proto::frame::encode(&hdr);
    s.extend_from_slice(&((CHUNK as u32) + 1).to_le_bytes());
    let err = recv_from(&s, dst.path()).await.unwrap_err();
    assert!(matches!(err, FileError::Protocol(_)), "{err:?}");
    // More data than announced.
    let hdr = pb::FileHeader {
        transfer_id: 1,
        index: 0,
        entry: Some(pb::FileEntry {
            rel_path: "short.bin".into(),
            size: 2,
            mtime_unix_ms: 0,
            is_dir: false,
        }),
        offset: 0,
    };
    let mut s = deskbridge_proto::frame::encode(&hdr);
    s.extend_from_slice(&3u32.to_le_bytes());
    s.extend_from_slice(b"abc");
    let err = recv_from(&s, dst.path()).await.unwrap_err();
    assert!(matches!(err, FileError::SizeMismatch), "{err:?}");
    assert!(!dst.path().join("short.bin").exists());
}

#[tokio::test]
async fn resume_after_mid_stream_abort() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data = pattern(3 * CHUNK + 4321, 99);
    let p = src.path().join("big.bin");
    std::fs::write(&p, &data).unwrap();
    let pf = planned(&p, "dir/big.bin");

    // First attempt: the connection dies in the middle of the third chunk.
    let mut stream = send_to_vec(&pf, 0).await;
    stream.truncate(2 * (CHUNK + 4) + 200 + CHUNK / 2);
    let err = recv_from(&stream, dst.path()).await.unwrap_err();
    assert!(!err.is_corrupt(), "{err:?}");
    assert!(!dst.path().join("dir/big.bin").exists());
    let part = part_of(dst.path(), "dir/big.bin");
    assert!(part.exists(), ".part must be kept for resume");

    // Second attempt starts at the valid prefix.
    let off = resume_offset(dst.path(), &pf.entry);
    assert_eq!(off, 2 * CHUNK as u64);
    let mut resumed = Vec::new();
    let mut sent = 0u64;
    let hash = send_file(&mut resumed, 7, 0, &pf, off, &mut |n| sent += n)
        .await
        .unwrap();
    assert_eq!(sent, data.len() as u64 - off);
    let got = recv_from(&resumed, dst.path()).await.unwrap();
    assert_eq!(std::fs::read(&got.path).unwrap(), data);
    assert_eq!(got.blake3, hash);
    assert_eq!(got.blake3, *blake3::hash(&data).as_bytes());
    assert_eq!(got.bytes, sent);
    assert!(!part.exists());
}

#[tokio::test]
async fn resume_with_tampered_prefix_fails_and_discards() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data = pattern(2 * CHUNK + 10, 3);
    let p = src.path().join("f.bin");
    std::fs::write(&p, &data).unwrap();
    let pf = planned(&p, "f.bin");
    let mut stream = send_to_vec(&pf, 0).await;
    stream.truncate(CHUNK + 100);
    let _ = recv_from(&stream, dst.path()).await.unwrap_err();
    // Corrupt the partial file on disk.
    let part = part_of(dst.path(), "f.bin");
    let mut bytes = std::fs::read(&part).unwrap();
    assert!(bytes.len() >= CHUNK);
    bytes[10] ^= 0xFF;
    std::fs::write(&part, &bytes).unwrap();
    let off = resume_offset(dst.path(), &pf.entry);
    let resumed = send_to_vec(&pf, off).await;
    let err = recv_from(&resumed, dst.path()).await.unwrap_err();
    assert!(matches!(err, FileError::HashMismatch), "{err:?}");
    assert!(!dst.path().join("f.bin").exists());
    assert!(!part.exists());
}

#[tokio::test]
async fn resume_offset_is_chunk_aligned_and_capped() {
    let dst = tempfile::tempdir().unwrap();
    let entry = pb::FileEntry {
        rel_path: "x.bin".into(),
        size: 100,
        mtime_unix_ms: 0,
        is_dir: false,
    };
    assert_eq!(resume_offset(dst.path(), &entry), 0);
    std::fs::write(part_of(dst.path(), "x.bin"), vec![0u8; 5000]).unwrap();
    assert_eq!(resume_offset(dst.path(), &entry), 0, "less than one chunk");
    let entry = pb::FileEntry {
        size: 10 * CHUNK as u64,
        ..entry
    };
    std::fs::write(part_of(dst.path(), "x.bin"), vec![0u8; 3 * CHUNK + 5]).unwrap();
    assert_eq!(resume_offset(dst.path(), &entry), 3 * CHUNK as u64);
    std::fs::write(part_of(dst.path(), "x.bin"), vec![0u8; 12 * CHUNK]).unwrap();
    assert_eq!(resume_offset(dst.path(), &entry), 10 * CHUNK as u64);
    let bad = pb::FileEntry {
        rel_path: "../x".into(),
        ..entry
    };
    assert_eq!(resume_offset(dst.path(), &bad), 0);
}

#[tokio::test]
async fn source_changed_during_send_is_reported_and_part_kept() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let p = src.path().join("s.bin");
    std::fs::write(&p, pattern(2 * CHUNK, 1)).unwrap();
    let mut pf = planned(&p, "s.bin");
    pf.entry.size += 100; // the file "shrank" after planning
    let mut out = Vec::new();
    let err = send_file(&mut out, 1, 0, &pf, 0, &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, FileError::SourceChanged), "{err:?}");
    let err = recv_from(&out, dst.path()).await.unwrap_err();
    assert!(matches!(err, FileError::SourceChanged), "{err:?}");
    assert!(!dst.path().join("s.bin").exists());
}

/// 1000 small files in a folder tree: plan, send over several parallel duplex streams (like the
/// 4 parallel QUIC streams), receive, verify every byte and the empty folders.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thousand_small_files_parallel() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let root = src.path().join("Projekt");
    for d in 0..10 {
        let dir = root.join(format!("ordner {d}"));
        std::fs::create_dir_all(dir.join("leer")).unwrap();
        for f in 0..100 {
            let len = (d * 100 + f) % 700;
            std::fs::write(
                dir.join(format!("datei_{f:03}.dat")),
                pattern(len, (d * 1000 + f) as u64),
            )
            .unwrap();
        }
    }
    let files = plan(std::slice::from_ref(&root)).unwrap();
    assert_eq!(files.iter().filter(|f| !f.entry.is_dir).count(), 1000);
    assert_eq!(files.iter().filter(|f| f.entry.is_dir).count(), 1 + 10 + 10);

    let files = Arc::new(files);
    let dst_root = dst.path().to_path_buf();
    let sem = Arc::new(tokio::sync::Semaphore::new(4));
    let mut tasks = Vec::new();
    for i in 0..files.len() {
        let permit = sem.clone().acquire_owned().await.unwrap();
        let files = files.clone();
        let dst_root = dst_root.clone();
        tasks.push(tokio::spawn(async move {
            let (mut tx, mut rx) = tokio::io::duplex(64 * 1024);
            let f = files.clone();
            let sender = tokio::spawn(async move {
                let h = send_file(&mut tx, 42, i as u32, &f[i], 0, &mut |_| {})
                    .await
                    .unwrap();
                tx.shutdown().await.unwrap();
                h
            });
            let got = receive_file(&mut rx, &dst_root, &mut |_| {}).await.unwrap();
            let sent_hash = sender.await.unwrap();
            assert_eq!(got.blake3, sent_hash);
            assert_eq!(got.index, i as u32);
            drop(permit);
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    for f in files.iter() {
        let target = dst.path().join(&f.entry.rel_path);
        if f.entry.is_dir {
            assert!(target.is_dir(), "{}", f.entry.rel_path);
        } else {
            assert_eq!(
                std::fs::read(&target).unwrap(),
                std::fs::read(&f.abs).unwrap(),
                "{}",
                f.entry.rel_path
            );
        }
    }
    assert!(dst.path().join("Projekt/ordner 3/leer").is_dir());
}

#[tokio::test]
async fn progress_callbacks_sum_up() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let data = pattern(5 * CHUNK + 17, 8);
    let p = src.path().join("p.bin");
    std::fs::write(&p, &data).unwrap();
    let pf = planned(&p, "p.bin");
    let mut sent = 0u64;
    let mut out = Vec::new();
    send_file(&mut out, 1, 0, &pf, 0, &mut |n| sent += n)
        .await
        .unwrap();
    let mut got = 0u64;
    let mut r = out.as_slice();
    receive_file(&mut r, dst.path(), &mut |n| got += n)
        .await
        .unwrap();
    assert_eq!(sent, data.len() as u64);
    assert_eq!(got, data.len() as u64);
}
