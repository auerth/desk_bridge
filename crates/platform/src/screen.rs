use crate::{Result, UnboundedSender};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenInfo {
    pub id: u32,
    /// Position in the device's virtual desktop, physical pixels.
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub dpi: u32,
    pub refresh_mhz: u32,
    pub primary: bool,
    pub name: String,
}

impl ScreenInfo {
    #[must_use]
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    #[must_use]
    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
}

pub trait ScreenProvider: Send + Sync {
    fn screens(&self) -> Vec<ScreenInfo>;
    fn cursor_pos(&self) -> Point;
    /// Notifies when the display configuration changes (monitor plugged, resolution, DPI).
    fn watch(&self, tx: UnboundedSender<()>) -> Result<()>;
}
