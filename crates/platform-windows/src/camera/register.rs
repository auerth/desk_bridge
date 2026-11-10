//! Registration of "Desk Bridge Kamera": the COM class under HKLM and the virtual camera device
//! (`MFCreateVirtualCamera`, Windows 11 build 22000+). Both need administrator rights.
//!
//! `MFCreateVirtualCamera` is looked up at run time: on Windows 10 `mfsensorgroup.dll` exists
//! but lacks the export, and a static import would keep the agent from starting there.

use deskbridge_camera::{VCAM_CLSID, VCAM_FRIENDLY_NAME};
use deskbridge_platform::{PlatformError, Result};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MF_VERSION, MFSTARTUP_FULL, MFStartup, MFVirtualCameraAccess,
    MFVirtualCameraAccess_AllUsers, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime,
    MFVirtualCameraLifetime_Session, MFVirtualCameraLifetime_System, MFVirtualCameraType,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::core::{GUID, HRESULT, HSTRING, Interface, PCWSTR, s, w};

type CreateFn = unsafe extern "system" fn(
    MFVirtualCameraType,
    MFVirtualCameraLifetime,
    MFVirtualCameraAccess,
    PCWSTR,
    PCWSTR,
    *const GUID,
    u32,
    *mut *mut core::ffi::c_void,
) -> HRESULT;

fn create_fn() -> Option<CreateFn> {
    // SAFETY: loading a system DLL and looking up an export with the documented signature.
    unsafe {
        let lib = LoadLibraryW(w!("mfsensorgroup.dll")).ok()?;
        let p = GetProcAddress(lib, s!("MFCreateVirtualCamera"))?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            CreateFn,
        >(p))
    }
}

/// Windows offers virtual cameras (Windows 11).
#[must_use]
pub fn os_supported() -> bool {
    create_fn().is_some()
}

fn clsid_string() -> String {
    format!("{{{:?}}}", GUID::from_u128(VCAM_CLSID))
}

fn class_key() -> HSTRING {
    HSTRING::from(format!("SOFTWARE\\Classes\\CLSID\\{}", clsid_string()))
}

fn os_err(what: &str, code: windows::Win32::Foundation::WIN32_ERROR) -> PlatformError {
    PlatformError::other(format!(
        "{what}: {:?}",
        windows::core::Error::from(code.to_hresult())
    ))
}

fn set_string(key: HKEY, name: PCWSTR, value: &str) -> Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the byte view covers the NUL-terminated UTF-16 buffer.
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    // SAFETY: valid key and buffer.
    let r = unsafe { RegSetValueExW(key, name, 0, REG_SZ, Some(bytes)) };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(os_err("RegSetValueExW", r))
    }
}

fn create_key(path: &HSTRING) -> Result<HKEY> {
    let mut key = HKEY::default();
    // SAFETY: plain registry call with out pointer to a local.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            path,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE | KEY_READ,
            None,
            &mut key,
            None,
        )
    };
    if r == ERROR_SUCCESS {
        Ok(key)
    } else {
        Err(os_err("RegCreateKeyExW", r))
    }
}

/// Path of the DLL the COM class points at, if it is registered.
#[must_use]
pub fn registered_dll() -> Option<PathBuf> {
    let key = HSTRING::from(format!("{}\\InprocServer32", class_key()));
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its length in bytes are passed.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &key,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1);
    Some(PathBuf::from(String::from_utf16_lossy(
        &buf[..n.min(buf.len())],
    )))
}

/// Registers the COM class of the media source (HKLM, `ThreadingModel=Both`).
pub fn register_com(dll: &Path) -> Result<()> {
    let key = create_key(&class_key())?;
    let r = set_string(key, PCWSTR::null(), VCAM_FRIENDLY_NAME);
    // SAFETY: we own the key.
    unsafe {
        let _ = RegCloseKey(key);
    }
    r?;
    let key = create_key(&HSTRING::from(format!("{}\\InprocServer32", class_key())))?;
    let r = set_string(key, PCWSTR::null(), &dll.to_string_lossy())
        .and_then(|()| set_string(key, w!("ThreadingModel"), "Both"));
    // SAFETY: we own the key.
    unsafe {
        let _ = RegCloseKey(key);
    }
    r
}

pub fn unregister_com() -> Result<()> {
    // SAFETY: plain registry call.
    let r = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &class_key()) };
    if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(os_err("RegDeleteTreeW", r))
    }
}

/// Handle to the virtual camera device.
pub struct VirtualCameraDevice(IMFVirtualCamera);

impl VirtualCameraDevice {
    /// `system`: the machine-wide camera that stays registered after this process (needs
    /// administrator rights); otherwise one for this user that lives as long as the handle
    /// (tests).
    pub fn open(system: bool) -> Result<Self> {
        let f = create_fn().ok_or(PlatformError::Unsupported(
            "virtual cameras (needs Windows 11)",
        ))?;
        let name = HSTRING::from(VCAM_FRIENDLY_NAME);
        let id = HSTRING::from(clsid_string());
        let (lifetime, access) = if system {
            (
                MFVirtualCameraLifetime_System,
                MFVirtualCameraAccess_AllUsers,
            )
        } else {
            (
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
            )
        };
        // SAFETY: MFStartup may be called repeatedly; the function has the documented
        // signature and fills the out pointer on success.
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)
                .map_err(|e| PlatformError::other(format!("MFStartup: {e}")))?;
            let mut out = std::ptr::null_mut();
            f(
                MFVirtualCameraType_SoftwareCameraSource,
                lifetime,
                access,
                PCWSTR(name.as_ptr()),
                PCWSTR(id.as_ptr()),
                std::ptr::null(),
                0,
                &mut out,
            )
            .ok()
            .map_err(|e| PlatformError::other(format!("MFCreateVirtualCamera: {e}")))?;
            Ok(Self(IMFVirtualCamera::from_raw(out)))
        }
    }

    /// Makes the camera visible to apps.
    pub fn start(&self) -> Result<()> {
        // SAFETY: plain COM call.
        unsafe { self.0.Start(None) }
            .map_err(|e| PlatformError::other(format!("IMFVirtualCamera::Start: {e}")))
    }

    /// Hides the camera from apps (it stays registered).
    pub fn stop(&self) -> Result<()> {
        // SAFETY: plain COM call.
        unsafe { self.0.Stop() }
            .map_err(|e| PlatformError::other(format!("IMFVirtualCamera::Stop: {e}")))
    }

    /// Unregisters the camera for good.
    pub fn remove(&self) -> Result<()> {
        // SAFETY: plain COM call.
        unsafe { self.0.Remove() }
            .map_err(|e| PlatformError::other(format!("IMFVirtualCamera::Remove: {e}")))
    }

    #[must_use]
    pub fn raw(&self) -> &IMFVirtualCamera {
        &self.0
    }
}

impl Drop for VirtualCameraDevice {
    fn drop(&mut self) {
        // SAFETY: plain COM call; releases the handle, the registration stays (system lifetime).
        let _ = unsafe { self.0.Shutdown() };
    }
}

/// Registers the COM class for `dll` and the machine-wide camera, enabled. Idempotent; used by
/// the installer (`deskbridge-agent --register-camera`) and by an elevated agent at start-up.
pub fn install(dll: &Path) -> Result<()> {
    if !os_supported() {
        return Err(PlatformError::Unsupported(
            "virtual cameras (needs Windows 11)",
        ));
    }
    if !dll.is_file() {
        return Err(PlatformError::other(format!("{} not found", dll.display())));
    }
    if registered_dll().as_deref() != Some(dll) {
        register_com(dll)?;
    }
    let cam = VirtualCameraDevice::open(true)?;
    cam.start()
}

/// Removes the camera and the COM class (uninstall).
pub fn uninstall() -> Result<()> {
    if os_supported()
        && let Ok(cam) = VirtualCameraDevice::open(true)
    {
        let _ = cam.remove();
    }
    unregister_com()
}
