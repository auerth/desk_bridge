//! The named file mapping both sides open (Windows).

use crate::{MAPPING_NAME, Ring, TOTAL_BYTES};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::core::{HSTRING, w};

/// Who may open the mapping: SYSTEM, administrators, `LOCAL SERVICE` (the Frame Server that
/// hosts the virtual camera) and interactively signed-in users (the agent runs as the user).
const SDDL: windows::core::PCWSTR = w!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GA;;;IU)");

pub struct SharedCamera {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    ring: Ring,
}

// SAFETY: the handle and view are process-wide; `Ring` is Send + Sync.
unsafe impl Send for SharedCamera {}
unsafe impl Sync for SharedCamera {}

impl SharedCamera {
    /// Opens the mapping, creating it when `create` is set and it does not exist yet. Creating a
    /// `Global\` object needs `SeCreateGlobalPrivilege` (services, elevated processes); a plain
    /// user process can only open one the other side created.
    pub fn open(create: bool) -> windows::core::Result<Self> {
        let name = HSTRING::from(MAPPING_NAME);
        // SAFETY: plain Win32 calls; the security descriptor is freed after use, the handle and
        // view are owned by the returned value.
        unsafe {
            let handle = if create {
                let mut sd = PSECURITY_DESCRIPTOR::default();
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    SDDL,
                    SDDL_REVISION_1,
                    &mut sd,
                    None,
                )?;
                let sa = SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd.0,
                    bInheritHandle: false.into(),
                };
                let r = CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    Some(&sa),
                    PAGE_READWRITE,
                    0,
                    TOTAL_BYTES as u32,
                    &name,
                );
                let _ = LocalFree(HLOCAL(sd.0));
                r?
            } else {
                OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &name)?
            };
            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, TOTAL_BYTES);
            if view.Value.is_null() {
                let e = windows::core::Error::from_win32();
                let _ = CloseHandle(handle);
                return Err(e);
            }
            let ring = Ring::new(view.Value.cast());
            Ok(Self { handle, view, ring })
        }
    }

    /// Opens the mapping or, if that fails, creates it.
    pub fn open_or_create() -> windows::core::Result<Self> {
        Self::open(false).or_else(|_| Self::open(true))
    }

    #[must_use]
    pub fn ring(&self) -> &Ring {
        &self.ring
    }
}

impl Drop for SharedCamera {
    fn drop(&mut self) {
        // SAFETY: we own the view and the handle.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}
