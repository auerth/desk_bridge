//! The one video stream of the camera: NV12 at the size the app chose, paced to its frame rate.

use crate::ObjectCount;
use crate::log::{self, vlog};
use deskbridge_camera::{OFFERED_FPS, OFFERED_SIZES, ReaderRequest, WriterState, nv12, tick_ms};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{E_POINTER, S_OK};
use windows::Win32::Media::KernelStreaming::PINNAME_VIDEO_CAPTURE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::{GUID, IUnknown, Interface, PROPVARIANT, implement};

/// A COM pointer handed to the worker thread. Media Foundation objects are free-threaded.
struct Agile<T>(T);
// SAFETY: see above; the objects passed this way (event queue, sample tokens) are MTA objects.
unsafe impl<T> Send for Agile<T> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Format {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
}

impl Format {
    fn interval(&self) -> Duration {
        let num = u64::from(self.fps_num.max(1));
        let den = u64::from(self.fps_den.max(1));
        Duration::from_nanos(1_000_000_000 * den / num)
            .clamp(Duration::from_millis(5), Duration::from_secs(1))
    }

    /// 100 ns units.
    fn duration_hns(&self) -> i64 {
        (self.interval().as_nanos() / 100) as i64
    }

    fn fps(&self) -> u32 {
        (self.fps_num / self.fps_den.max(1)).max(1)
    }
}

impl Default for Format {
    fn default() -> Self {
        Self {
            width: OFFERED_SIZES[0].0,
            height: OFFERED_SIZES[0].1,
            fps_num: OFFERED_FPS,
            fps_den: 1,
        }
    }
}

#[derive(Default)]
struct Work {
    tokens: VecDeque<Option<Agile<IUnknown>>>,
    run: bool,
    paused: bool,
    format: Format,
}

type WorkCell = Arc<(Mutex<Work>, Condvar)>;

struct State {
    source: Option<IMFMediaSource>,
    active: bool,
    stream_state: MF_STREAM_STATE,
    shutdown: bool,
    worker: Option<std::thread::JoinHandle<()>>,
}

pub(crate) struct StreamCore {
    queue: IMFMediaEventQueue,
    sd: IMFStreamDescriptor,
    attrs: IMFAttributes,
    state: Mutex<State>,
    work: WorkCell,
}

// SAFETY: the COM members are free-threaded Media Foundation objects; mutable state is behind
// mutexes.
unsafe impl Send for StreamCore {}
unsafe impl Sync for StreamCore {}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn media_type(w: u32, h: u32) -> windows::core::Result<IMFMediaType> {
    let size = nv12::size(w, h) as u32;
    // SAFETY: plain attribute setters on a new media type.
    unsafe {
        let mt = MFCreateMediaType()?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(OFFERED_FPS) << 32) | 1)?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        mt.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        mt.SetUINT32(&MF_MT_SAMPLE_SIZE, size)?;
        mt.SetUINT32(&MF_MT_DEFAULT_STRIDE, w)?;
        mt.SetUINT32(&MF_MT_AVG_BITRATE, size.saturating_mul(8 * OFFERED_FPS))?;
        mt.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        mt.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(mt)
    }
}

fn set_stream_attributes(a: &IMFAttributes) -> windows::core::Result<()> {
    // SAFETY: plain attribute setters.
    unsafe {
        a.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
        a.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
        a.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
        a.SetUINT32(
            &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
            MFFrameSourceTypes_Color.0 as u32,
        )?;
    }
    Ok(())
}

fn format_of(mt: &IMFMediaType) -> Format {
    // SAFETY: plain attribute getters.
    unsafe {
        let mut f = Format::default();
        if let Ok(v) = mt.GetUINT64(&MF_MT_FRAME_SIZE) {
            f.width = (v >> 32) as u32;
            f.height = v as u32;
        }
        if let Ok(v) = mt.GetUINT64(&MF_MT_FRAME_RATE) {
            f.fps_num = ((v >> 32) as u32).max(1);
            f.fps_den = (v as u32).max(1);
        }
        f
    }
}

/// Creates the stream: its core (shared with the source), its COM object and its descriptor.
pub(crate) fn create()
-> windows::core::Result<(Arc<StreamCore>, IMFMediaStream2, IMFStreamDescriptor)> {
    // SAFETY: plain Media Foundation constructors.
    unsafe {
        let types = OFFERED_SIZES
            .iter()
            .map(|&(w, h)| media_type(w, h).map(Some))
            .collect::<windows::core::Result<Vec<_>>>()?;
        let sd = MFCreateStreamDescriptor(0, &types)?;
        sd.GetMediaTypeHandler()?
            .SetCurrentMediaType(types[0].as_ref())?;
        set_stream_attributes(&sd.cast()?)?;
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 4)?;
        let attrs = attrs.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        set_stream_attributes(&attrs)?;
        let core = Arc::new(StreamCore {
            queue: MFCreateEventQueue()?,
            sd: sd.clone(),
            attrs,
            state: Mutex::new(State {
                source: None,
                active: false,
                stream_state: MF_STREAM_STATE_STOPPED,
                shutdown: false,
                worker: None,
            }),
            work: Arc::default(),
        });
        let obj: IMFMediaStream2 = MediaStream {
            core: core.clone(),
            _count: ObjectCount::new(),
        }
        .into();
        Ok((core, obj, sd))
    }
}

impl StreamCore {
    pub(crate) fn set_source(&self, s: Option<IMFMediaSource>) {
        lock(&self.state).source = s;
    }

    pub(crate) fn attributes(&self) -> IMFAttributes {
        self.attrs.clone()
    }

    pub(crate) fn is_active(&self) -> bool {
        lock(&self.state).active
    }

    pub(crate) fn start(
        &self,
        sd: &IMFStreamDescriptor,
        pos: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        // SAFETY: plain COM calls.
        let format = unsafe {
            sd.GetMediaTypeHandler()
                .and_then(|h| h.GetCurrentMediaType())
                .map(|mt| format_of(&mt))
                .unwrap_or_default()
        };
        {
            let mut st = lock(&self.state);
            if st.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            st.active = true;
            st.stream_state = MF_STREAM_STATE_RUNNING;
            {
                let (m, cv) = &*self.work;
                let mut w = lock(m);
                w.format = format;
                w.run = true;
                w.paused = false;
                w.tokens.clear();
                cv.notify_all();
            }
            if st.worker.is_none() {
                let work = self.work.clone();
                let queue = Agile(self.queue.clone());
                st.worker = std::thread::Builder::new()
                    .name("vcam-stream".into())
                    .spawn(move || worker(&work, &queue))
                    .ok();
            }
        }
        vlog!(
            "stream started {}x{} @ {}/{} fps",
            format.width,
            format.height,
            format.fps_num,
            format.fps_den
        );
        let empty = PROPVARIANT::default();
        let pos = if pos.is_null() { &raw const empty } else { pos };
        // SAFETY: `pos` is valid for the call.
        unsafe {
            self.queue
                .QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, pos)
        }
    }

    fn stop_worker(&self) {
        let handle = {
            let mut st = lock(&self.state);
            st.active = false;
            st.stream_state = MF_STREAM_STATE_STOPPED;
            let (m, cv) = &*self.work;
            let mut w = lock(m);
            w.run = false;
            w.tokens.clear();
            cv.notify_all();
            st.worker.take()
        };
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    pub(crate) fn stop(&self) -> windows::core::Result<()> {
        self.stop_worker();
        vlog!("stream stopped");
        // SAFETY: plain COM call.
        unsafe {
            self.queue.QueueEventParamVar(
                MEStreamStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )
        }
    }

    pub(crate) fn shutdown(&self) {
        self.stop_worker();
        let mut st = lock(&self.state);
        st.shutdown = true;
        st.source = None;
        // SAFETY: plain COM call.
        let _ = unsafe { self.queue.Shutdown() };
    }
}

/// Delivers one sample per request, at most one per frame interval.
fn worker(work: &WorkCell, queue: &Agile<IMFMediaEventQueue>) {
    // SAFETY: balanced with CoUninitialize below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut producer = Producer::default();
    let mut next_due = Instant::now();
    let (m, cv) = &**work;
    loop {
        let (token, format) = {
            let mut w = lock(m);
            loop {
                if !w.run {
                    break;
                }
                if !w.paused && !w.tokens.is_empty() {
                    let now = Instant::now();
                    if now >= next_due {
                        break;
                    }
                    w = cv
                        .wait_timeout(w, next_due - now)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                    continue;
                }
                w = cv
                    .wait_timeout(w, Duration::from_millis(500))
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
            if !w.run {
                break;
            }
            (w.tokens.pop_front().flatten(), w.format)
        };
        let now = Instant::now();
        let interval = format.interval();
        next_due = if now.saturating_duration_since(next_due) > interval * 2 {
            now + interval
        } else {
            next_due + interval
        };
        match producer.sample(format, token.as_ref().map(|t| &t.0)) {
            // SAFETY: plain COM call; the queue is free-threaded.
            Ok(s) => {
                if let Err(e) = unsafe {
                    queue
                        .0
                        .QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &s)
                } {
                    vlog!("queueing a sample failed: {e}");
                }
            }
            Err(e) => vlog!("building a sample failed: {e}"),
        }
    }
    if let Some(s) = log::shared() {
        s.ring().reader_beat(None, tick_ms());
    }
    if com {
        // SAFETY: balanced with the CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

#[derive(Default)]
struct Producer {
    /// Latest picture read from the ring and its info.
    buf: Vec<u8>,
    last: Option<deskbridge_camera::FrameInfo>,
    last_seq: u64,
    /// Picture at the stream's size.
    out: Vec<u8>,
    /// Size and "waiting" flag of the placeholder in `out`, if `out` holds one.
    placeholder: Option<(u32, u32, bool)>,
    delivered: u64,
    last_state: Option<WriterState>,
}

impl Producer {
    fn picture(&mut self, f: Format) {
        let (w, h) = (f.width, f.height);
        let len = nv12::size(w, h);
        if self.out.len() != len {
            self.out.resize(len, 0);
            self.placeholder = None;
        }
        let now = tick_ms();
        let state = match log::shared() {
            Some(s) => {
                let ring = s.ring();
                ring.reader_beat(
                    Some(ReaderRequest {
                        width: w,
                        height: h,
                        fps: f.fps(),
                    }),
                    now,
                );
                let st = ring.writer_state(now);
                if st == WriterState::Streaming
                    && let Some(fi) = ring.read_latest(self.last_seq, &mut self.buf)
                {
                    self.last_seq = fi.seq;
                    self.last = Some(fi);
                    nv12::scale(&self.buf, fi.width, fi.height, &mut self.out, w, h);
                    self.placeholder = None;
                    ring.header()
                        .reader_frames
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return self.note_state(st);
                }
                ring.header()
                    .reader_repeats
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                st
            }
            None => WriterState::Idle,
        };
        self.note_state(state);
        if state == WriterState::Streaming && self.last.is_some() && self.placeholder.is_none() {
            return; // no new picture yet: show the previous one again
        }
        self.last = None;
        let waiting = state == WriterState::Waiting;
        if self.placeholder != Some((w, h, waiting)) {
            nv12::placeholder(&mut self.out, w, h, waiting);
            self.placeholder = Some((w, h, waiting));
        }
    }

    fn note_state(&mut self, st: WriterState) {
        if self.last_state != Some(st) {
            vlog!("camera picture source: {st:?}");
            self.last_state = Some(st);
        }
    }

    fn sample(&mut self, f: Format, token: Option<&IUnknown>) -> windows::core::Result<IMFSample> {
        self.picture(f);
        let (w, h) = (f.width as usize, f.height as usize);
        // SAFETY: the 2D buffer is locked while we write inside its pitch-sized rows.
        unsafe {
            let buffer = MFCreate2DMediaBuffer(f.width, f.height, MFVideoFormat_NV12.data1, false)?;
            let b2: IMF2DBuffer = buffer.cast()?;
            let mut scan0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            b2.Lock2D(&mut scan0, &mut pitch)?;
            if pitch > 0 && !scan0.is_null() {
                let pitch = pitch as usize;
                for row in 0..(h + h / 2) {
                    std::ptr::copy_nonoverlapping(
                        self.out.as_ptr().add(row * w),
                        scan0.add(row * pitch),
                        w,
                    );
                }
            }
            b2.Unlock2D()?;
            let _ = buffer.SetCurrentLength(
                b2.GetContiguousLength()
                    .unwrap_or(nv12::size(f.width, f.height) as u32),
            );
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(f.duration_hns())?;
            if let Some(t) = token {
                sample.SetUnknown(&MFSampleExtension_Token, t)?;
            }
            self.delivered += 1;
            if self.delivered == 1 {
                vlog!("first sample delivered ({}x{})", f.width, f.height);
            }
            Ok(sample)
        }
    }
}

#[implement(IMFMediaStream2)]
pub(crate) struct MediaStream {
    core: Arc<StreamCore>,
    _count: ObjectCount,
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows::core::Result<IMFMediaEvent> {
        let q = self.core.queue.clone();
        // SAFETY: plain COM call, outside every lock (it may block).
        unsafe { q.GetEvent(flags.0) }
    }

    fn BeginGetEvent(
        &self,
        cb: Option<&IMFAsyncCallback>,
        state: Option<&IUnknown>,
    ) -> windows::core::Result<()> {
        // SAFETY: plain COM call.
        unsafe { self.core.queue.BeginGetEvent(cb, state) }
    }

    fn EndGetEvent(&self, result: Option<&IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: plain COM call.
        unsafe { self.core.queue.EndGetEvent(result) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        ext: *const GUID,
        hr: windows::core::HRESULT,
        v: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        // SAFETY: plain COM call with the caller's pointers.
        unsafe { self.core.queue.QueueEventParamVar(met, ext, hr, v) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> windows::core::Result<IMFMediaSource> {
        let st = lock(&self.core.state);
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        st.source.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> windows::core::Result<IMFStreamDescriptor> {
        if lock(&self.core.state).shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(self.core.sd.clone())
    }

    fn RequestSample(&self, token: Option<&IUnknown>) -> windows::core::Result<()> {
        let st = lock(&self.core.state);
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        if !st.active {
            return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());
        }
        let (m, cv) = &*self.core.work;
        // A request without a token still gets its sample.
        lock(m).tokens.push_back(token.cloned().map(Agile));
        cv.notify_all();
        Ok(())
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> windows::core::Result<()> {
        let mut st = lock(&self.core.state);
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        st.stream_state = value;
        let (m, cv) = &*self.core.work;
        lock(m).paused = value != MF_STREAM_STATE_RUNNING;
        cv.notify_all();
        Ok(())
    }

    fn GetStreamState(&self) -> windows::core::Result<MF_STREAM_STATE> {
        let st = lock(&self.core.state);
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(st.stream_state)
    }
}
