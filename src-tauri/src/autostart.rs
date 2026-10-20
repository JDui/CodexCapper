//! Start with the session. The per user Run key on Windows, an XDG autostart
//! entry on Linux. Neither needs elevation, and the plugin picks the right one.

use tauri::AppHandle;

pub fn apply(app: &AppHandle, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();

    // Removing an entry that was never created reports a missing file, so the
    // current state is checked first.
    if manager.is_enabled().unwrap_or(false) == enabled {
        return;
    }
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(e) = result {
        tracing::error!("autostart could not be updated: {e}");
    }
}

pub fn sync(app: &AppHandle, enabled: bool) {
    shed_old_name();
    apply(app, enabled);
}

/// The software used to be called FluxDock, and so was the Run key it started
/// itself from. That entry points at an executable which no longer exists, so
/// once per session it goes away and no boot goes looking for the old name.
#[cfg(windows)]
fn shed_old_name() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    };

    const RUN: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN, Some(0), KEY_SET_VALUE, &mut key).is_ok() {
            let _ = RegDeleteValueW(key, w!("FluxDock"));
            let _ = RegCloseKey(key);
        }
    }
}

#[cfg(not(windows))]
fn shed_old_name() {}
