//! System accessibility preferences used by the app.

use crate::dev;

/// Whether animations should render their end state. `VELA_DEV_REDUCE_MOTION`
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

/// Whether the platform asks applications to use opaque surfaces.
pub fn reduce_transparency() -> bool {
    dev::reduce_transparency().unwrap_or_else(system_reduce_transparency)
}

#[cfg(target_os = "macos")]
fn system_reduce_transparency() -> bool {
    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    // SAFETY: this documented NSWorkspace property returns a BOOL.
    unsafe { objc2::msg_send![&*workspace, accessibilityDisplayShouldReduceTransparency] }
}

#[cfg(not(target_os = "macos"))]
fn system_reduce_transparency() -> bool {
    false
}
