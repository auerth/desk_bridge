//! "Desk Bridge Kamera": a Media Foundation media source for `MFCreateVirtualCamera`.
//!
//! Windows 11 (build 22000+) lets an app register a *virtual camera* without a driver: the
//! camera is a COM class (this DLL, registered under HKLM) that the Windows Camera Frame Server
//! loads and asks for pictures; Teams, Zoom, the browser and the Camera app see it like any
//! other webcam. The pictures come from the agent through the shared memory described in
//! `deskbridge-camera`.
//!
//! Objects (all free-threaded, the Frame Server calls from its MTA thread pool):
//! * [`factory::ClassFactory`] for [`deskbridge_camera::VCAM_CLSID`] creates
//! * [`activate::Activator`] (`IMFActivate`), which creates
//! * [`source::MediaSource`] (`IMFMediaSourceEx`, `IKsControl`, `IMFGetService`) with one
//! * [`stream::MediaStream`] (`IMFMediaStream2`), whose worker thread delivers one NV12 sample
//!   per `RequestSample`, paced to the frame rate the app chose.

#![cfg(windows)]
#![allow(non_snake_case)]

mod activate;
mod attrs;
mod factory;
mod log;
mod source;
mod stream;

use std::sync::atomic::{AtomicI32, Ordering};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::IClassFactory;
use windows::core::{GUID, HRESULT, Interface};

/// Live COM objects and server locks (`DllCanUnloadNow`).
pub(crate) static OBJECTS: AtomicI32 = AtomicI32::new(0);
pub(crate) static LOCKS: AtomicI32 = AtomicI32::new(0);

pub(crate) const CLSID: GUID = GUID::from_u128(deskbridge_camera::VCAM_CLSID);

/// Counts one live object for `DllCanUnloadNow`.
pub(crate) struct ObjectCount;

impl ObjectCount {
    pub(crate) fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for ObjectCount {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// # Safety
/// COM entry point: `rclsid`, `riid` and `ppv` are valid pointers from the COM runtime.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() || riid.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked for null above.
    unsafe {
        *ppv = std::ptr::null_mut();
        if *rclsid != CLSID {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let f: IClassFactory = factory::ClassFactory::new().into();
        f.query(riid, ppv)
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 && LOCKS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}
