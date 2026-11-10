//! Class factory for the activator.

use crate::{LOCKS, ObjectCount, activate::Activator};
use std::sync::atomic::Ordering;
use windows::Win32::Foundation::{BOOL, CLASS_E_NOAGGREGATION, E_POINTER};
use windows::Win32::Media::MediaFoundation::IMFActivate;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::{GUID, IUnknown, Interface, implement};

#[implement(IClassFactory)]
pub(crate) struct ClassFactory {
    _count: ObjectCount,
}

impl ClassFactory {
    pub(crate) fn new() -> Self {
        Self {
            _count: ObjectCount::new(),
        }
    }
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Option<&IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: checked for null.
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let a: IMFActivate = Activator::new()?.into();
        // SAFETY: COM contract of CreateInstance.
        unsafe { a.query(riid, ppv).ok() }
    }

    fn LockServer(&self, lock: BOOL) -> windows::core::Result<()> {
        if lock.as_bool() {
            LOCKS.fetch_add(1, Ordering::SeqCst);
        } else {
            LOCKS.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
