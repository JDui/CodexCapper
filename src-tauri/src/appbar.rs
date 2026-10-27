//! The strip of desktop the widget reserves for itself while it is pinned.
//!
//! Pinned placement used to be one thing: a window laid over the taskbar. That
//! cannot work, however carefully it is positioned, because the shell does not
//! know the window is there. It lays out its own content — the task button list
//! above all — without leaving room for it, so a desktop with enough windows
//! open simply grows buttons underneath the widget and they are covered.
//!
//! An application desktop toolbar is the opposite arrangement. `ABM_NEW` tells
//! the shell a strip along an edge is spoken for; `ABM_QUERYPOS` asks what is
//! actually free, because other applications reserve strips too; `ABM_SETPOS`
//! commits to one. From then on the shell keeps that band clear of everything it
//! draws and shrinks the maximised area to match, so nothing arrives under the
//! widget from either side.
//!
//! Two costs come with it and are worth stating plainly. A work area is a single
//! rectangle that cannot have a notch cut in it, so a strip 169px wide reserves
//! the whole width of the display for its height — which is why
//! `monitor::appbar_plan` sizes the row to the content rather than borrowing the
//! taskbar's height. And the strip is beside the taskbar rather than inside its
//! row: nothing third party can be placed inside that row on the modern
//! taskbar, and the tools that fake it by parenting a window into
//! `Shell_TrayWnd` bind their input queue to Explorer's and fall over whenever
//! Explorer restarts.
//!
//! Nothing here hooks window messages. The shell posts `ABM_POSCHANGED` when the
//! strips move, but the reconciler in `window` already compares a signature of
//! the work areas and repositions on any change, which covers a taskbar moving,
//! a display appearing and another application docking a strip of its own — one
//! path instead of a window procedure of our own to get wrong.

use crate::monitor::{DockPlan, Rect};

#[cfg(windows)]
use crate::monitor::{intersect_area, DockEdge};
#[cfg(windows)]
use std::sync::atomic::Ordering;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, RECT};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    APPBARDATA, SHAppBarMessage, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABE_BOTTOM, ABE_TOP,
};

/// The window handle the shell currently holds a registration for, or zero.
///
/// `ABM_NEW` fails for a window that is already registered, so registering
/// happens once and is given back explicitly rather than being reissued on every
/// pass of the reconciler.
#[cfg(windows)]
static REGISTERED: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The strip the shell agreed to, kept only so that a change of it is worth a
/// log line and a repeat of it is not.
#[cfg(windows)]
static RESERVED: parking_lot::Mutex<Option<Rect>> = parking_lot::Mutex::new(None);

/// Ask the shell for the strip described by `plan`, and report the one it
/// actually gave back in physical pixels.
///
/// `keep_out` is the taskbar's own rectangle: an answer overlapping it would put
/// the widget back on top of the task buttons, which is the bug this module
/// exists to end. `bounds` is the display, because a strip the shell agrees to
/// and then places off screen is not a strip.
///
/// `None` means the shell refused, and the caller should fall back to the
/// overlay rather than leave the widget nowhere.
pub fn dock(hwnd: isize, plan: &DockPlan, keep_out: &Rect, bounds: &Rect) -> Option<Rect> {
    #[cfg(windows)]
    {
        if let Some(rect) = attempt(hwnd, plan, keep_out, bounds) {
            return Some(rect);
        }

        // Explorer forgets registrations. It does so across a restart, and the
        // work area it goes on reserving afterwards is built from what it has in
        // hand, so a half remembered one keeps a band clear for a window that is
        // no longer where the shell thinks it is. Dropping the registration and
        // making a fresh one is the recovery that does not involve restarting
        // this process.
        tracing::debug!("rebuilding the reservation for the docked strip");
        unregister(hwnd);
        attempt(hwnd, plan, keep_out, bounds)
    }

    #[cfg(not(windows))]
    {
        let _ = (hwnd, plan, keep_out, bounds);
        None
    }
}

/// Is this process holding a strip from the shell at the moment?
///
/// Placement code asks this before it bothers giving one back. A floating
/// widget never takes a strip in the first place, and should not have to resolve
/// a window handle and speak to Explorer four times a second to find that out.
pub fn holding() -> bool {
    #[cfg(windows)]
    {
        REGISTERED.load(Ordering::SeqCst) != 0
    }

    #[cfg(not(windows))]
    {
        false
    }
}

/// Hand the strip back to the shell.
///
/// A reservation outlives nothing on its own, so this runs before every hide and
/// on every pass that places the window somewhere else: with the widget off
/// screen the shell would keep a band of the desktop clear for something nobody
/// can see. Asking to give back a strip that was never taken is what a floating
/// widget does on every single pass, so that case is answered without a word to
/// the shell.
pub fn undock(hwnd: isize) {
    #[cfg(windows)]
    if REGISTERED.load(Ordering::SeqCst) != 0 {
        unregister(hwnd);
    }

    #[cfg(not(windows))]
    let _ = hwnd;
}

#[cfg(windows)]
fn attempt(hwnd: isize, plan: &DockPlan, keep_out: &Rect, bounds: &Rect) -> Option<Rect> {
    let edge = match plan.edge {
        DockEdge::Top => ABE_TOP,
        DockEdge::Bottom => ABE_BOTTOM,
    };

    unsafe {
        if REGISTERED.load(Ordering::SeqCst) != hwnd {
            let mut data = bar_data(hwnd);
            data.uEdge = edge;
            if SHAppBarMessage(ABM_NEW, &mut data) == 0 {
                tracing::warn!("the shell refused to register the docked strip");
                return None;
            }
            REGISTERED.store(hwnd, Ordering::SeqCst);
        }

        let mut query = bar_data(hwnd);
        query.uEdge = edge;
        query.rc = to_rect(plan.rect);
        if SHAppBarMessage(ABM_QUERYPOS, &mut query) == 0 {
            tracing::warn!("the shell would not say what is free for the docked strip");
            return None;
        }
        let offered = Rect::from(query.rc);
        if !acceptable(&offered, plan, keep_out, bounds) {
            tracing::warn!(
                "the shell offered {}x{} at {},{} for the docked strip, which is no use",
                offered.width(),
                offered.height(),
                offered.left,
                offered.top
            );
            return None;
        }

        let mut set = bar_data(hwnd);
        set.uEdge = edge;
        set.rc = to_rect(offered);
        if SHAppBarMessage(ABM_SETPOS, &mut set) == 0 {
            tracing::warn!("the shell would not reserve the docked strip");
            return None;
        }

        let repeated = RESERVED.lock().replace(offered).is_some_and(|last| last == offered);
        if !repeated {
            tracing::info!(
                "reserved a docked strip of {}x{} at {},{} along the {}",
                offered.width(),
                offered.height(),
                offered.left,
                offered.top,
                match plan.edge {
                    DockEdge::Top => "top edge",
                    DockEdge::Bottom => "bottom edge",
                }
            );
        }
        Some(offered)
    }
}

/// One `APPBARDATA`, at the size the shell expects to read.
///
/// `uCallbackMessage` stays zero on purpose: a message would want a window
/// procedure of our own on a window the toolkit owns, and the reconciler polls
/// for the topology changes that message would only have announced sooner.
#[cfg(windows)]
fn bar_data(hwnd: isize) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: HWND(hwnd as *mut std::ffi::c_void),
        ..Default::default()
    }
}

#[cfg(windows)]
fn to_rect(r: Rect) -> RECT {
    RECT {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Whether what the shell offered is something the widget can be drawn in.
///
/// The answer is otherwise authoritative: another application may already hold a
/// strip on that side, and the shell moving ours is it doing its job, so arguing
/// with a shift is a fight that can only be lost. What must not come back is a
/// rectangle with no area, one mostly off the display, one inside the taskbar —
/// the original bug arriving through the shell's own arithmetic — or one so much
/// smaller than the proposal that the widget would be clipped to nothing.
#[cfg(windows)]
fn acceptable(offered: &Rect, plan: &DockPlan, keep_out: &Rect, bounds: &Rect) -> bool {
    if offered.width() <= 0 || offered.height() <= 0 {
        return false;
    }
    if intersect_area(offered, keep_out) != 0 {
        return false;
    }
    let area = offered.width() as i64 * offered.height() as i64;
    if intersect_area(offered, bounds) * 2 < area {
        return false;
    }
    offered.width() * 2 >= plan.rect.width() && offered.height() * 2 >= plan.rect.height()
}

/// Tear the registration down, whether or not this process believes it holds
/// one.
///
/// The belief and the shell can disagree — that is the whole reason `dock`
/// retries — so `ABM_REMOVE` is sent regardless. On a window the shell has never
/// heard of it is a no-op.
#[cfg(windows)]
fn unregister(hwnd: isize) {
    REGISTERED.store(0, Ordering::SeqCst);
    unsafe {
        let mut data = bar_data(hwnd);
        SHAppBarMessage(ABM_REMOVE, &mut data);
    }
    *RESERVED.lock() = None;
}
