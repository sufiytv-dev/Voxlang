// ui.rs - Global flag for UI thread state used by the diagnostic system.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static UI_RUNNING_FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// Store the atomic flag that indicates whether the UI thread should keep running.
/// Called by `diagnostic::spawn_ui_thread()` when starting the UI thread.
pub fn set_ui_running_flag(flag: Arc<AtomicBool>) {
    let _ = UI_RUNNING_FLAG.set(flag);
}

/// Signal the UI thread to stop by setting the stored flag to false.
/// If the flag was never set, this function does nothing.
pub fn stop_ui() {
    if let Some(flag) = UI_RUNNING_FLAG.get() {
        flag.store(false, Ordering::Relaxed);
    }
}
