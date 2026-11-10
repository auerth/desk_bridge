//! `IMFActivate`: what the Frame Server creates from the registered CLSID.

use crate::ObjectCount;
use crate::attrs::forward_attributes;
use crate::log::vlog;
use crate::source::MediaSource;
use std::sync::Mutex;
use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFMediaSource, MFCreateAttributes,
};
use windows::core::{GUID, Interface, implement};

#[implement(IMFActivate)]
pub(crate) struct Activator {
    attrs: IMFAttributes,
    source: Mutex<Option<IMFMediaSource>>,
    _count: ObjectCount,
}

impl Activator {
    pub(crate) fn new() -> windows::core::Result<Self> {
        let mut attrs = None;
        // SAFETY: out pointer to a local.
        unsafe { MFCreateAttributes(&mut attrs, 4)? };
        Ok(Self {
            attrs: attrs.ok_or_else(|| windows::core::Error::from(E_POINTER))?,
            source: Mutex::new(None),
            _count: ObjectCount::new(),
        })
    }
}

forward_attributes!(Activator_Impl, attrs);

impl IMFActivate_Impl for Activator_Impl {
    fn ActivateObject(
        &self,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        let mut g = self.source.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            let src = MediaSource::create(&self.attrs)
                .inspect_err(|e| vlog!("media source creation failed: {e}"))?;
            vlog!("media source created");
            *g = Some(src);
        }
        let src = g.as_ref().expect("just set");
        // SAFETY: COM contract.
        unsafe { src.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> windows::core::Result<()> {
        if let Some(s) = self.source.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // SAFETY: plain COM call.
            let _ = unsafe { s.Shutdown() };
        }
        Ok(())
    }

    fn DetachObject(&self) -> windows::core::Result<()> {
        self.source.lock().unwrap_or_else(|e| e.into_inner()).take();
        Ok(())
    }
}
