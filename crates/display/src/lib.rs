//! Display management for focus mode (plan 6.4, 8.7, 8.8, 14, 16).
//!
//! Everything here is platform independent; the Windows implementation of
//! [`deskbridge_platform::display::DisplayManager`] lives in `deskbridge-platform-windows`
//! and builds on the pure logic of this crate ([`topology`], [`vdd`]).
//!
//! * [`plan`]: virtual display mode / viewer region / driver mode list for a layout.
//! * [`session`]: [`FocusDisplaySession`] – topology saved to disk first, rollback at every step,
//!   crash recovery at the next start.
//! * [`topology`]: model of a CCD configuration, re-matching after reboot, edits.
//! * [`vdd`]: `vdd_settings.xml` patching, pipe messages, render GPU choice.
//! * [`mock`]: recording `DisplayManager` with failure injection.

pub mod mock;
pub mod plan;
pub mod session;
pub mod topology;
pub mod util;
pub mod vdd;

pub use plan::{FocusLayout, FocusPlan, PlanOptions, Rect, Side, SplitShare, plan_focus};
pub use session::{
    FocusDisplaySession, FocusMarker, FocusOptions, FocusState, OsProcessProbe, ProcessProbe,
    RecoveryOutcome, ScreenId, StepTimeouts, recover_on_startup,
};
