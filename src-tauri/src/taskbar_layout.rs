//! Where the strip's own content sits, measured from outside the shell.
//!
//! Pinned placement wants to live inside the taskbar strip, in the same row as
//! the task buttons, and that row belongs to Explorer. The buttons used to be a
//! window of their own — `MSTaskListWClass` — whose rectangle could simply be
//! read. On the modern taskbar they are drawn by XAML instead: the old window is
//! still there, still reporting a rectangle, and no longer visible, so the
//! numbers in it are a formality that has stopped describing the screen.
//! Counting windows and multiplying by an assumed button width is the other
//! answer, and it is wrong in precisely the case that matters: pinned apps,
//! grouped buttons and search boxes all cost pixels the count never adds up.
//! Laying the widget over a strip measured that way is how task buttons ended
//! up underneath it.
//!
//! UI Automation is what is left that actually knows. The shell publishes its
//! button list to accessibility clients and every button reports where it is in
//! screen coordinates, whether XAML or the older code drew it. This module asks,
//! on a thread of its own, and caches the answer.
//!
//! Two things are deliberately absent. Nothing is hooked or subclassed: the
//! shell is asked rather than intercepted, exactly as the reconciler compares
//! work areas instead of watching for them to change. And nothing outside this
//! module ever waits on the shell — a taskbar that has stopped answering leaves
//! the previous measurement standing, which is why callers read `None` as "do
//! not lay over the strip" rather than as "there is nothing in the strip".

use parking_lot::Mutex;

/// How far the buttons reach inside the strip: the leftmost and rightmost pixel
/// of anything the shell is using, in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster {
    pub left: i32,
    pub right: i32,
}

/// One entry per taskbar window, so a second monitor's strip is measured rather
/// than assumed to mirror the first.
static MEASURED: Mutex<Vec<(isize, Cluster)>> = Mutex::new(Vec::new());

/// How far the buttons move in a moment without anything having moved.
///
/// The tree behind the buttons is rebuilt by XAML, and a rebuild can report the
/// same button one pixel along. Nothing has changed for the user, but a number
/// that changes makes the reconciler move the window, and a number that changes
/// several times a second makes it move the window several times a second.
const JITTER: i32 = 4;

/// The answer to keep, given what was kept before.
///
/// A measurement that agrees with the held one to within [JITTER] is the same
/// measurement, and the held one is kept — its widening included, which is
/// already a little generous on both sides. Anything beyond that is a button
/// appearing, disappearing or being dragged, and is published at once: the
/// widget has to get out of the way of that, and it has its configured gap to
/// spare before it is in real danger.
fn settle(held: Option<&Cluster>, fresh: Cluster) -> Cluster {
    match held {
        Some(kept)
            if (fresh.left - kept.left).abs() <= JITTER
                && (fresh.right - kept.right).abs() <= JITTER => *kept,
        _ => widen(&fresh),
    }
}

/// The strip's buttons move in whole pixels but the tree behind them is rebuilt
/// by XAML, so a measurement can wobble by a pixel while nothing has moved.
/// Widening the cluster outwards to a four pixel grid keeps the numbers tidy
/// without ever reporting a button as free that is not.
fn widen(cluster: &Cluster) -> Cluster {
    Cluster {
        left: cluster.left & !3,
        right: (cluster.right + 3) & !3,
    }
}

/// Where the buttons are on a given strip, as of the last completed pass.
pub fn cluster(taskbar_hwnd: isize) -> Option<Cluster> {
    MEASURED
        .lock()
        .iter()
        .find(|(hwnd, _)| *hwnd == taskbar_hwnd)
        .map(|(_, found)| *found)
}

/// A failed query belongs to one strip. A successful query of another monitor
/// must not erase its last answer (Explorer stops answering UIA during Win+X).
fn update_measurements(
    previous: &[(isize, Cluster)],
    fresh: impl IntoIterator<Item = (isize, Option<Cluster>)>,
) -> Vec<(isize, Cluster)> {
    fresh
        .into_iter()
        .filter_map(|(hwnd, measured)| {
            let held = previous.iter().find(|(was, _)| *was == hwnd).map(|(_, c)| c);
            measured.map(|c| settle(held, c)).or(held.copied()).map(|c| (hwnd, c))
        })
        .collect()
}

/// Starts the thread that keeps the measurements current.
///
/// The reconciler cannot ask this question itself: a UI Automation call is a
/// round trip into Explorer, and an Explorer that has stopped answering would
/// take the placement of the widget down with it. So the question is asked
/// somewhere it is allowed to hang, and the reconciler only ever reads what was
/// last brought back.
pub fn spawn() {
    #[cfg(windows)]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static STARTED: AtomicBool = AtomicBool::new(false);
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("taskbar-layout".into())
            .spawn(survey);
    }
}

#[cfg(windows)]
fn survey() {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

    // UIA requires a dedicated MTA thread without windows or an STA message
    // pump. In particular, shell menus must not hold up our UI thread.
    if let Err(error) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        tracing::warn!("taskbar accessibility initialization failed: {error}");
        return;
    }

    let mut automation: Option<IUIAutomation> = None;
    loop {
        // Made on the spot rather than up front: the first attempt can fail for
        // reasons that do not last, and a client that gave up then would never
        // measure anything again.
        if automation.is_none() {
            automation = unsafe { CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.ok();
        }
        if let Some(client) = automation.as_ref() {
            refresh(client);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(windows)]
fn refresh(client: &windows::Win32::UI::Accessibility::IUIAutomation) {
    use crate::monitor::taskbar_windows;

    let previous = MEASURED.lock().clone();
    let found = update_measurements(
        &previous,
        taskbar_windows().into_iter().map(|hwnd| (hwnd.0 as isize, measure(client, hwnd))),
    );
    let mut held = MEASURED.lock();
    if *held != found {
        for (hwnd, cluster) in &found {
            let before = held.iter().find(|(was, _)| was == hwnd).map(|(_, c)| *c);
            if before != Some(*cluster) {
                // Debug, not info: a second monitor's cluster can wobble
                // several times a second on its own, and this is a number to
                // read when something is wrong, not an event to be told about.
                tracing::debug!(
                    "strip {hwnd:#x}: the buttons run from {} to {}",
                    cluster.left,
                    cluster.right
                );
            }
        }
        *held = found;
    }
}

/// The buttons on one strip, as the shell describes them.
#[cfg(windows)]
fn measure(client: &windows::Win32::UI::Accessibility::IUIAutomation, taskbar: windows::Win32::Foundation::HWND) -> Option<Cluster> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::System::Variant::{
        VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4,
    };
    use windows::Win32::UI::Accessibility::{
        UIA_ButtonControlTypeId, UIA_ControlTypePropertyId, TreeScope_Subtree,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut band = RECT::default();
    unsafe { GetWindowRect(taskbar, &mut band).ok()? };
    if band.right <= band.left || band.bottom <= band.top {
        return None;
    }

    // The notification area is the shell's own right hand end of the strip and
    // its buttons are nobody's business: the clock, the volume, the overflow
    // chevron. Only the ground between the start of the strip and the tray can
    // be shared, so only that ground is measured.
    let tray_left = crate::monitor::tray_left_of(taskbar).unwrap_or(band.right);

    // A VT_I4 VARIANT holding the button control type id, spelled out because
    // the generated type is the C union and offers no integer constructor.
    let wanted = VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: core::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 {
                    lVal: UIA_ButtonControlTypeId.0,
                },
            }),
        },
    };

    unsafe {
        let root = client.ElementFromHandle(taskbar).ok()?;
        let condition = client
            .CreatePropertyCondition(UIA_ControlTypePropertyId, &wanted)
            .ok()?;
        let buttons = root.FindAll(TreeScope_Subtree, &condition).ok()?;
        let count = buttons.Length().ok()?;

        let mut left = i32::MAX;
        let mut right = i32::MIN;
        for index in 0..count {
            let Ok(button) = buttons.GetElement(index) else {
                continue;
            };
            let Ok(rect) = button.CurrentBoundingRectangle() else {
                continue;
            };
            if rect.right <= rect.left || rect.bottom <= rect.top {
                continue;
            }
            // Off the strip entirely, or into the tray.
            if rect.top >= band.bottom || rect.bottom <= band.top {
                continue;
            }
            if rect.left >= tray_left - TRAY_EDGE {
                continue;
            }
            left = left.min(rect.left);
            right = right.max(rect.right);
        }

        (right > left).then_some(Cluster { left, right })
    }
}

/// How far left of the tray a button still counts as part of the strip's
/// content. The overflow chevron sits a pixel or two outside `TrayNotifyWnd`
/// and is not content the widget may crowd out.
#[cfg(windows)]
const TRAY_EDGE: i32 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measurement_is_widened_outwards_so_it_never_undersells_the_buttons() {
        let measured = Cluster { left: 1211, right: 3570 };
        let held = widen(&measured);
        assert_eq!(held.left, 1208);
        assert_eq!(held.right, 3572);
        assert!(held.left <= measured.left && held.right >= measured.right);
    }

    #[test]
    fn a_widened_measurement_is_stable_when_the_shell_wobbles_by_a_pixel() {
        let held = settle(None, Cluster { left: 1211, right: 3570 });
        assert_eq!(
            settle(Some(&held), Cluster { left: 1212, right: 3569 }),
            held,
            "a pixel of wobble must not reach the reconciler, which would move the window for it"
        );
        assert_eq!(
            settle(Some(&held), Cluster { left: 1208, right: 3573 }),
            held,
            "and neither must the rounding of a second measurement of the same buttons"
        );
    }

    #[test]
    fn real_growth_is_published_even_though_wobble_is_not() {
        let held = settle(None, Cluster { left: 1211, right: 3570 });
        // Another application pinned: far beyond any wobble, and the widget has
        // to be told, because it is the widget that is in the way.
        let grown = settle(Some(&held), Cluster { left: 1211, right: 3820 });
        assert!(grown.right > held.right);
    }

    #[test]
    fn an_unmeasured_strip_is_asked_about_rather_than_assumed_empty() {
        assert!(cluster(0).is_none());
    }

    #[test]
    fn a_shell_menu_on_one_monitor_does_not_discard_its_cached_layout() {
        let primary = Cluster { left: 1080, right: 4216 };
        let secondary = Cluster { left: 2228, right: 3376 };
        let previous = [(1, primary), (2, secondary)];
        let updated = update_measurements(&previous, [(1, None), (2, Some(secondary))]);
        assert_eq!(updated, previous);
        assert_eq!(update_measurements(&updated, [(1, None), (2, None)]), previous);
    }

    #[test]
    fn removed_strips_are_forgotten_and_new_strips_require_a_measurement() {
        let primary = Cluster { left: 1080, right: 4216 };
        let previous = [(1, primary), (2, primary)];
        assert_eq!(update_measurements(&previous, [(1, None), (3, None)]), vec![(1, primary)]);
        assert!(update_measurements(&previous, []).is_empty());
        let changed = Cluster { left: 1080, right: 4400 };
        assert_eq!(update_measurements(&previous, [(1, Some(changed))]), vec![(1, changed)]);
    }
}
