//! The focus viewer receives a cursor update for every pointer move on the laptop (~144 a second),
//! each carrying the unchanged shape. Every one used to create a new cursor texture: the PC's
//! RTX 3070 Ti filled up to 6.9 GB within 3.5 minutes, then `Present` hung and new devices failed
//! with `E_OUTOFMEMORY`. Opens a small window on the real GPU. `-- --ignored --nocapture`
#![cfg(windows)]

use std::time::Duration;

use deskbridge_platform::video::{CursorShapeData, CursorState, Frame, FrameData, VideoPresenter};
use deskbridge_platform_windows::video::{D3dPresenter, PresentMode};
use windows::Win32::Graphics::Dxgi::{
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIDevice,
};
use windows::core::Interface;

/// Video memory this process uses on the presenter's adapter (bytes).
fn vram(p: &D3dPresenter) -> u64 {
    // SAFETY: plain COM calls on live interfaces.
    unsafe {
        let dxgi: IDXGIDevice = p.device().cast().unwrap();
        let adapter: IDXGIAdapter3 = dxgi.GetAdapter().unwrap().cast().unwrap();
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        adapter
            .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
            .unwrap();
        info.CurrentUsage
    }
}

#[test]
#[ignore = "opens a window on the real GPU"]
fn cursor_moves_with_an_unchanged_shape_do_not_grow_video_memory() {
    let (w, h) = (640u32, 360u32);
    let mut p = D3dPresenter::new(
        PresentMode::Window {
            width: w,
            height: h,
        },
        None,
    )
    .unwrap();
    let frame = Frame {
        width: w,
        height: h,
        ts_us: 0,
        data: FrameData::Bgra {
            data: vec![96; (w * h * 4) as usize],
            stride: (w * 4) as usize,
        },
    };
    p.present(&frame).unwrap();
    let shape = CursorShapeData {
        width: 48,
        height: 48,
        hot_x: 0,
        hot_y: 0,
        rgba: vec![255; 48 * 48 * 4],
    };
    let moves = |p: &mut D3dPresenter, n: u32| {
        for i in 0..n {
            p.set_cursor(&CursorState {
                x: (i % 100) as f32 / 100.0,
                y: 0.5,
                visible: true,
                shape_serial: 1,
                shape: Some(shape.clone()),
            });
            // above the presenter's 2 ms redraw limit: every move is drawn
            std::thread::sleep(Duration::from_millis(3));
        }
    };
    moves(&mut p, 100);
    let before = vram(&p);
    moves(&mut p, 3000);
    let after = vram(&p);
    p.close();
    let grown_mb = after.saturating_sub(before) / (1024 * 1024);
    println!(
        "video memory before {} MB, after 3000 moves {} MB (+{grown_mb} MB)",
        before / (1024 * 1024),
        after / (1024 * 1024)
    );
    assert!(
        grown_mb < 32,
        "cursor moves leaked {grown_mb} MB of video memory"
    );
}
