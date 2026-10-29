//! The native taskbar child is paired with an Explorer XAML layout reservation.
//! HWND positioning alone cannot resize Windows 11's XAML application labels.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use crate::monitor::{MonitorInfo, Rect, TaskbarInfo};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Shared {
    magic: u32,
    version: u32,
    owner_pid: u32,
    enabled: u32,
    taskbar_hwnd: u64,
    width_px: u32,
    generation: u32,
    heartbeat_ms: u64,
    applied_generation: u32,
    status: i32,
    before_width: f32,
    after_width: f32,
    slot_left: i32,
    slot_top: i32,
    slot_right: i32,
    slot_bottom: i32,
    side: u32,
    reserved: u32,
}

const _: () = assert!(std::mem::size_of::<Shared>() == 80);

struct Mapping {
    file: isize,
    view: usize,
    injected_pid: u32,
    next_attempt: Option<Instant>,
    last_status: (u32, i32),
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view as *mut _,
            });
            let _ = CloseHandle(HANDLE(self.file as *mut _));
        }
    }
}

static MAPPING: Mutex<Option<Mapping>> = Mutex::new(None);
static BINARIES: OnceLock<Option<(std::path::PathBuf, std::path::PathBuf)>> = OnceLock::new();

unsafe fn control_word(address: *const u32) -> u32 {
    (&*address.cast::<AtomicU32>()).load(Ordering::Acquire)
}

unsafe fn publish_word(address: *mut u32, value: u32) {
    (&*address.cast::<AtomicU32>()).store(value, Ordering::Release);
}

fn acknowledged_slot(
    reply: &Shared,
    generation: u32,
    bar: &TaskbarInfo,
    width: i32,
    gap: i32,
    buttons: Option<crate::taskbar_layout::Cluster>,
) -> Option<Rect> {
    if reply.status != 1
        || reply.applied_generation != generation
        || !reply.before_width.is_finite()
        || !reply.after_width.is_finite()
        || reply.after_width < 16.0
        || reply.before_width - reply.after_width < reply.width_px as f32 - 2.0
    {
        return None;
    }
    let slot = Rect {
        left: reply.slot_left,
        top: reply.slot_top,
        right: reply.slot_right,
        bottom: reply.slot_bottom,
    };
    if slot.width() < width + 2 * gap - 1
        || slot.height() <= 0
        || slot.left < bar.rect.left
        || slot.right > bar.tray_left
        || slot.top < bar.rect.top
        || slot.bottom > bar.rect.bottom
    {
        return None;
    }
    let right = slot.right - gap;
    let rect = Rect {
        left: right - width,
        right,
        top: slot.top,
        bottom: slot.bottom,
    };
    if let Some(buttons) = buttons {
        if buttons.left < rect.right + gap && buttons.right > rect.left - gap {
            return None;
        }
    } else {
        return None;
    }
    Some(rect)
}

fn binaries() -> Option<&'static (std::path::PathBuf, std::path::PathBuf)> {
    BINARIES
        .get_or_init(|| {
            let folder = crate::settings::data_dir().join("native").join(format!(
                "{}-{}",
                env!("CARGO_PKG_VERSION"),
                std::process::id()
            ));
            std::fs::create_dir_all(&folder).ok()?;
            let dll = folder.join("codexcapper-taskbar-bridge.dll");
            let injector = folder.join("codexcapper-taskbar-injector.exe");
            std::fs::write(
                &dll,
                include_bytes!(concat!(env!("OUT_DIR"), "/codexcapper-taskbar-bridge.dll")),
            )
            .ok()?;
            std::fs::write(
                &injector,
                include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/codexcapper-taskbar-injector.exe"
                )),
            )
            .ok()?;
            Some((dll, injector))
        })
        .as_ref()
}

fn open() -> windows::core::Result<Mapping> {
    unsafe {
        let file = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            std::mem::size_of::<Shared>() as u32,
            w!("Local\\CodexCapper.TaskbarReservation.v2"),
        )?;
        let view = MapViewOfFile(
            file,
            FILE_MAP_ALL_ACCESS,
            0,
            0,
            std::mem::size_of::<Shared>(),
        );
        if view.Value.is_null() {
            let _ = CloseHandle(file);
            return Err(windows::core::Error::from_thread());
        }
        let mapping = Mapping {
            file: file.0 as isize,
            view: view.Value as usize,
            injected_pid: 0,
            next_attempt: None,
            last_status: (0, 0),
        };
        // Reply fields belong to the TAP, so do not overwrite them when it is
        // still restoring a previous process's reservation.
        let shared = mapping.view as *mut Shared;
        publish_word(&mut (*shared).enabled, 0);
        publish_word(
            &mut (*shared).generation,
            (*shared).generation.wrapping_add(1).max(1),
        );
        std::ptr::write_volatile(&mut (*shared).magic, 0x43435052);
        std::ptr::write_volatile(&mut (*shared).version, 1);
        std::ptr::write_volatile(&mut (*shared).owner_pid, std::process::id());
        Ok(mapping)
    }
}

/// An acknowledged XAML reservation outranks free-space placement. Pending or
/// rejected requests fall back to the existing measured-gap implementation.
pub fn request(
    bar: &TaskbarInfo,
    monitor: &MonitorInfo,
    compact: bool,
    gap: i32,
    left: bool,
) -> Option<Rect> {
    let width =
        (crate::monitor::taskbar_width_logical(1, compact) * monitor.scale()).round() as i32;
    let gap = (gap.max(0) as f64 * monitor.scale()).round() as i32;
    let mut guard = MAPPING.lock();
    if guard.is_none() {
        match open() {
            Ok(mapping) => *guard = Some(mapping),
            Err(error) => {
                tracing::warn!("taskbar layout IPC unavailable: {error}");
                return None;
            }
        }
    }
    let mapping = guard.as_mut()?;
    unsafe {
        let shared = mapping.view as *mut Shared;
        let changed = (*shared).taskbar_hwnd != bar.taskbar_hwnd as u64
            || (*shared).width_px != (width + 2 * gap) as u32
            || (*shared).side != u32::from(left)
            || (*shared).enabled == 0;
        if changed {
            publish_word(&mut (*shared).enabled, 0);
            std::ptr::write_volatile(&mut (*shared).taskbar_hwnd, bar.taskbar_hwnd as u64);
            std::ptr::write_volatile(&mut (*shared).width_px, (width + 2 * gap) as u32);
            std::ptr::write_volatile(&mut (*shared).side, u32::from(left));
            publish_word(
                &mut (*shared).generation,
                (*shared).generation.wrapping_add(1).max(1),
            );
        }
        std::ptr::write_volatile(&mut (*shared).heartbeat_ms, GetTickCount64());
        publish_word(&mut (*shared).enabled, 1);
        let mut explorer_pid = 0;
        GetWindowThreadProcessId(HWND(bar.taskbar_hwnd as *mut _), Some(&mut explorer_pid));
        if mapping.injected_pid != 0 && mapping.injected_pid != explorer_pid {
            mapping.injected_pid = 0;
            mapping.next_attempt = None;
        }
        if explorer_pid != 0
            && mapping.injected_pid != explorer_pid
            && mapping
                .next_attempt
                .map_or(true, |time| time.elapsed() >= Duration::from_secs(60))
        {
            mapping.next_attempt = Some(Instant::now());
            if let Some((dll, injector)) = binaries() {
                mapping.injected_pid = explorer_pid;
                let (dll, injector) = (dll.clone(), injector.clone());
                std::thread::spawn(move || {
                    let result = crate::providers::quiet_command(&injector.to_string_lossy())
                        .args([
                            "--pid",
                            &explorer_pid.to_string(),
                            "--dll",
                            &dll.to_string_lossy(),
                        ])
                        .output();
                    match result {
                        Ok(output) if output.status.success() => tracing::info!(
                            "taskbar XAML bridge attached to Explorer {explorer_pid}"
                        ),
                        result => {
                            tracing::warn!(
                                "taskbar XAML bridge unavailable, keeping gap fallback: {result:?}"
                            );
                            if let Some(mapping) = MAPPING.lock().as_mut() {
                                mapping.injected_pid = 0;
                            }
                        }
                    }
                });
            }
        }
        let generation = control_word(&(*shared).generation);
        let acknowledged = control_word(&(*shared).applied_generation);
        if acknowledged != generation || control_word((&(*shared).status as *const i32).cast()) != 1
        {
            return None;
        }
        let reply = std::ptr::read_volatile(shared);
        if acknowledged != control_word(&(*shared).applied_generation)
            || generation != control_word(&(*shared).generation)
            || control_word((&(*shared).status as *const i32).cast()) != 1
        {
            return None;
        }
        acknowledged_slot(
            &reply,
            generation,
            bar,
            width,
            gap,
            crate::taskbar_layout::cluster(bar.taskbar_hwnd),
        )
    }
}

/// Refresh the lease independently of layout changes; an abandoned process
/// cannot leave the application labels permanently shortened.
pub fn heartbeat() -> bool {
    let mut guard = MAPPING.lock();
    let Some(mapping) = guard.as_mut() else {
        return false;
    };
    unsafe {
        let shared = mapping.view as *mut Shared;
        if (*shared).enabled != 0 {
            std::ptr::write_volatile(&mut (*shared).heartbeat_ms, GetTickCount64());
        }
        let state = (
            std::ptr::read_volatile(&(*shared).applied_generation),
            std::ptr::read_volatile(&(*shared).status),
        );
        if state == mapping.last_status {
            return (*shared).enabled != 0
                && mapping.injected_pid == 0
                && mapping
                    .next_attempt
                    .map_or(true, |time| time.elapsed() >= Duration::from_secs(60));
        }
        mapping.last_status = state;
        tracing::info!(
            "taskbar reservation: generation={}, status={}, XAML width {} -> {}",
            state.0,
            state.1,
            (*shared).before_width,
            (*shared).after_width
        );
        true
    }
}

pub fn release() {
    if let Some(mapping) = MAPPING.lock().as_mut() {
        unsafe {
            publish_word(&mut (*(mapping.view as *mut Shared)).enabled, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Shared, TaskbarInfo, crate::taskbar_layout::Cluster) {
        let shared = Shared {
            status: 1,
            generation: 7,
            applied_generation: 7,
            width_px: 195,
            before_width: 1000.0,
            after_width: 805.0,
            slot_left: 705,
            slot_top: 0,
            slot_right: 900,
            slot_bottom: 60,
            ..Shared::default()
        };
        let bar = TaskbarInfo {
            rect: Rect::from_size(0, 0, 1000, 60),
            tray_left: 900,
            start_right: 0,
            horizontal: true,
            auto_hide: false,
            on_screen: true,
            taskbar_hwnd: 1,
            task_items_left: Some(100),
            task_items_right: 690,
        };
        (
            shared,
            bar,
            crate::taskbar_layout::Cluster {
                left: 100,
                right: 690,
            },
        )
    }
    #[test]
    fn reservation_requires_measured_capacity_reduction() {
        let (mut shared, bar, buttons) = fixture();
        assert!(acknowledged_slot(&shared, 7, &bar, 175, 10, Some(buttons)).is_some());
        shared.after_width = shared.before_width;
        assert!(acknowledged_slot(&shared, 7, &bar, 175, 10, Some(buttons)).is_none());
        shared.after_width = f32::NAN;
        assert!(acknowledged_slot(&shared, 7, &bar, 175, 10, Some(buttons)).is_none());
    }
    #[test]
    fn stale_or_overlapping_reservation_acks_are_rejected() {
        let (shared, bar, buttons) = fixture();
        assert!(acknowledged_slot(&shared, 8, &bar, 175, 10, Some(buttons)).is_none());
        assert!(acknowledged_slot(&shared, 7, &bar, 175, 10, None).is_none());
        assert!(acknowledged_slot(
            &shared,
            7,
            &bar,
            175,
            10,
            Some(crate::taskbar_layout::Cluster {
                left: 100,
                right: 850
            })
        )
        .is_none());
        let bad = Shared {
            slot_right: 1050,
            ..shared
        };
        assert!(acknowledged_slot(&bad, 7, &bar, 175, 10, Some(buttons)).is_none());
    }
    #[test]
    fn reservation_can_use_a_verified_left_slot() {
        let (shared, bar, _) = fixture();
        let left = Shared {
            slot_left: 0,
            slot_right: 195,
            side: 1,
            ..shared
        };
        let slot = acknowledged_slot(
            &left,
            7,
            &bar,
            175,
            10,
            Some(crate::taskbar_layout::Cluster {
                left: 220,
                right: 800,
            }),
        )
        .unwrap();
        assert_eq!((slot.left, slot.right), (10, 185));
    }
}
