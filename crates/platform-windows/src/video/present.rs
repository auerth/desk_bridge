//! Native Direct3D 11 video window (`VideoPresenter`).
//!
//! * own Win32 window on its own thread with a message loop (no WebView, no winit);
//! * modes: `Fullscreen` (borderless, topmost, covers one monitor), `Region` (borderless on part
//!   of a monitor, optionally registered as an AppBar so maximised PC windows leave it alone) and
//!   `Window` (normal resizable window);
//! * flip-model swap chain (`FLIP_DISCARD`, tearing, waitable object); every frame is presented on
//!   arrival with `Present(0, ALLOW_TEARING)`;
//! * decoded NV12 pictures are converted with the D3D11 video processor into a BGRA "scene"
//!   texture, then drawn letterboxed with a bilinear shader; the remote cursor and the stats
//!   overlay are alpha-blended sprites on top of the scene (the scene is kept, so pointer
//!   movement can be redrawn without a new video frame);
//! * all OS objects (window, AppBar registration, D3D objects) are released by `Drop` guards.

use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use deskbridge_platform::video::{CursorShapeData, CursorState, Frame, FrameData, VideoPresenter};
use deskbridge_platform::{PlatformError, Result, UnboundedReceiver, UnboundedSender, unbounded_channel};
use parking_lot::Mutex;
use windows::Win32::Foundation::{
    BOOL, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    RECT, WPARAM,
};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD,
    D3D11_BLEND_ZERO, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_COMPARISON_NEVER,
    D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_RASTERIZER_DESC,
    D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SAMPLER_DESC, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
    ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader,
    ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11Resource, ID3D11SamplerState,
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_MWA_NO_ALT_ENTER, DXGI_MWA_NO_WINDOW_CHANGES, DXGI_PRESENT_ALLOW_TEARING,
    DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2,
    IDXGIFactory5, IDXGISwapChain1, IDXGISwapChain2, DXGI_FEATURE_PRESENT_ALLOW_TEARING,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_LEFT, DT_NOPREFIX, DT_TOP,
    DeleteDC, DeleteObject, DrawTextW, FF_DONTCARE, FIXED_PITCH, FW_NORMAL, GetMonitorInfoW, HBITMAP,
    HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, ValidateRect, ANTIALIASED_QUALITY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::Win32::UI::Shell::{
    ABE_LEFT, ABE_RIGHT, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetClientRect, GetMessageW, HTCLIENT, MA_NOACTIVATE, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, SIZE_MINIMIZED, SW_SHOW, SW_SHOWNOACTIVATE, SetCursor,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_MOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SIZE, WNDCLASSEXW,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, CREATESTRUCTW,
};
use windows::core::{Interface, PCSTR, PCWSTR};

use super::common::{
    D3dDevice, GpuFrame, adapter_by_luid, create_device, device_adapter_info, find_output_by_rect,
    list_outputs, os_err,
};
use super::vp::{ConvKind, VpConverter};

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_APP_REDRAW: u32 = WM_APP + 2;
const WM_APPBAR_CALLBACK: u32 = WM_APP + 10;
const ABN_POSCHANGED: usize = 1;
const CLASS_NAME: &str = "DeskBridgeVideoWindow";

/// A rectangle in physical pixels (virtual desktop coordinates unless noted otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl PxRect {
    #[must_use]
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    fn to_rect(self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.x + self.w as i32, bottom: self.y + self.h as i32 }
    }
}

/// Where and how the video window appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentMode {
    /// Borderless topmost window covering a whole monitor.
    Fullscreen { monitor_rect: PxRect },
    /// Borderless topmost window on part of a monitor (split mode). With `appbar` the window
    /// registers as an AppBar so the PC work area shrinks to the free part.
    Region { rect: PxRect, appbar: bool },
    /// Normal resizable window with the given initial client size.
    Window { width: u32, height: u32 },
}

/// Notifications from the window thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterEvent {
    /// The window was closed (by the user or programmatically).
    Closed,
    /// The client area changed size.
    Resized { width: u32, height: u32 },
}

// ---------------------------------------------------------------------------------------------
// Shared state between the window thread and the presenting thread
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct RemoteCursor {
    x: f32,
    y: f32,
    visible: bool,
    shape: Option<CursorShapeData>,
    shape_dirty: bool,
}

struct Overlay {
    enabled: bool,
    text: String,
    dirty: bool,
}

struct Shared {
    hwnd: AtomicIsize,
    closed: AtomicBool,
    client_rect: Mutex<PxRect>,
    /// Pointer position in client coordinates while it is over the window.
    pointer: Mutex<Option<(i32, i32)>>,
    remote: Mutex<RemoteCursor>,
    overlay: Mutex<Overlay>,
    render: Mutex<Option<Renderer>>,
    events: UnboundedSender<PresenterEvent>,
    mode: PresentMode,
    appbar: Mutex<Option<AppBarGuard>>,
    last_redraw: Mutex<Option<Instant>>,
}

// SAFETY: every field is either atomic, behind a mutex, or an interface of a multithread-protected
// D3D11 device (see `create_device`). `Renderer` is only touched while its mutex is held.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

/// Native video window. Creating it spawns the window thread; `Drop` closes it.
pub struct D3dPresenter {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    device: D3dDevice,
    events: Option<UnboundedReceiver<PresenterEvent>>,
    waitable: HANDLE,
}

// SAFETY: see `Shared`; the waitable handle is only waited on while holding the render mutex.
unsafe impl Send for D3dPresenter {}

impl D3dPresenter {
    /// Opens the window. `device`: the D3D11 device the decoder uses (so decoded textures can be
    /// sampled without copies); `None` creates one on the adapter that owns the target monitor.
    pub fn new(mode: PresentMode, device: Option<&ID3D11Device>) -> Result<Self> {
        crate::input::init_dpi_awareness();
        let (events_tx, events_rx) = unbounded_channel();
        let shared = Arc::new(Shared {
            hwnd: AtomicIsize::new(0),
            closed: AtomicBool::new(false),
            client_rect: Mutex::new(PxRect::default()),
            pointer: Mutex::new(None),
            remote: Mutex::new(RemoteCursor::default()),
            overlay: Mutex::new(Overlay { enabled: false, text: String::new(), dirty: false }),
            render: Mutex::new(None),
            events: events_tx,
            mode,
            appbar: Mutex::new(None),
            last_redraw: Mutex::new(None),
        });
        let dev_in = device.cloned();
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<D3dDevice, String>>();
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("db-video-window".into())
            .spawn(move || window_thread(sh, dev_in, ready_tx))
            .map_err(|e| PlatformError::other(format!("spawn window thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(device)) => {
                let waitable = shared
                    .render
                    .lock()
                    .as_ref()
                    .map_or(HANDLE::default(), |r| r.waitable);
                Ok(Self { shared, thread: Some(thread), device, events: Some(events_rx), waitable })
            }
            Ok(Err(msg)) => {
                let _ = thread.join();
                Err(PlatformError::other(msg))
            }
            Err(_) => {
                let _ = thread.join();
                Err(PlatformError::other("video window thread died during setup"))
            }
        }
    }

    /// The D3D11 device the window renders with (hand it to the decoder when `new` was called
    /// with `None`).
    #[must_use]
    pub fn device(&self) -> &ID3D11Device {
        &self.device.device
    }

    #[must_use]
    pub fn adapter_name(&self) -> &str {
        &self.device.adapter_name
    }

    /// Client area of the window in virtual-desktop coordinates (for the input region).
    #[must_use]
    pub fn window_rect(&self) -> PxRect {
        *self.shared.client_rect.lock()
    }

    /// Takes the event receiver (closed / resized notifications).
    pub fn take_events(&mut self) -> Option<UnboundedReceiver<PresenterEvent>> {
        self.events.take()
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// Shows or hides the stats overlay and sets its text (multi-line).
    pub fn set_stats_overlay(&self, enabled: bool, text: &str) {
        {
            let mut o = self.shared.overlay.lock();
            if o.text != text || o.enabled != enabled {
                o.enabled = enabled;
                o.text.clear();
                o.text.push_str(text);
                o.dirty = true;
            }
        }
        // Repaint (also when no new frame arrives, e.g. static picture).
        let h = self.shared.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            // SAFETY: posting to our own window; failure means it is gone.
            unsafe {
                let _ = PostMessageW(HWND(h as *mut c_void), WM_APP_REDRAW, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Re-presents the current picture (e.g. after the overlay or the cursor changed).
    pub fn redraw(&self) {
        redraw_now(&self.shared, true);
    }

    fn close_window(&mut self) {
        let h = self.shared.hwnd.load(Ordering::SeqCst);
        if h != 0 && !self.shared.closed.load(Ordering::SeqCst) {
            // SAFETY: posting to our own window; failure means it is already gone.
            unsafe {
                let _ = PostMessageW(HWND(h as *mut c_void), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl VideoPresenter for D3dPresenter {
    fn present(&mut self, frame: &Frame) -> Result<()> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(PlatformError::other("video window is closed"));
        }
        // Pace on the swap chain (latency 1): blocks only if a previous present is still queued.
        if !self.waitable.is_invalid() {
            // SAFETY: handle owned by the swap chain, alive while the renderer exists.
            unsafe {
                let _ = WaitForSingleObjectEx(self.waitable, 20, false);
            }
        }
        let mut guard = self.shared.render.lock();
        let r = guard.as_mut().ok_or_else(|| PlatformError::other("renderer not ready"))?;
        r.update_scene(frame)?;
        let draw = DrawInput::collect(&self.shared, r);
        r.compose_and_present(&draw)
    }

    fn set_cursor(&mut self, cursor: &CursorState) {
        {
            let mut rc = self.shared.remote.lock();
            rc.x = cursor.x;
            rc.y = cursor.y;
            rc.visible = cursor.visible;
            if let Some(s) = &cursor.shape {
                rc.shape = Some(s.clone());
                rc.shape_dirty = true;
            }
        }
        redraw_now(&self.shared, false);
    }

    fn close(&mut self) {
        self.close_window();
    }
}

impl Drop for D3dPresenter {
    fn drop(&mut self) {
        self.close_window();
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing input snapshot
// ---------------------------------------------------------------------------------------------

struct DrawInput {
    /// Pointer in client pixels if the local pointer is over the window.
    pointer: Option<(i32, i32)>,
    remote: Option<(f32, f32)>,
    overlay: Option<String>,
}

impl DrawInput {
    fn collect(shared: &Shared, r: &mut Renderer) -> Self {
        let pointer = *shared.pointer.lock();
        let remote = {
            let mut rc = shared.remote.lock();
            if rc.shape_dirty {
                rc.shape_dirty = false;
                if let Some(s) = &rc.shape {
                    r.set_cursor_shape(s);
                }
            }
            if rc.visible { Some((rc.x, rc.y)) } else { None }
        };
        let overlay = {
            let mut o = shared.overlay.lock();
            if !o.enabled {
                None
            } else {
                if o.dirty {
                    o.dirty = false;
                    r.set_overlay_text(&o.text);
                }
                Some(o.text.clone())
            }
        };
        Self { pointer, remote, overlay }
    }
}

/// Redraws the current scene (cursor moved, overlay changed, window exposed). Rate limited to
/// ~500 Hz unless `force`.
fn redraw_now(shared: &Shared, force: bool) {
    if shared.closed.load(Ordering::SeqCst) {
        return;
    }
    if !force {
        let mut last = shared.last_redraw.lock();
        let now = Instant::now();
        if last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(2)) {
            return;
        }
        *last = Some(now);
    }
    let Some(mut guard) = shared.render.try_lock() else {
        return; // a frame is being presented right now; it will include the new state
    };
    if let Some(r) = guard.as_mut() {
        if r.has_scene() {
            let draw = DrawInput::collect(shared, r);
            if let Err(e) = r.compose_and_present(&draw) {
                tracing::debug!("redraw failed: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------------------------

const HLSL: &str = r"
cbuffer P : register(b0) { float4 rect; float4 uv; };
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
V vs_main(uint id : SV_VertexID) {
    float2 c = float2(id & 1, id >> 1);
    V o;
    o.pos = float4(lerp(rect.xy, rect.zw, c), 0, 1);
    o.uv = lerp(uv.xy, uv.zw, c);
    return o;
}
Texture2D t0 : register(t0);
SamplerState s0 : register(s0);
float4 ps_main(V i) : SV_Target { return t0.Sample(s0, i.uv); }
";

#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    rect: [f32; 4],
    uv: [f32; 4],
}

struct Sprite {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    w: u32,
    h: u32,
    hot: (i32, i32),
}

struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    waitable: HANDLE,
    tearing: bool,
    flags: DXGI_SWAP_CHAIN_FLAG,
    rtv: Option<ID3D11RenderTargetView>,
    size: (u32, u32),
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    blend_alpha: ID3D11BlendState,
    blend_opaque: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    cb: ID3D11Buffer,
    scene: Option<(ID3D11Texture2D, ID3D11ShaderResourceView, u32, u32)>,
    nv12: Option<VpConverter>,
    cursor: Option<Sprite>,
    overlay: Option<Sprite>,
}

fn compile(entry: &str, target: &str) -> Result<ID3DBlob> {
    let entry_c = std::ffi::CString::new(entry).expect("no NUL");
    let target_c = std::ffi::CString::new(target).expect("no NUL");
    let mut code: Option<ID3DBlob> = None;
    let mut err: Option<ID3DBlob> = None;
    // SAFETY: source pointer/len are valid; the C strings outlive the call.
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr().cast()),
            PCSTR(target_c.as_ptr().cast()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut err),
        )
    };
    if let Err(e) = r {
        let msg = err
            .map(|b| {
                // SAFETY: blob pointer/size are valid while the blob lives.
                unsafe {
                    String::from_utf8_lossy(std::slice::from_raw_parts(
                        b.GetBufferPointer().cast::<u8>(),
                        b.GetBufferSize(),
                    ))
                    .into_owned()
                }
            })
            .unwrap_or_default();
        return Err(PlatformError::other(format!("D3DCompile {entry}: {e} {msg}")));
    }
    code.ok_or_else(|| PlatformError::other("D3DCompile returned no code"))
}

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    // SAFETY: valid for the lifetime of the blob borrow.
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

impl Renderer {
    fn new(dev: &D3dDevice, hwnd: HWND, w: u32, h: u32) -> Result<Self> {
        let (device, context) = (dev.device.clone(), dev.context.clone());
        // --- swap chain ---
        // SAFETY: plain COM calls with valid descriptors.
        let (swap, waitable, tearing, flags) = unsafe {
            let dxgi: IDXGIDevice = device.cast().map_err(|e| os_err("IDXGIDevice", &e))?;
            let adapter = dxgi.GetAdapter().map_err(|e| os_err("GetAdapter", &e))?;
            let factory: IDXGIFactory2 =
                adapter.GetParent().map_err(|e| os_err("adapter parent factory", &e))?;
            let mut allow = BOOL(0);
            if let Ok(f5) = factory.cast::<IDXGIFactory5>() {
                let _ = f5.CheckFeatureSupport(
                    DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                    (&mut allow as *mut BOOL).cast(),
                    size_of::<BOOL>() as u32,
                );
            }
            let tearing = allow.as_bool();
            let mut flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0;
            if tearing {
                flags |= DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0;
            }
            let flags = DXGI_SWAP_CHAIN_FLAG(flags);
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w.max(1),
                Height: h.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: BOOL(0),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: flags.0 as u32,
            };
            let swap = factory
                .CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)
                .map_err(|e| os_err("CreateSwapChainForHwnd", &e))?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
            let s2: IDXGISwapChain2 = swap.cast().map_err(|e| os_err("IDXGISwapChain2", &e))?;
            let _ = s2.SetMaximumFrameLatency(1);
            let waitable = s2.GetFrameLatencyWaitableObject();
            (swap, waitable, tearing, flags)
        };

        // --- shaders and state ---
        let vs_blob = compile("vs_main", "vs_5_0")?;
        let ps_blob = compile("ps_main", "ps_5_0")?;
        let mut vs: Option<ID3D11VertexShader> = None;
        let mut ps: Option<ID3D11PixelShader> = None;
        let mut sampler: Option<ID3D11SamplerState> = None;
        let mut blend_alpha: Option<ID3D11BlendState> = None;
        let mut blend_opaque: Option<ID3D11BlendState> = None;
        let mut raster: Option<ID3D11RasterizerState> = None;
        let mut cb: Option<ID3D11Buffer> = None;
        // SAFETY: valid descriptors / bytecode; out pointers valid.
        unsafe {
            device
                .CreateVertexShader(blob_bytes(&vs_blob), None, Some(&mut vs))
                .map_err(|e| os_err("CreateVertexShader", &e))?;
            device
                .CreatePixelShader(blob_bytes(&ps_blob), None, Some(&mut ps))
                .map_err(|e| os_err("CreatePixelShader", &e))?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&sd, Some(&mut sampler)).map_err(|e| os_err("CreateSamplerState", &e))?;
            let mk_blend = |on: bool| {
                let mut rt = D3D11_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: BOOL(i32::from(on)),
                    SrcBlend: D3D11_BLEND_ONE,
                    DestBlend: if on { D3D11_BLEND_INV_SRC_ALPHA } else { D3D11_BLEND_ZERO },
                    BlendOp: D3D11_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D11_BLEND_ONE,
                    DestBlendAlpha: if on { D3D11_BLEND_INV_SRC_ALPHA } else { D3D11_BLEND_ZERO },
                    BlendOpAlpha: D3D11_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
                };
                if !on {
                    rt.BlendEnable = BOOL(0);
                }
                D3D11_BLEND_DESC { RenderTarget: [rt; 8], ..Default::default() }
            };
            device
                .CreateBlendState(&mk_blend(true), Some(&mut blend_alpha))
                .map_err(|e| os_err("CreateBlendState", &e))?;
            device
                .CreateBlendState(&mk_blend(false), Some(&mut blend_opaque))
                .map_err(|e| os_err("CreateBlendState", &e))?;
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: BOOL(1),
                ..Default::default()
            };
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(|e| os_err("CreateRasterizerState", &e))?;
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: size_of::<Params>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            device.CreateBuffer(&bd, None, Some(&mut cb)).map_err(|e| os_err("CreateBuffer", &e))?;
        }
        let none = || PlatformError::other("D3D11 object creation returned null");
        let mut r = Self {
            device,
            context,
            swap,
            waitable,
            tearing,
            flags,
            rtv: None,
            size: (w.max(1), h.max(1)),
            vs: vs.ok_or_else(none)?,
            ps: ps.ok_or_else(none)?,
            sampler: sampler.ok_or_else(none)?,
            blend_alpha: blend_alpha.ok_or_else(none)?,
            blend_opaque: blend_opaque.ok_or_else(none)?,
            raster: raster.ok_or_else(none)?,
            cb: cb.ok_or_else(none)?,
            scene: None,
            nv12: None,
            cursor: None,
            overlay: None,
        };
        r.create_rtv()?;
        Ok(r)
    }

    fn create_rtv(&mut self) -> Result<()> {
        // SAFETY: plain COM calls.
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0).map_err(|e| os_err("GetBuffer", &e))?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            self.device
                .CreateRenderTargetView(&back, None, Some(&mut rtv))
                .map_err(|e| os_err("CreateRenderTargetView", &e))?;
            self.rtv = rtv;
        }
        Ok(())
    }

    fn resize(&mut self, w: u32, h: u32) -> Result<()> {
        let (w, h) = (w.max(1), h.max(1));
        if (w, h) == self.size {
            return Ok(());
        }
        self.rtv = None;
        // SAFETY: all references to the back buffer were dropped above.
        unsafe {
            self.context.ClearState();
            self.swap
                .ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, self.flags)
                .map_err(|e| os_err("ResizeBuffers", &e))?;
        }
        self.size = (w, h);
        self.create_rtv()
    }

    fn has_scene(&self) -> bool {
        self.scene.is_some()
    }

    fn ensure_scene(&mut self, w: u32, h: u32) -> Result<()> {
        if matches!(&self.scene, Some((_, _, sw, sh)) if (*sw, *sh) == (w, h)) {
            return Ok(());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            ..Default::default()
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: valid descriptor and out pointers.
        unsafe {
            self.device.CreateTexture2D(&desc, None, Some(&mut tex)).map_err(|e| os_err("CreateTexture2D(scene)", &e))?;
            let t = tex.as_ref().ok_or_else(|| PlatformError::other("null scene texture"))?;
            self.device
                .CreateShaderResourceView(t, None, Some(&mut srv))
                .map_err(|e| os_err("CreateShaderResourceView(scene)", &e))?;
        }
        self.nv12 = None;
        self.scene = Some((
            tex.ok_or_else(|| PlatformError::other("null scene texture"))?,
            srv.ok_or_else(|| PlatformError::other("null scene SRV"))?,
            w,
            h,
        ));
        Ok(())
    }

    /// Copies / converts the incoming picture into the BGRA scene texture.
    fn update_scene(&mut self, frame: &Frame) -> Result<()> {
        match &frame.data {
            FrameData::Bgra { data, stride } => {
                self.ensure_scene(frame.width, frame.height)?;
                let (tex, ..) = self.scene.as_ref().expect("scene just created");
                let res: ID3D11Resource = tex.cast().map_err(|e| os_err("scene resource", &e))?;
                if data.len() < stride * frame.height as usize {
                    return Err(PlatformError::other("BGRA frame buffer too small"));
                }
                // SAFETY: `data` holds at least stride*height bytes.
                unsafe {
                    self.context.UpdateSubresource(&res, 0, None, data.as_ptr().cast(), *stride as u32, 0);
                }
                Ok(())
            }
            FrameData::Gpu(any) => {
                let g = GpuFrame::from_any(any.as_ref())
                    .ok_or_else(|| PlatformError::other("GPU frame of unknown type"))?;
                if g.device.as_raw() != self.device.as_raw() {
                    return Err(PlatformError::other(
                        "frame lives on another D3D11 device (create the presenter with the decoder's device)",
                    ));
                }
                self.ensure_scene(g.width, g.height)?;
                let (scene_tex, ..) = self.scene.clone().expect("scene just created");
                if g.format == DXGI_FORMAT_NV12 {
                    if self.nv12.as_ref().is_none_or(|c| c.in_size() != (g.width, g.height)) {
                        self.nv12 = Some(VpConverter::new(
                            &self.device,
                            &self.context,
                            ConvKind::Nv12ToBgra,
                            (g.width, g.height),
                            (g.width, g.height),
                        )?);
                    }
                    self.nv12
                        .as_mut()
                        .expect("converter just created")
                        .blt(&g.texture, g.subresource, &scene_tex, 0, None)
                } else if g.format == DXGI_FORMAT_B8G8R8A8_UNORM {
                    let src: ID3D11Resource = g.texture.cast().map_err(|e| os_err("src resource", &e))?;
                    let dst: ID3D11Resource = scene_tex.cast().map_err(|e| os_err("scene resource", &e))?;
                    // SAFETY: same device, same format and size (scene sized from the frame).
                    unsafe {
                        self.context.CopySubresourceRegion(&dst, 0, 0, 0, 0, &src, g.subresource, None);
                    }
                    Ok(())
                } else {
                    Err(PlatformError::other(format!("unsupported GPU frame format {:?}", g.format)))
                }
            }
        }
    }

    fn set_cursor_shape(&mut self, s: &CursorShapeData) {
        match self.make_sprite_from_rgba(s.width, s.height, &s.rgba, (s.hot_x, s.hot_y)) {
            Ok(sp) => self.cursor = Some(sp),
            Err(e) => tracing::debug!("cursor sprite: {e}"),
        }
    }

    fn set_overlay_text(&mut self, text: &str) {
        if text.is_empty() {
            self.overlay = None;
            return;
        }
        if let Some((w, h, bgra)) = render_text_bgra(text) {
            match self.make_sprite_bgra_premul(w, h, &bgra, (0, 0)) {
                Ok(sp) => self.overlay = Some(sp),
                Err(e) => tracing::debug!("overlay sprite: {e}"),
            }
        }
    }

    fn make_sprite_from_rgba(&self, w: u32, h: u32, rgba: &[u8], hot: (i32, i32)) -> Result<Sprite> {
        if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
            return Err(PlatformError::other("bad cursor shape"));
        }
        // straight-alpha RGBA -> premultiplied BGRA
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for (src, dst) in rgba.chunks_exact(4).zip(bgra.chunks_exact_mut(4)) {
            let a = u32::from(src[3]);
            let pm = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
            dst[0] = pm(src[2]);
            dst[1] = pm(src[1]);
            dst[2] = pm(src[0]);
            dst[3] = src[3];
        }
        self.make_sprite_bgra_premul(w, h, &bgra, hot)
    }

    fn make_sprite_bgra_premul(&self, w: u32, h: u32, bgra: &[u8], hot: (i32, i32)) -> Result<Sprite> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: bgra.as_ptr().cast(), SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        let mut tex: Option<ID3D11Texture2D> = None;
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: `bgra` holds w*h*4 bytes; descriptors and out pointers are valid.
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut tex))
                .map_err(|e| os_err("CreateTexture2D(sprite)", &e))?;
            let t = tex.as_ref().ok_or_else(|| PlatformError::other("null sprite texture"))?;
            self.device
                .CreateShaderResourceView(t, None, Some(&mut srv))
                .map_err(|e| os_err("CreateShaderResourceView(sprite)", &e))?;
        }
        Ok(Sprite {
            tex: tex.ok_or_else(|| PlatformError::other("null sprite texture"))?,
            srv: srv.ok_or_else(|| PlatformError::other("null sprite SRV"))?,
            w,
            h,
            hot,
        })
    }

    /// Letterboxed destination rectangle of the scene inside the client area (pixels).
    fn letterbox(&self, sw: u32, sh: u32) -> (f32, f32, f32, f32) {
        let (cw, ch) = (self.size.0 as f32, self.size.1 as f32);
        let scale = (cw / sw as f32).min(ch / sh as f32);
        let (dw, dh) = ((sw as f32 * scale).round(), (sh as f32 * scale).round());
        (((cw - dw) / 2.0).floor(), ((ch - dh) / 2.0).floor(), dw, dh)
    }

    fn draw_quad(&self, rect_px: (f32, f32, f32, f32), srv: &ID3D11ShaderResourceView, alpha: bool) {
        let (cw, ch) = (self.size.0 as f32, self.size.1 as f32);
        let (x, y, w, h) = rect_px;
        let params = Params {
            rect: [x / cw * 2.0 - 1.0, 1.0 - y / ch * 2.0, (x + w) / cw * 2.0 - 1.0, 1.0 - (y + h) / ch * 2.0],
            uv: [0.0, 0.0, 1.0, 1.0],
        };
        // SAFETY: plain COM calls; `params` is a plain-old-data constant buffer image.
        unsafe {
            self.context.UpdateSubresource(&self.cb, 0, None, (&params as *const Params).cast::<c_void>(), 0, 0);
            let bf = [0.0f32; 4];
            self.context.OMSetBlendState(
                if alpha { &self.blend_alpha } else { &self.blend_opaque },
                Some(&bf),
                0xFFFF_FFFF,
            );
            self.context.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
            self.context.Draw(4, 0);
        }
    }

    fn compose_and_present(&mut self, d: &DrawInput) -> Result<()> {
        let Some(rtv) = self.rtv.clone() else {
            return Ok(());
        };
        let Some((_, scene_srv, sw, sh)) = self.scene.clone() else {
            return Ok(());
        };
        let vp = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: self.size.0 as f32,
            Height: self.size.1 as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        // SAFETY: plain COM calls on interfaces owned by this renderer.
        unsafe {
            let ctx = &self.context;
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[vp]));
            ctx.RSSetState(&self.raster);
            ctx.ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
        }
        let (dx, dy, dw, dh) = self.letterbox(sw, sh);
        self.draw_quad((dx, dy, dw, dh), &scene_srv, false);

        // Cursor: local pointer wins (zero latency), else the position reported by the source.
        if let Some(sp) = &self.cursor {
            let scale = dw / sw as f32;
            let pos = if let Some((px, py)) = d.pointer {
                Some((px as f32, py as f32))
            } else {
                d.remote.map(|(rx, ry)| (dx + rx * dw, dy + ry * dh))
            };
            if let Some((px, py)) = pos {
                let rect = (
                    px - sp.hot.0 as f32 * scale,
                    py - sp.hot.1 as f32 * scale,
                    sp.w as f32 * scale,
                    sp.h as f32 * scale,
                );
                self.draw_quad(rect, &sp.srv, true);
            }
        }
        if d.overlay.is_some() {
            if let Some(sp) = &self.overlay {
                self.draw_quad((8.0, 8.0, sp.w as f32, sp.h as f32), &sp.srv, true);
            }
        }
        // SAFETY: plain COM call.
        let r = unsafe {
            let flags = if self.tearing { DXGI_PRESENT_ALLOW_TEARING } else { windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0) };
            self.swap.Present(0, flags)
        };
        // SAFETY: unbind the back buffer so the next ResizeBuffers cannot be blocked by references.
        unsafe {
            self.context.OMSetRenderTargets(None, None);
        }
        if r.is_err() {
            return Err(os_err("Present", &windows::core::Error::from(r)));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Stats overlay text via GDI
// ---------------------------------------------------------------------------------------------

/// Renders multi-line text to a premultiplied BGRA bitmap (white text on a translucent dark
/// panel). Returns `(width, height, pixels)`.
fn render_text_bgra(text: &str) -> Option<(u32, u32, Vec<u8>)> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let face: Vec<u16> = "Consolas\0".encode_utf16().collect();
    // SAFETY: standard GDI sequence; every created object is selected out and deleted below.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let font = CreateFontW(
            -16, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, u32::from(DEFAULT_CHARSET.0),
            u32::from(OUT_DEFAULT_PRECIS.0), u32::from(CLIP_DEFAULT_PRECIS.0),
            u32::from(ANTIALIASED_QUALITY.0), (FIXED_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR(face.as_ptr()),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        let mut measure = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let mut buf = wide.clone();
        DrawTextW(dc, &mut buf, &mut measure, DT_CALCRECT | DT_LEFT | DT_TOP | DT_NOPREFIX);
        const PAD: i32 = 6;
        let w = (measure.right - measure.left + 2 * PAD).clamp(8, 2048);
        let h = (measure.bottom - measure.top + 2 * PAD).clamp(8, 2048);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp: HBITMAP = match CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) if !bits.is_null() => b,
            _ => {
                SelectObject(dc, old_font);
                let _ = DeleteObject(HGDIOBJ(font.0));
                let _ = DeleteDC(dc);
                return None;
            }
        };
        let old_bmp = SelectObject(dc, HGDIOBJ(bmp.0));
        let n = (w * h) as usize;
        let px = std::slice::from_raw_parts_mut(bits.cast::<u8>(), n * 4);
        // background: dark grey, ~75 % opaque, premultiplied
        const BG: [u8; 4] = [0x18, 0x18, 0x18, 0xC0];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&BG);
        }
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00FF_FFFF));
        let mut r = RECT { left: PAD, top: PAD, right: w - PAD, bottom: h - PAD };
        let mut buf = wide.clone();
        DrawTextW(dc, &mut buf, &mut r, DT_LEFT | DT_TOP | DT_NOPREFIX);
        // GDI leaves the alpha byte untouched on text pixels: restore opacity where colour changed.
        let mut out = vec![0u8; n * 4];
        for (src, dst) in px.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
            if src[..3] == BG[..3] {
                dst.copy_from_slice(&BG);
            } else {
                dst.copy_from_slice(&[src[0], src[1], src[2], 0xFF]);
            }
        }
        SelectObject(dc, old_bmp);
        SelectObject(dc, old_font);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteDC(dc);
        Some((w as u32, h as u32, out))
    }
}

// ---------------------------------------------------------------------------------------------
// AppBar
// ---------------------------------------------------------------------------------------------

/// Registers a window as AppBar and removes the registration when dropped.
struct AppBarGuard {
    hwnd: HWND,
    edge: u32,
}

// SAFETY: the guard only stores a window handle and calls thread-safe shell APIs.
unsafe impl Send for AppBarGuard {}

impl AppBarGuard {
    /// Registers `hwnd` for the monitor edge that `rect` touches and returns the approved rectangle.
    fn register(hwnd: HWND, rect: PxRect) -> Result<(Self, RECT)> {
        let want = rect.to_rect();
        // SAFETY: plain shell/GDI calls with valid structures.
        unsafe {
            let hmon = MonitorFromRect(&want, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
                return Err(PlatformError::other("GetMonitorInfo failed"));
            }
            let m = mi.rcMonitor;
            let edge = if want.left <= m.left + 1 && want.right < m.right - 1 {
                ABE_LEFT
            } else if want.right >= m.right - 1 && want.left > m.left + 1 {
                ABE_RIGHT
            } else {
                return Err(PlatformError::other("region does not touch exactly one side of the monitor"));
            };
            let mut abd = APPBARDATA {
                cbSize: size_of::<APPBARDATA>() as u32,
                hWnd: hwnd,
                uCallbackMessage: WM_APPBAR_CALLBACK,
                ..Default::default()
            };
            if SHAppBarMessage(ABM_NEW, &mut abd) == 0 {
                return Err(PlatformError::other("ABM_NEW rejected"));
            }
            let guard = Self { hwnd, edge };
            let mut rc = want;
            guard.set_pos(&mut abd, &mut rc, m);
            Ok((guard, rc))
        }
    }

    unsafe fn set_pos(&self, abd: &mut APPBARDATA, rc: &mut RECT, monitor: RECT) {
        // SAFETY: caller passes valid structures.
        unsafe {
            abd.uEdge = self.edge;
            // The appbar rectangle spans the full monitor height along the chosen edge.
            let width = rc.right - rc.left;
            rc.top = monitor.top;
            rc.bottom = monitor.bottom;
            abd.rc = *rc;
            SHAppBarMessage(ABM_QUERYPOS, abd);
            if self.edge == ABE_LEFT {
                abd.rc.right = abd.rc.left + width;
            } else {
                abd.rc.left = abd.rc.right - width;
            }
            SHAppBarMessage(ABM_SETPOS, abd);
            *rc = abd.rc;
        }
    }
}

impl Drop for AppBarGuard {
    fn drop(&mut self) {
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            ..Default::default()
        };
        // SAFETY: valid structure; removing twice is harmless.
        unsafe {
            SHAppBarMessage(ABM_REMOVE, &mut abd);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Window thread
// ---------------------------------------------------------------------------------------------

/// Per-window context behind `GWLP_USERDATA`.
struct WindowCtx {
    shared: Arc<Shared>,
    tracking_mouse: AtomicBool,
}

fn utf16z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn update_client_rect(hwnd: HWND, shared: &Shared) -> (u32, u32) {
    let mut rc = RECT::default();
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: valid out pointers; hwnd is our window.
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut pt);
    }
    let (w, h) = ((rc.right - rc.left).max(0) as u32, (rc.bottom - rc.top).max(0) as u32);
    *shared.client_rect.lock() = PxRect { x: pt.x, y: pt.y, w, h };
    (w, h)
}

fn window_thread(
    shared: Arc<Shared>,
    device_in: Option<ID3D11Device>,
    ready: mpsc::Sender<std::result::Result<D3dDevice, String>>,
) {
    let result = catch_unwind(AssertUnwindSafe(|| setup_window(&shared, device_in)));
    let hwnd = match result {
        Ok(Ok((hwnd, dev))) => {
            let _ = ready.send(Ok(dev));
            hwnd
        }
        Ok(Err(e)) => {
            let _ = ready.send(Err(format!("video window: {e}")));
            return;
        }
        Err(_) => {
            let _ = ready.send(Err("video window setup panicked".into()));
            return;
        }
    };
    drop(ready);

    // Message loop. A panic in a handler must not unwind through the OS; handlers catch their own.
    let mut msg = MSG::default();
    // SAFETY: standard message pump on the thread that owns the window.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Guards: whatever is left (window destroyed by someone else, panic) is cleaned up here.
    *shared.appbar.lock() = None;
    *shared.render.lock() = None;
    shared.closed.store(true, Ordering::SeqCst);
    shared.hwnd.store(0, Ordering::SeqCst);
    // SAFETY: the window may already be gone; DestroyWindow then fails harmlessly.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    let _ = shared.events.send(PresenterEvent::Closed);
}

fn setup_window(shared: &Arc<Shared>, device_in: Option<ID3D11Device>) -> Result<(HWND, D3dDevice)> {
    let (style, ex_style, rect, show) = match shared.mode {
        PresentMode::Fullscreen { monitor_rect } => {
            (WS_POPUP, WS_EX_TOPMOST | WS_EX_TOOLWINDOW, monitor_rect, SW_SHOWNOACTIVATE)
        }
        PresentMode::Region { rect, .. } => (WS_POPUP, WS_EX_TOPMOST | WS_EX_TOOLWINDOW, rect, SW_SHOWNOACTIVATE),
        PresentMode::Window { width, height } => {
            let mut rc = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            // SAFETY: valid rect pointer.
            unsafe {
                let _ = AdjustWindowRectEx(&mut rc, WS_OVERLAPPEDWINDOW, false, WS_EX_APPWINDOW);
            }
            (
                WS_OVERLAPPEDWINDOW,
                WS_EX_APPWINDOW,
                PxRect::new(
                    windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                    windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                    (rc.right - rc.left) as u32,
                    (rc.bottom - rc.top) as u32,
                ),
                SW_SHOW,
            )
        }
    };

    // Device: reuse the decoder's, or create one on the adapter that owns the target monitor.
    let dev = if let Some(d) = device_in {
        let (luid, name) = device_adapter_info(&d);
        // SAFETY: plain COM call.
        let ctx = unsafe { d.GetImmediateContext() }.map_err(|e| os_err("GetImmediateContext", &e))?;
        super::common::enable_multithread_protection(&ctx);
        D3dDevice { device: d, context: ctx, adapter_luid: luid, adapter_name: name }
    } else {
        let adapter = list_outputs()
            .ok()
            .and_then(|o| find_output_by_rect(&o, rect.x, rect.y, rect.w, rect.h).cloned())
            .and_then(|o| adapter_by_luid(o.adapter_luid).ok());
        create_device(adapter.as_ref())?
    };

    let class = utf16z(CLASS_NAME);
    let title = utf16z("Desk Bridge");
    let ctx = Box::into_raw(Box::new(WindowCtx { shared: shared.clone(), tracking_mouse: AtomicBool::new(false) }));
    // SAFETY: plain Win32 calls; `class`/`title` outlive them; `ctx` is freed in WM_NCDESTROY
    // handling below (or here if creation fails).
    let hwnd = unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).map_err(|e| os_err("GetModuleHandle", &e))?.into();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            drop(Box::from_raw(ctx));
            return Err(os_err("RegisterClassExW", &windows::core::Error::from_win32()));
        }
        match CreateWindowExW(
            ex_style,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            rect.x,
            rect.y,
            rect.w as i32,
            rect.h as i32,
            None,
            None,
            hinst,
            Some(ctx.cast::<c_void>()),
        ) {
            Ok(h) => h,
            Err(e) => {
                drop(Box::from_raw(ctx));
                return Err(os_err("CreateWindowExW", &e));
            }
        }
    };
    shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);

    // AppBar first: it may move the window to the approved rectangle.
    if let PresentMode::Region { rect, appbar: true } = shared.mode {
        match AppBarGuard::register(hwnd, rect) {
            Ok((guard, rc)) => {
                // SAFETY: our window.
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        rc.left,
                        rc.top,
                        rc.right - rc.left,
                        rc.bottom - rc.top,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                *shared.appbar.lock() = Some(guard);
            }
            Err(e) => tracing::warn!("AppBar registration failed, using a plain topmost window: {e}"),
        }
    }

    let (cw, ch) = update_client_rect(hwnd, shared);
    let renderer = Renderer::new(&dev, hwnd, cw, ch)?;
    *shared.render.lock() = Some(renderer);
    // SAFETY: our window.
    unsafe {
        let _ = ShowWindow(hwnd, show);
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOZORDER | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE);
    }
    let _ = update_client_rect(hwnd, shared);
    Ok((hwnd, dev))
}

unsafe fn ctx_of(hwnd: HWND) -> Option<&'static WindowCtx> {
    // SAFETY: GWLP_USERDATA holds the pointer stored in WM_NCCREATE (null before / after).
    unsafe {
        let p = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowCtx;
        p.as_ref()
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Never unwind into the OS.
    catch_unwind(AssertUnwindSafe(|| wndproc_inner(hwnd, msg, wparam, lparam)))
        // SAFETY: plain default handling.
        .unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

fn wndproc_inner(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: Win32 contract for window procedures; see `ctx_of`.
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let Some(ctx) = ctx_of(hwnd) else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        let shared = &ctx.shared;
        let passive = !matches!(shared.mode, PresentMode::Window { .. });
        match msg {
            WM_CREATE => LRESULT(0),
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let _ = ValidateRect(hwnd, None);
                redraw_now(shared, true);
                LRESULT(0)
            }
            WM_APP_REDRAW => {
                redraw_now(shared, true);
                LRESULT(0)
            }
            WM_SIZE => {
                if wparam.0 as u32 != SIZE_MINIMIZED {
                    let (w, h) = update_client_rect(hwnd, shared);
                    if w > 0 && h > 0 {
                        if let Some(r) = shared.render.lock().as_mut() {
                            if let Err(e) = r.resize(w, h) {
                                tracing::warn!("swap chain resize failed: {e}");
                            }
                        }
                        let _ = shared.events.send(PresenterEvent::Resized { width: w, height: h });
                        redraw_now(shared, true);
                    }
                }
                LRESULT(0)
            }
            WM_MOVE => {
                let _ = update_client_rect(hwnd, shared);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                *shared.pointer.lock() = Some((x, y));
                if !ctx.tracking_mouse.swap(true, Ordering::Relaxed) {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                }
                redraw_now(shared, false);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                ctx.tracking_mouse.store(false, Ordering::Relaxed);
                *shared.pointer.lock() = None;
                redraw_now(shared, true);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if (lparam.0 & 0xFFFF) as u32 == HTCLIENT {
                    // The remote cursor is drawn by us; hide the system cursor.
                    SetCursor(None);
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_MOUSEACTIVATE if passive => LRESULT(MA_NOACTIVATE as isize),
            WM_APPBAR_CALLBACK => {
                if wparam.0 == ABN_POSCHANGED {
                    if let PresentMode::Region { rect, appbar: true } = shared.mode {
                        let guard = shared.appbar.lock();
                        if let Some(g) = guard.as_ref() {
                            let (hmon, mut mi) = (
                                MonitorFromRect(&rect.to_rect(), MONITOR_DEFAULTTONEAREST),
                                MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() },
                            );
                            if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                                let mut abd = APPBARDATA {
                                    cbSize: size_of::<APPBARDATA>() as u32,
                                    hWnd: hwnd,
                                    ..Default::default()
                                };
                                let mut rc = rect.to_rect();
                                g.set_pos(&mut abd, &mut rc, mi.rcMonitor);
                                let _ = SetWindowPos(
                                    hwnd,
                                    HWND_TOPMOST,
                                    rc.left,
                                    rc.top,
                                    rc.right - rc.left,
                                    rc.bottom - rc.top,
                                    SWP_NOACTIVATE,
                                );
                            }
                        }
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                // Remove the AppBar before the window disappears.
                *shared.appbar.lock() = None;
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                *shared.appbar.lock() = None;
                shared.closed.store(true, Ordering::SeqCst);
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                // Last message of the window: release the context allocated in `setup_window`.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ctx as *const WindowCtx as *mut WindowCtx));
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
