//! The system's "reduce motion" setting. GPUI does not read it by itself.

use crate::dev;

/// Whether animations should render their end state. `VSCP_DEV_REDUCE_MOTION`
/// overrides the system setting in development builds.
pub fn reduce_motion() -> bool {
    dev::reduce_motion().unwrap_or_else(system_reduce_motion)
}

#[cfg(target_os = "macos")]
fn system_reduce_motion() -> bool {
    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    // SAFETY: a documented NSWorkspace property that returns a BOOL. The typed
    // binding sits behind the NSAccessibility feature, which would add nothing else.
    unsafe { objc2::msg_send![&*workspace, accessibilityDisplayShouldReduceMotion] }
}

#[cfg(not(target_os = "macos"))]
fn system_reduce_motion() -> bool {
    false
}
