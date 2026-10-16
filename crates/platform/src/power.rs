use crate::{Result, UnboundedSender};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    Suspend,
    Resume,
    /// IP address / network interface changed.
    NetworkChanged,
    LidClosed,
    LidOpened,
}

pub trait PowerManager: Send + Sync {
    fn watch(&self, tx: UnboundedSender<PowerEvent>) -> Result<()>;
    /// Sets the AC lid-close action to "do nothing" (`true`) or restores the saved value.
    fn set_lid_action_do_nothing(&self, on: bool) -> Result<()>;
    /// Keeps the system awake while focus mode runs.
    fn set_keep_awake(&self, on: bool);
}
