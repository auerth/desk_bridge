//! Virtual files (plan 8.6, stage 2): a custom `IDataObject` exposing `FileGroupDescriptorW` +
//! `FileContents` (one `IStream` per file) so Explorer shows its own copy dialog and pulls the
//! data straight from the provider (network) without an intermediate copy.
//!
//! # Threading / COM lifetime
//!
//! The data object is created on the clipboard STA thread and handed to `OleSetClipboard` there.
//! Explorer reaches it through OLE's clipboard proxy: every `IDataObject` call (and, because the
//! returned `IStream`s are created by - and therefore belong to - the same apartment, every
//! `IStream::Read` too) is marshalled back to that STA thread and executed from its message pump.
//! Consequences:
//! * the clipboard thread must keep pumping messages (it does; the pump is the thread's job);
//! * a slow `Read` (network) blocks the clipboard thread for its duration. The provider is
//!   responsible for timeouts; clipboard events/commands are merely delayed meanwhile;
//! * the stream state is still protected by a mutex, so it is also safe if COM ever calls it from
//!   another thread (e.g. when used via the free-threaded marshaler in tests).

use deskbridge_platform::clipboard::{FileOffer, VirtualFileProvider};
use std::io::{self, Read};
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use windows::Win32::Foundation::{
    BOOL, DV_E_DVASPECT, DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_FAIL, E_INVALIDARG, E_NOTIMPL,
    E_POINTER, FILETIME, HGLOBAL, OLE_E_ADVISENOTSUPPORTED, S_FALSE, S_OK, STG_E_ACCESSDENIED,
    STG_E_INVALIDFUNCTION, STG_E_INVALIDPOINTER, STG_E_READFAULT,
};
use windows::Win32::System::Com::{
    CoTaskMemAlloc, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE,
    STATFLAG, STATFLAG_NONAME, STATSTG, STGC, STGM, STGMEDIUM, STGMEDIUM_0, STGTY_STREAM,
    STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, ReleaseStgMedium};
use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
use windows::core::{Error, HRESULT, PWSTR, Result as WinResult, implement};

// FILEDESCRIPTOR flags (shlobj_core.h).
const FD_ATTRIBUTES: u32 = 0x0000_0004;
const FD_WRITESTIME: u32 = 0x0000_0020;
const FD_FILESIZE: u32 = 0x0000_0040;
const FD_PROGRESSUI: u32 = 0x0000_4000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

/// `sizeof(FILEDESCRIPTORW)`, packed (identical to the natural layout).
pub const FILEDESCRIPTOR_LEN: usize = 592;
const NAME_UNITS: usize = 260;
const DATA_S_SAMEFORMATETC: HRESULT = HRESULT(0x0004_0130);
const DATADIR_GET: u32 = 1;
const STREAM_CHUNK: usize = 64 * 1024;

/// Clipboard format ids used by the data object (all registered formats fit in 16 bits).
#[derive(Debug, Clone, Copy)]
pub struct ObjectFormats {
    pub file_descriptor: u16,
    pub file_contents: u16,
    pub preferred_drop_effect: u16,
    pub performed_drop_effect: u16,
    pub logical_performed_drop_effect: u16,
    pub paste_succeeded: u16,
    pub origin: u16,
}

/// Converts a Unix timestamp in milliseconds to a Windows FILETIME tick count (100 ns since 1601).
#[must_use]
pub fn unix_ms_to_filetime(ms: i64) -> u64 {
    const EPOCH_DIFF_MS: i64 = 11_644_473_600_000;
    u64::try_from(ms.saturating_add(EPOCH_DIFF_MS)).map_or(0, |v| v.saturating_mul(10_000))
}

/// Validates a transfer-relative path from a (remote, untrusted) peer and converts it to the
/// backslash form used in file descriptors. Rejects absolute paths, drive letters, `..`,
/// empty components and names that do not fit `cFileName`.
pub fn descriptor_name(rel_path: &str) -> Result<String, String> {
    let trimmed = rel_path.trim_matches('/');
    if trimmed.is_empty() {
        return Err("empty file name".into());
    }
    if rel_path.starts_with('/') || rel_path.starts_with('\\') {
        return Err(format!("absolute path rejected: {rel_path}"));
    }
    for comp in trimmed.split(['/', '\\']) {
        if comp.is_empty()
            || comp == "."
            || comp == ".."
            || comp.contains(':')
            || comp.contains('\0')
        {
            return Err(format!("unsafe path component in {rel_path}"));
        }
    }
    let name = trimmed.replace('/', "\\");
    if name.encode_utf16().count() >= NAME_UNITS {
        return Err(format!("path too long for a file descriptor: {rel_path}"));
    }
    Ok(name)
}

/// Builds a `FILEGROUPDESCRIPTORW` (`cItems` + `FILEDESCRIPTORW[n]`).
pub fn build_file_group_descriptor(files: &[FileOffer]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(4 + files.len() * FILEDESCRIPTOR_LEN);
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for f in files {
        let name = descriptor_name(&f.rel_path)?;
        let mut flags = FD_ATTRIBUTES | FD_WRITESTIME | FD_PROGRESSUI;
        let attrs = if f.is_dir {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            flags |= FD_FILESIZE;
            FILE_ATTRIBUTE_NORMAL
        };
        let size = if f.is_dir { 0 } else { f.size };
        let start = out.len();
        out.extend_from_slice(&flags.to_le_bytes()); // dwFlags
        out.extend_from_slice(&[0u8; 16]); // clsid
        out.extend_from_slice(&[0u8; 8]); // sizel
        out.extend_from_slice(&[0u8; 8]); // pointl
        out.extend_from_slice(&attrs.to_le_bytes()); // dwFileAttributes
        out.extend_from_slice(&[0u8; 8]); // ftCreationTime
        out.extend_from_slice(&[0u8; 8]); // ftLastAccessTime
        out.extend_from_slice(&unix_ms_to_filetime(f.mtime_unix_ms).to_le_bytes()); // ftLastWriteTime
        out.extend_from_slice(&((size >> 32) as u32).to_le_bytes()); // nFileSizeHigh
        out.extend_from_slice(&(size as u32).to_le_bytes()); // nFileSizeLow
        let mut units = [0u16; NAME_UNITS];
        for (d, s) in units.iter_mut().zip(name.encode_utf16()) {
            *d = s;
        }
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
        debug_assert_eq!(out.len() - start, FILEDESCRIPTOR_LEN);
    }
    Ok(out)
}

fn guard<T>(f: impl FnOnce() -> WinResult<T>) -> WinResult<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(Error::from(E_FAIL)))
}

fn guard_hr(f: impl FnOnce() -> HRESULT) -> HRESULT {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(E_FAIL)
}

/// Allocates a movable `HGLOBAL` holding `bytes`.
fn hglobal_from(bytes: &[u8]) -> WinResult<HGLOBAL> {
    // SAFETY: the block is `bytes.len()` (>= 1) bytes large, locked only while copying; the
    // caller (OLE) takes ownership through the returned STGMEDIUM.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let p = GlobalLock(h).cast::<u8>();
        if p.is_null() {
            return Err(Error::from_win32());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

fn hglobal_medium(bytes: &[u8]) -> WinResult<STGMEDIUM> {
    Ok(STGMEDIUM {
        tymed: TYMED_HGLOBAL.0 as u32,
        u: STGMEDIUM_0 {
            hGlobal: hglobal_from(bytes)?,
        },
        pUnkForRelease: ManuallyDrop::new(None),
    })
}

fn fmt_etc(cf: u16, tymed: i32, lindex: i32) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex,
        tymed: tymed as u32,
    }
}

/// What a `FORMATETC` request addresses.
enum Which {
    Descriptor,
    /// `None` = any index (`lindex == -1`, used by `QueryGetData`).
    Contents(Option<usize>),
    Hglobal(HglobalKind),
}

#[derive(Clone, Copy)]
enum HglobalKind {
    PreferredEffect,
    Origin,
}

struct Entry {
    leaf: String,
    size: u64,
    mtime: u64,
    is_dir: bool,
}

/// The virtual-files data object.
#[implement(IDataObject)]
pub struct VirtualFilesObject {
    ids: ObjectFormats,
    descriptor: Vec<u8>,
    entries: Vec<Entry>,
    provider: Arc<dyn VirtualFileProvider>,
    origin: Vec<u8>,
}

/// Creates the data object. `origin` is the payload of the `DeskBridge.Origin` marker.
pub fn create_virtual_files_object(
    files: &[FileOffer],
    provider: Arc<dyn VirtualFileProvider>,
    ids: ObjectFormats,
    origin: Vec<u8>,
) -> Result<IDataObject, String> {
    let descriptor = build_file_group_descriptor(files)?;
    let entries = files
        .iter()
        .map(|f| {
            let name = descriptor_name(&f.rel_path)?;
            let leaf = name.rsplit('\\').next().unwrap_or(&name).to_owned();
            Ok(Entry {
                leaf,
                size: if f.is_dir { 0 } else { f.size },
                mtime: unix_ms_to_filetime(f.mtime_unix_ms),
                is_dir: f.is_dir,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(VirtualFilesObject {
        ids,
        descriptor,
        entries,
        provider,
        origin,
    }
    .into())
}

impl VirtualFilesObject {
    fn classify(&self, fe: &FORMATETC) -> Result<Which, HRESULT> {
        if fe.dwAspect & DVASPECT_CONTENT.0 == 0 {
            return Err(DV_E_DVASPECT);
        }
        let cf = fe.cfFormat;
        let wants = |t: i32| fe.tymed & (t as u32) != 0;
        let ids = &self.ids;
        if cf == ids.file_descriptor {
            return if wants(TYMED_HGLOBAL.0) {
                Ok(Which::Descriptor)
            } else {
                Err(DV_E_TYMED)
            };
        }
        if cf == ids.file_contents {
            if !wants(TYMED_ISTREAM.0) {
                return Err(DV_E_TYMED);
            }
            return match usize::try_from(fe.lindex) {
                Ok(i) if i < self.entries.len() && !self.entries[i].is_dir => {
                    Ok(Which::Contents(Some(i)))
                }
                Ok(_) => Err(DV_E_LINDEX),
                Err(_) if fe.lindex == -1 => Ok(Which::Contents(None)),
                Err(_) => Err(DV_E_LINDEX),
            };
        }
        let kind = if cf == ids.preferred_drop_effect {
            HglobalKind::PreferredEffect
        } else if cf == ids.origin {
            HglobalKind::Origin
        } else {
            return Err(DV_E_FORMATETC);
        };
        if wants(TYMED_HGLOBAL.0) {
            Ok(Which::Hglobal(kind))
        } else {
            Err(DV_E_TYMED)
        }
    }

    /// Formats offered by `EnumFormatEtc`. `FileContents` is listed once per file with its own
    /// `lindex`: OLE's clipboard consumer wrapper only forwards `GetData` for FORMATETCs that
    /// were enumerated, and Explorer asks with a concrete index.
    fn all_formats(&self) -> Vec<FORMATETC> {
        let mut v = vec![fmt_etc(self.ids.file_descriptor, TYMED_HGLOBAL.0, -1)];
        for (i, e) in self.entries.iter().enumerate() {
            if !e.is_dir {
                v.push(fmt_etc(self.ids.file_contents, TYMED_ISTREAM.0, i as i32));
            }
        }
        v.push(fmt_etc(self.ids.preferred_drop_effect, TYMED_HGLOBAL.0, -1));
        v.push(fmt_etc(self.ids.origin, TYMED_HGLOBAL.0, -1));
        v
    }

    fn get_data(&self, fe: &FORMATETC) -> WinResult<STGMEDIUM> {
        match self.classify(fe).map_err(Error::from)? {
            Which::Descriptor => hglobal_medium(&self.descriptor),
            Which::Hglobal(HglobalKind::PreferredEffect) => {
                hglobal_medium(&DROPEFFECT_COPY.0.to_le_bytes())
            }
            Which::Hglobal(HglobalKind::Origin) => hglobal_medium(&self.origin),
            Which::Contents(None) => Err(Error::from(DV_E_LINDEX)),
            Which::Contents(Some(i)) => {
                let e = &self.entries[i];
                let stream: IStream = VirtualFileStream::new(
                    self.provider.clone(),
                    i,
                    e.size,
                    e.mtime,
                    e.leaf.clone(),
                )
                .into();
                Ok(STGMEDIUM {
                    tymed: TYMED_ISTREAM.0 as u32,
                    u: STGMEDIUM_0 {
                        pstm: ManuallyDrop::new(Some(stream)),
                    },
                    pUnkForRelease: ManuallyDrop::new(None),
                })
            }
        }
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for VirtualFilesObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
        // SAFETY: COM guarantees a valid FORMATETC pointer for the duration of the call.
        let fe = unsafe { pformatetcin.as_ref() }.ok_or_else(|| Error::from(E_INVALIDARG))?;
        guard(|| self.get_data(fe))
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> WinResult<()> {
        Err(Error::from(E_NOTIMPL))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: see GetData.
        let Some(fe) = (unsafe { pformatetc.as_ref() }) else {
            return E_INVALIDARG;
        };
        guard_hr(|| match self.classify(fe) {
            Ok(_) => S_OK,
            Err(hr) => hr,
        })
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        // SAFETY: the out pointer, when non-null, is writable by contract.
        if let Some(out) = unsafe { pformatetcout.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        pformatetc: *const FORMATETC,
        pmedium: *const STGMEDIUM,
        frelease: BOOL,
    ) -> WinResult<()> {
        // SAFETY: see GetData.
        let fe = unsafe { pformatetc.as_ref() }.ok_or_else(|| Error::from(E_INVALIDARG))?;
        let ids = &self.ids;
        let cf = fe.cfFormat;
        // Explorer reports the outcome of the operation; accept and ignore.
        if cf == ids.performed_drop_effect
            || cf == ids.logical_performed_drop_effect
            || cf == ids.paste_succeeded
            || cf == ids.preferred_drop_effect
        {
            if frelease.as_bool() && !pmedium.is_null() {
                // SAFETY: with fRelease=TRUE the callee owns the medium and must release it.
                unsafe { ReleaseStgMedium(pmedium.cast_mut()) };
            }
            return Ok(());
        }
        Err(Error::from(E_NOTIMPL))
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> WinResult<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET {
            return Err(Error::from(E_NOTIMPL));
        }
        // SAFETY: the format array outlives the call; the shell copies it.
        guard(|| unsafe { SHCreateStdEnumFmtEtc(&self.all_formats()) })
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Option<&IAdviseSink>,
    ) -> WinResult<u32> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> WinResult<()> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn EnumDAdvise(&self) -> WinResult<IEnumSTATDATA> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }
}

// ------------------------------------------------------------------------------------------
// Stream

struct StreamState {
    provider: Arc<dyn VirtualFileProvider>,
    index: usize,
    size: u64,
    /// Logical position as seen by the consumer.
    pos: u64,
    /// Underlying sequential reader and its own position (lazily opened).
    reader: Option<Box<dyn Read + Send>>,
    rpos: u64,
}

impl StreamState {
    /// Makes the reader match `pos`: opens it lazily, reopens on backward seeks and skips
    /// forward by reading and discarding.
    fn sync_reader(&mut self) -> io::Result<()> {
        if self.reader.is_some() && self.rpos > self.pos {
            self.reader = None;
        }
        if self.reader.is_none() {
            let provider = self.provider.clone();
            let index = self.index;
            let opened = catch_unwind(AssertUnwindSafe(|| provider.open(index)))
                .map_err(|_| io::Error::other("provider panicked in open()"))?
                .map_err(|e| io::Error::other(e.to_string()))?;
            self.reader = Some(opened);
            self.rpos = 0;
        }
        let mut sink = vec![0u8; STREAM_CHUNK];
        while self.rpos < self.pos {
            let want = ((self.pos - self.rpos) as usize).min(sink.len());
            let n = self.read_raw(&mut sink[..want])?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        Ok(())
    }

    fn read_raw(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(reader) = self.reader.as_mut() else {
            return Err(io::ErrorKind::NotConnected.into());
        };
        loop {
            match catch_unwind(AssertUnwindSafe(|| reader.read(buf))) {
                Ok(Ok(n)) => {
                    self.rpos += n as u64;
                    return Ok(n);
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(e)) => {
                    self.reader = None;
                    return Err(e);
                }
                Err(_) => {
                    self.reader = None;
                    return Err(io::Error::other("provider panicked in read()"));
                }
            }
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = (buf.len() as u64).min(self.size.saturating_sub(self.pos)) as usize;
        if want == 0 {
            return Ok(0);
        }
        self.sync_reader()?;
        // Fill the caller's buffer like a file stream does: a network source delivers short
        // reads, and consumers (Explorer) may treat `read < requested` as the end of the file.
        let mut filled = 0;
        while filled < want {
            let n = self.read_raw(&mut buf[filled..want])?;
            if n == 0 {
                // Declared size not delivered: report an error rather than silently truncating.
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            filled += n;
            self.pos += n as u64;
        }
        Ok(filled)
    }
}

/// Read-only `IStream` over a [`VirtualFileProvider`] file.
#[implement(IStream)]
struct VirtualFileStream {
    state: parking_lot::Mutex<StreamState>,
    mtime: u64,
    leaf: String,
}

impl VirtualFileStream {
    fn new(
        provider: Arc<dyn VirtualFileProvider>,
        index: usize,
        size: u64,
        mtime: u64,
        leaf: String,
    ) -> Self {
        Self {
            state: parking_lot::Mutex::new(StreamState {
                provider,
                index,
                size,
                pos: 0,
                reader: None,
                rpos: 0,
            }),
            mtime,
            leaf,
        }
    }
}

#[allow(non_snake_case)]
impl ISequentialStream_Impl for VirtualFileStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return STG_E_INVALIDPOINTER;
        }
        guard_hr(|| {
            // SAFETY: COM guarantees `pv` points to `cb` writable bytes.
            let buf = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
            let res = self.state.lock().read(buf);
            match res {
                Ok(n) => {
                    // SAFETY: optional out pointer, writable when non-null.
                    if let Some(out) = unsafe { pcbread.as_mut() } {
                        *out = n as u32;
                    }
                    if n == 0 && cb > 0 { S_FALSE } else { S_OK }
                }
                Err(e) => {
                    tracing::warn!("virtual file stream read failed: {e}");
                    // SAFETY: as above.
                    if let Some(out) = unsafe { pcbread.as_mut() } {
                        *out = 0;
                    }
                    STG_E_READFAULT
                }
            }
        })
    }

    fn Write(&self, _pv: *const core::ffi::c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

#[allow(non_snake_case)]
impl IStream_Impl for VirtualFileStream_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> WinResult<()> {
        let mut st = self.state.lock();
        let base = match dworigin {
            STREAM_SEEK_SET => 0i64,
            STREAM_SEEK_CUR => st.pos as i64,
            STREAM_SEEK_END => st.size as i64,
            _ => return Err(Error::from(STG_E_INVALIDFUNCTION)),
        };
        let target = base
            .checked_add(dlibmove)
            .filter(|&t| t >= 0)
            .ok_or_else(|| Error::from(STG_E_INVALIDFUNCTION))?;
        // Only the logical position changes; the reader is repositioned lazily on the next Read.
        st.pos = target as u64;
        // SAFETY: optional out pointer, writable when non-null.
        if let Some(out) = unsafe { plibnewposition.as_mut() } {
            *out = target as u64;
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> WinResult<()> {
        Err(Error::from(STG_E_ACCESSDENIED))
    }

    fn CopyTo(
        &self,
        pstm: Option<&IStream>,
        cb: u64,
        pcbread: *mut u64,
        pcbwritten: *mut u64,
    ) -> WinResult<()> {
        let dst = pstm.ok_or_else(|| Error::from(E_POINTER))?;
        let (mut total_r, mut total_w) = (0u64, 0u64);
        let mut buf = vec![0u8; STREAM_CHUNK];
        let mut result = Ok(());
        while total_r < cb {
            let want = ((cb - total_r) as usize).min(buf.len());
            let n = match self.state.lock().read(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => {
                    result = Err(Error::from(STG_E_READFAULT));
                    break;
                }
            };
            total_r += n as u64;
            let mut written = 0u32;
            // SAFETY: `buf[..n]` is valid for reads; `written` is a valid out slot.
            let hr = unsafe { dst.Write(buf.as_ptr().cast(), n as u32, Some(&mut written)) };
            total_w += u64::from(written);
            if hr.is_err() {
                result = Err(Error::from(hr));
                break;
            }
        }
        // SAFETY: optional out pointers, writable when non-null.
        unsafe {
            if let Some(o) = pcbread.as_mut() {
                *o = total_r;
            }
            if let Some(o) = pcbwritten.as_mut() {
                *o = total_w;
            }
        }
        result
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> WinResult<()> {
        Ok(())
    }

    fn Revert(&self) -> WinResult<()> {
        Ok(())
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> WinResult<()> {
        Err(Error::from(STG_E_INVALIDFUNCTION))
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> WinResult<()> {
        Err(Error::from(STG_E_INVALIDFUNCTION))
    }

    fn Stat(&self, pstatstg: *mut STATSTG, grfstatflag: &STATFLAG) -> WinResult<()> {
        // SAFETY: COM guarantees a writable STATSTG.
        let out = unsafe { pstatstg.as_mut() }.ok_or_else(|| Error::from(E_POINTER))?;
        let size = self.state.lock().size;
        let mut name = PWSTR::null();
        if *grfstatflag != STATFLAG_NONAME {
            let wide: Vec<u16> = self.leaf.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: the buffer is `wide.len()` u16s; ownership passes to the caller (CoTaskMemFree).
            unsafe {
                let p = CoTaskMemAlloc(wide.len() * 2).cast::<u16>();
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                    name = PWSTR(p);
                }
            }
        }
        let ft = FILETIME {
            dwLowDateTime: self.mtime as u32,
            dwHighDateTime: (self.mtime >> 32) as u32,
        };
        *out = STATSTG {
            pwcsName: name,
            r#type: STGTY_STREAM.0 as u32,
            cbSize: size,
            mtime: ft,
            ctime: FILETIME::default(),
            atime: FILETIME::default(),
            grfMode: STGM(0), // STGM_READ
            grfLocksSupported: 0,
            clsid: windows::core::GUID::zeroed(),
            grfStateBits: 0,
            reserved: 0,
        };
        Ok(())
    }

    fn Clone(&self) -> WinResult<IStream> {
        let st = self.state.lock();
        let copy = VirtualFileStream::new(
            st.provider.clone(),
            st.index,
            st.size,
            self.mtime,
            self.leaf.clone(),
        );
        copy.state.lock().pos = st.pos;
        Ok(copy.into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::format;
    use super::*;

    fn offer(path: &str, size: u64, dir: bool) -> FileOffer {
        FileOffer {
            rel_path: path.into(),
            size,
            mtime_unix_ms: 1_700_000_000_000,
            is_dir: dir,
        }
    }

    fn u32_at(d: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
    }

    #[test]
    fn filetime_conversion() {
        assert_eq!(unix_ms_to_filetime(0), 116_444_736_000_000_000);
        assert_eq!(
            unix_ms_to_filetime(1_000),
            116_444_736_000_000_000 + 10_000_000
        );
        assert_eq!(unix_ms_to_filetime(i64::MIN), 0);
    }

    #[test]
    fn group_descriptor_layout() {
        let files = [
            offer("Photos", 0, true),
            offer("Photos/Sub Dir/a ü.jpg", 5_000_000_123, false),
            offer("b.txt", 7, false),
        ];
        let d = build_file_group_descriptor(&files).unwrap();
        assert_eq!(d.len(), 4 + 3 * FILEDESCRIPTOR_LEN);
        assert_eq!(u32_at(&d, 0), 3);

        let e = |i: usize| 4 + i * FILEDESCRIPTOR_LEN;
        // Directory entry.
        assert_eq!(
            u32_at(&d, e(0)),
            FD_ATTRIBUTES | FD_WRITESTIME | FD_PROGRESSUI
        );
        assert_eq!(u32_at(&d, e(0) + 36), FILE_ATTRIBUTE_DIRECTORY);
        // File entry: size split into high/low, write time, nested path with backslashes.
        assert_eq!(
            u32_at(&d, e(1)),
            FD_ATTRIBUTES | FD_WRITESTIME | FD_PROGRESSUI | FD_FILESIZE
        );
        assert_eq!(u32_at(&d, e(1) + 36), FILE_ATTRIBUTE_NORMAL);
        let ft = u64::from(u32_at(&d, e(1) + 56)) | (u64::from(u32_at(&d, e(1) + 60)) << 32);
        assert_eq!(ft, unix_ms_to_filetime(1_700_000_000_000));
        assert_eq!(
            u64::from(u32_at(&d, e(1) + 64)) << 32 | u64::from(u32_at(&d, e(1) + 68)),
            5_000_000_123
        );
        let name: Vec<u16> = d[e(1) + 72..e(1) + 72 + 520]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(format::utf16_to_string(&name), "Photos\\Sub Dir\\a ü.jpg");
        assert_eq!(u32_at(&d, e(2) + 68), 7);
    }

    #[test]
    fn descriptor_name_validation() {
        assert_eq!(descriptor_name("a/b/c.txt").unwrap(), "a\\b\\c.txt");
        assert_eq!(descriptor_name("dir/").unwrap(), "dir");
        for bad in [
            "", "/", "/abs", "\\abs", "../x", "a/../b", "a//b", "C:/x", "a/./b", "a/b:c",
        ] {
            assert!(descriptor_name(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(descriptor_name(&"x".repeat(260)).is_err());
        assert!(descriptor_name(&"x".repeat(259)).is_ok());
        assert!(build_file_group_descriptor(&[offer("../evil", 1, false)]).is_err());
    }

    #[test]
    fn stream_state_read_seek_and_eof() {
        struct P;
        impl VirtualFileProvider for P {
            fn open(&self, index: usize) -> deskbridge_platform::Result<Box<dyn Read + Send>> {
                assert_eq!(index, 3);
                Ok(Box::new(io::Cursor::new((0..100u8).collect::<Vec<_>>())))
            }
        }
        let mut st = StreamState {
            provider: Arc::new(P),
            index: 3,
            size: 100,
            pos: 0,
            reader: None,
            rpos: 0,
        };
        let mut b = [0u8; 10];
        assert_eq!(st.read(&mut b).unwrap(), 10);
        assert_eq!(b[9], 9);
        st.pos = 50; // forward seek: skipped lazily
        assert_eq!(st.read(&mut b).unwrap(), 10);
        assert_eq!(b[0], 50);
        st.pos = 5; // backward seek: reopen
        assert_eq!(st.read(&mut b).unwrap(), 10);
        assert_eq!(b[0], 5);
        st.pos = 95;
        let mut big = [0u8; 50];
        assert_eq!(st.read(&mut big).unwrap(), 5); // capped at declared size
        assert_eq!(st.read(&mut big).unwrap(), 0); // EOF
    }

    #[test]
    fn stream_state_short_source_is_an_error() {
        struct P;
        impl VirtualFileProvider for P {
            fn open(&self, _index: usize) -> deskbridge_platform::Result<Box<dyn Read + Send>> {
                Ok(Box::new(io::Cursor::new(vec![1u8, 2, 3])))
            }
        }
        let mut st = StreamState {
            provider: Arc::new(P),
            index: 0,
            size: 10,
            pos: 0,
            reader: None,
            rpos: 0,
        };
        let mut b = [0u8; 8];
        // The buffer is filled completely, so running out of data is an error right away.
        assert!(st.read(&mut b).is_err());
    }
}
