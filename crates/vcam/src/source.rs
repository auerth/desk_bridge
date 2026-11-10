//! The media source: one live video stream, no controls (`IKsControl` answers "not found").

use crate::ObjectCount;
use crate::log::vlog;
use crate::stream::{self, StreamCore};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::{GUID, HRESULT, IUnknown, Interface, PROPVARIANT, implement};

struct State {
    shutdown: bool,
}

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl)]
pub(crate) struct MediaSource {
    state: Mutex<State>,
    queue: IMFMediaEventQueue,
    attrs: IMFAttributes,
    pd: IMFPresentationDescriptor,
    stream: Arc<StreamCore>,
    stream_obj: IMFMediaStream2,
    _count: ObjectCount,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl MediaSource {
    /// Creates the source; the attributes the Frame Server put on the activator are copied.
    pub(crate) fn create(activate_attrs: &IMFAttributes) -> windows::core::Result<IMFMediaSource> {
        // SAFETY: plain Media Foundation constructors.
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 8)?;
            let attrs = attrs.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            activate_attrs.CopyAllItems(&attrs)?;
            let (core, stream_obj, sd) = stream::create()?;
            let pd = MFCreatePresentationDescriptor(Some(&[Some(sd)]))?;
            pd.SelectStream(0)?;
            let src: IMFMediaSourceEx = Self {
                state: Mutex::new(State { shutdown: false }),
                queue: MFCreateEventQueue()?,
                attrs,
                pd,
                stream: core.clone(),
                stream_obj,
                _count: ObjectCount::new(),
            }
            .into();
            let src: IMFMediaSource = src.cast()?;
            core.set_source(Some(src.clone()));
            Ok(src)
        }
    }

    fn check(&self) -> windows::core::Result<()> {
        if lock(&self.state).shutdown {
            Err(MF_E_SHUTDOWN.into())
        } else {
            Ok(())
        }
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows::core::Result<IMFMediaEvent> {
        let q = self.queue.clone();
        // SAFETY: plain COM call, outside every lock (it may block).
        unsafe { q.GetEvent(flags.0) }
    }

    fn BeginGetEvent(
        &self,
        cb: Option<&IMFAsyncCallback>,
        state: Option<&IUnknown>,
    ) -> windows::core::Result<()> {
        // SAFETY: plain COM call.
        unsafe { self.queue.BeginGetEvent(cb, state) }
    }

    fn EndGetEvent(&self, result: Option<&IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: plain COM call.
        unsafe { self.queue.EndGetEvent(result) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        ext: *const GUID,
        hr: HRESULT,
        v: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        // SAFETY: plain COM call with the caller's pointers.
        unsafe { self.queue.QueueEventParamVar(met, ext, hr, v) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> windows::core::Result<u32> {
        self.check()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        self.check()?;
        // SAFETY: plain COM call.
        unsafe { self.pd.Clone() }
    }

    fn Start(
        &self,
        pd: Option<&IMFPresentationDescriptor>,
        time_format: *const GUID,
        pos: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        self.check()?;
        let pd = pd.ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;
        // SAFETY: the caller's pointers are valid for the call; plain COM calls.
        unsafe {
            if !time_format.is_null() && *time_format != GUID::zeroed() {
                return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
            }
            let empty = PROPVARIANT::default();
            let pos = if pos.is_null() { &raw const empty } else { pos };
            for i in 0..pd.GetStreamDescriptorCount()? {
                let mut selected = windows::Win32::Foundation::BOOL::default();
                let mut sd = None;
                pd.GetStreamDescriptorByIndex(i, &mut selected, &mut sd)?;
                let Some(sd) = sd else { continue };
                if sd.GetStreamIdentifier()? != 0 {
                    return Err(MF_E_INVALIDSTREAMNUMBER.into());
                }
                if selected.as_bool() {
                    let ev = if self.stream.is_active() {
                        MEUpdatedStream
                    } else {
                        MENewStream
                    };
                    self.queue.QueueEventParamUnk(
                        ev.0 as u32,
                        &GUID::zeroed(),
                        S_OK,
                        &self.stream_obj,
                    )?;
                    self.stream.start(&sd, pos)?;
                } else if self.stream.is_active() {
                    self.stream.stop()?;
                }
            }
            self.queue
                .QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, pos)
        }
    }

    fn Stop(&self) -> windows::core::Result<()> {
        self.check()?;
        if self.stream.is_active() {
            self.stream.stop()?;
        }
        // SAFETY: plain COM call.
        unsafe {
            self.queue.QueueEventParamVar(
                MESourceStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )
        }
    }

    fn Pause(&self) -> windows::core::Result<()> {
        self.check()?;
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> windows::core::Result<()> {
        {
            let mut st = lock(&self.state);
            if st.shutdown {
                return Ok(());
            }
            st.shutdown = true;
        }
        self.stream.shutdown();
        // SAFETY: plain COM call.
        let _ = unsafe { self.queue.Shutdown() };
        vlog!("media source shut down");
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> windows::core::Result<IMFAttributes> {
        self.check()?;
        Ok(self.attrs.clone())
    }

    fn GetStreamAttributes(&self, id: u32) -> windows::core::Result<IMFAttributes> {
        self.check()?;
        if id != 0 {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        Ok(self.stream.attributes())
    }

    fn SetD3DManager(&self, _manager: Option<&IUnknown>) -> windows::core::Result<()> {
        // System-memory samples only.
        self.check()
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(
        &self,
        _service: *const GUID,
        _riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if !ppv.is_null() {
            // SAFETY: checked for null.
            unsafe { *ppv = std::ptr::null_mut() };
        }
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

fn not_found() -> windows::core::Error {
    HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into()
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }

    fn KsMethod(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }

    fn KsEvent(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }
}
