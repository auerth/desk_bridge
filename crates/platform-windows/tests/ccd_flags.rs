//! Validates (never applies) which SetDisplayConfig flag combinations Windows accepts for the
//! "extend all displays" topology change. `-- --ignored --nocapture`
#![cfg(windows)]

use windows::Win32::Devices::Display::{
    SDC_ALLOW_CHANGES, SDC_APPLY, SDC_TOPOLOGY_EXTEND, SDC_VALIDATE, SetDisplayConfig,
};

#[test]
#[ignore = "talks to the display stack (validate only)"]
fn which_extend_flags_are_valid() {
    let plain = unsafe { SetDisplayConfig(None, None, SDC_VALIDATE | SDC_TOPOLOGY_EXTEND) };
    let with_changes = unsafe {
        SetDisplayConfig(
            None,
            None,
            SDC_VALIDATE | SDC_TOPOLOGY_EXTEND | SDC_ALLOW_CHANGES,
        )
    };
    let apply_changes = unsafe {
        SetDisplayConfig(
            None,
            None,
            SDC_VALIDATE | SDC_APPLY | SDC_TOPOLOGY_EXTEND | SDC_ALLOW_CHANGES,
        )
    };
    println!(
        "validate extend: {plain}; extend+allow_changes: {with_changes}; apply+extend+allow_changes: {apply_changes}"
    );
}
