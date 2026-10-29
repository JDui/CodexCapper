//! Monitor and panel geometry.
//!
//! Placement is always derived, never restored from stored coordinates. What
//! gets saved is a persistent monitor identity plus a corner and an offset, so
//! a display that comes back on a different arrangement cannot strand the
//! window off screen.
//!
//! On Windows the identity comes from
//! `DISPLAYCONFIG_TARGET_DEVICE_NAME.monitorDevicePath`, which carries the
//! vendor and product codes along with the connector instance. Adapter LUIDs
//! are deliberately not part of it: they can change across reboots, which would
//! make the identity unstable.
//!
//! On Linux the same three facts come out of the EDID the kernel exposes under
//! `/sys/class/drm`, keyed by the connector the display is plugged into. The
//! shape of the string differs between the two platforms, which does not
//! matter: it is only ever compared with itself.
//!
//! Everything below the identity is arithmetic on a rectangle, and is shared.

use serde::Serialize;

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use windows::core::BOOL;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY,
};
#[cfg(windows)]
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_GETTASKBARPOS, ABS_AUTOHIDE, APPBARDATA,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

/// Not re-exported by the windows crate.
#[cfg(windows)]
const MONITORINFOF_PRIMARY: u32 = 1;

/// Floating window width in logical pixels. The row is left aligned: the grip,
/// a code label, a half-width bar, the percentage tight against the bar and a
/// bracketed reset time — and nothing beyond it.
pub const LOGICAL_W: f64 = 236.0;

/// Floating window height before the interface has measured itself. Two
/// providers with two windows each. It is deliberately not the tallest case:
/// the interface reports its real height within a frame of the first paint, and
/// a window that starts too tall shows a band of empty surface until it does.
pub const DEFAULT_LOGICAL_H: f64 = 88.0;

/// Height of a docked strip before the interface has measured itself: the two
/// rows a provider draws, their gap and the frame around them.
///
/// Pinned placement used to borrow the taskbar's height and let the content
/// float inside it. A reserved strip cannot afford that — the reservation costs
/// the whole width of the display, so a row taller than what is drawn in it is
/// paid for across the entire monitor.
pub const DEFAULT_PINNED_LOGICAL_H: f64 = 40.0;

/// Bounds for a measured height, so a broken measurement cannot produce a
/// window that is invisible or takes over the screen.
pub const MIN_LOGICAL_H: f64 = 24.0;
pub const MAX_LOGICAL_H: f64 = 400.0;
/// Compact placement shares one width; a sole quota gets a taller capsule.
pub const COMPACT_LOGICAL_W: f64 = 132.0;
pub const COMPACT_SINGLE_LOGICAL_H: f64 = 22.0;

const MARGIN_X: f64 = 12.0;
const MARGIN_Y: f64 = 8.0;

/// Width of one provider column when pinned to the taskbar. Exactly the
/// content: label, gap, bar, gap, percentage, gap, bracketed reset time — so
/// the time ends as far from the right edge as the label starts from the left.
const TASKBAR_COLUMN_W: f64 = 224.0;
const TASKBAR_COMPACT_COLUMN_W: f64 = COMPACT_LOGICAL_W - TASKBAR_PADDING;
const TASKBAR_COLUMN_GAP: f64 = 12.0;
const TASKBAR_PADDING: f64 = 12.0;

/// The strip is a guest in the notification area, so no column is drawn wider
/// than the row inside it. The extra once handed out while there were few
/// columns is gone: snug on both ends, left and right alike.
const TASKBAR_COLUMN_BONUS: [f64; 3] = [0.0, 0.0, 0.0];

/// A rectangle in physical pixels.
///
/// The field names are deliberately the same as `windows::Win32::Foundation::RECT`
/// so that the Win32 bodies that were written against it read the same after the
/// conversion at the boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn from_size(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }

    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }
}

#[cfg(windows)]
impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub stable_id: String,
    pub friendly_name: String,
    /// The name the platform gives the output. `\\.\DISPLAY1` on Windows, the
    /// connector name such as `DP-1` on Linux.
    pub gdi_name: String,
    pub primary: bool,
    #[serde(skip)]
    pub work: Rect,
    #[serde(skip)]
    pub bounds: Rect,
    /// Stored as dots per inch on both platforms so that one definition of
    /// scale serves the whole module. Linux reports a factor, which is
    /// multiplied back up.
    #[serde(skip)]
    pub dpi: u32,
}

impl MonitorInfo {
    pub fn scale(&self) -> f64 {
        self.dpi as f64 / 96.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The monitor the user picked is attached.
    Pinned,
    /// A monitor was picked but is not attached; the choice is kept.
    FallbackPrimary,
    /// Nothing was picked, so the primary display is followed.
    FollowPrimary,
}

#[cfg(windows)]
struct EnumCtx {
    out: Vec<(HMONITOR, MONITORINFOEXW)>,
}

#[cfg(windows)]
unsafe extern "system" fn enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumCtx);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
        ctx.out.push((hmon, info));
    }
    BOOL(1)
}

#[cfg(windows)]
pub fn enumerate() -> Vec<MonitorInfo> {
    let mut ctx = EnumCtx { out: Vec::new() };
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }

    let names = query_display_names();

    ctx.out
        .into_iter()
        .map(|(hmon, info)| {
            let gdi_name = wide_to_string(&info.szDevice);
            let (stable_id, friendly_name) = names
                .iter()
                .find(|n| n.gdi_name == gdi_name)
                .map(|n| (n.device_path.clone(), n.friendly_name.clone()))
                .unwrap_or_else(|| (gdi_name.clone(), gdi_name.clone()));

            let dpi = unsafe {
                let mut x = 96u32;
                let mut y = 96u32;
                let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
                x.max(48)
            };

            MonitorInfo {
                stable_id,
                friendly_name: if friendly_name.trim().is_empty() {
                    gdi_name.clone()
                } else {
                    friendly_name
                },
                gdi_name,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                work: info.monitorInfo.rcWork.into(),
                bounds: info.monitorInfo.rcMonitor.into(),
                dpi,
            }
        })
        .collect()
}

pub fn primary() -> Option<MonitorInfo> {
    let all = enumerate();
    all.iter()
        .find(|m| m.primary)
        .cloned()
        .or_else(|| all.first().cloned())
}

pub fn resolve(stable_id: Option<&str>) -> Option<(MonitorInfo, Resolution)> {
    let all = enumerate();
    if all.is_empty() {
        return None;
    }
    if let Some(id) = stable_id {
        if let Some(m) = all.iter().find(|m| m.stable_id == id) {
            return Some((m.clone(), Resolution::Pinned));
        }
    }
    let fallback = all
        .iter()
        .find(|m| m.primary)
        .cloned()
        .or_else(|| all.first().cloned())?;
    let how = if stable_id.is_some() {
        Resolution::FallbackPrimary
    } else {
        Resolution::FollowPrimary
    };
    Some((fallback, how))
}

/// Bottom right of the work area, in physical pixels.
///
/// The height is passed in rather than fixed, because what the widget shows
/// decides how tall it has to be: one provider needs half of what two do, and a
/// window sized for the larger case leaves the smaller one padded with dead
/// space at both ends.
pub fn target_position(
    monitor: &MonitorInfo,
    offset_x: i32,
    offset_y: i32,
    logical_h: f64,
    compact: bool,
) -> (i32, i32, i32, i32) {
    let scale = monitor.scale();
    let w = (if compact { COMPACT_LOGICAL_W } else { LOGICAL_W } * scale).round() as i32;
    let h = (logical_h.clamp(if compact { COMPACT_SINGLE_LOGICAL_H } else { MIN_LOGICAL_H }, MAX_LOGICAL_H) * scale).round() as i32;

    let mut work = monitor.work;

    // With an auto hiding taskbar the work area covers the whole display, so
    // the widget would sit on top of the reveal strip.
    if taskbar_autohide() {
        if let Some(bar) = taskbar_rect() {
            let bar_h = (bar.bottom - bar.top).abs();
            let bar_w = (bar.right - bar.left).abs();
            if bar_w >= bar_h {
                if bar.top >= (monitor.bounds.top + monitor.bounds.bottom) / 2 {
                    work.bottom -= bar_h;
                } else {
                    work.top += bar_h;
                }
            } else if bar.left >= (monitor.bounds.left + monitor.bounds.right) / 2 {
                work.right -= bar_w;
            } else {
                work.left += bar_w;
            }
        }
    }

    let mx = (MARGIN_X * scale).round() as i32;
    let my = (MARGIN_Y * scale).round() as i32;

    (
        work.right - w - mx - offset_x,
        work.bottom - h - my - offset_y,
        w,
        h,
    )
}

/// Guarantees the window keeps at least half of its area on a work area,
/// falling back to the primary corner when it would not.
pub fn clamp_or_primary(x: i32, y: i32, w: i32, h: i32, logical_h: f64, compact: bool) -> (i32, i32, i32, i32) {
    let area = (w as i64) * (h as i64);
    if area <= 0 {
        return (x, y, w, h);
    }
    let rect = Rect::from_size(x, y, w, h);
    let best = enumerate()
        .iter()
        .map(|m| intersect_area(&rect, &m.work))
        .max()
        .unwrap_or(0);

    if best * 2 >= area {
        return (x, y, w, h);
    }
    match primary() {
        Some(p) => target_position(&p, 0, 0, logical_h, compact),
        None => (x, y, w, h),
    }
}

/// Does this rectangle overlap the monitor's work area by at least half?
pub fn mostly_inside(monitor: &MonitorInfo, x: i32, y: i32, w: i32, h: i32) -> bool {
    let area = (w as i64) * (h as i64);
    if area <= 0 {
        return false;
    }
    let rect = Rect::from_size(x, y, w, h);
    intersect_area(&rect, &monitor.work) * 2 >= area
}

/// The monitor whose bounds contain this point, or nothing.
///
/// `monitor_at` cannot answer nothing: on Windows it asks `MonitorFromPoint`
/// with `MONITOR_DEFAULTTOPRIMARY`, which substitutes the primary display for a
/// point that is off the desktop. That is the right answer when the question is
/// "where should this window go" and the wrong one when it is "which display is
/// this on", because a gap in a non rectangular arrangement comes back as a
/// confident primary.
pub fn containing(monitors: &[MonitorInfo], x: i32, y: i32) -> Option<&MonitorInfo> {
    monitors.iter().find(|m| {
        x >= m.bounds.left && x < m.bounds.right && y >= m.bounds.top && y < m.bounds.bottom
    })
}

/// Overlap of two rectangles. `pub(crate)` because the one caller that has to
/// ask whether what the shell offered collides with something the shell owns
/// lives in another module, and it is the same question.
pub(crate) fn intersect_area(a: &Rect, b: &Rect) -> i64 {
    let l = a.left.max(b.left);
    let t = a.top.max(b.top);
    let r = a.right.min(b.right);
    let bo = a.bottom.min(b.bottom);
    if r <= l || bo <= t {
        0
    } else {
        (r - l) as i64 * (bo - t) as i64
    }
}

#[cfg(windows)]
pub fn monitor_at(x: i32, y: i32) -> Option<MonitorInfo> {
    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    unsafe {
        if !GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            return None;
        }
    }
    let gdi = wide_to_string(&info.szDevice);
    enumerate().into_iter().find(|m| m.gdi_name == gdi)
}

/// Taskbar geometry.
///
/// Native placement creates a child inside this window without moving
/// shell-owned controls. A reserved strip remains the fallback when the
/// measured buttons leave no room for the child.
#[derive(Debug, Clone, Copy)]
pub struct TaskbarInfo {
    pub rect: Rect,
    /// Left edge of the notification area, estimated when it cannot be found.
    pub tray_left: i32,
    /// Right edge of the start button when it can be found, otherwise the
    /// left edge of the strip. Docking at the start side parks the widget just
    /// after it rather than underneath it.
    pub start_right: i32,
    pub horizontal: bool,
    pub auto_hide: bool,
    /// False while an auto hiding taskbar is slid away.
    pub on_screen: bool,
    pub taskbar_hwnd: isize,
    /// Right edge of the task buttons. Measured from the shell where UI
    /// Automation will talk to us, estimated from the windows on the desktop
    /// where it will not; `task_items_left` says which of the two happened.
    pub task_items_right: i32,
    /// Left edge of the task buttons, when the shell was able to say. `None`
    /// means the right edge is an estimate, and an estimate is not something to
    /// lay the widget out beside.
    pub task_items_left: Option<i32>,
}

#[cfg(windows)]
struct TrayScan {
    monitor_bounds: Rect,
    found: Option<HWND>,
}

#[cfg(windows)]
unsafe extern "system" fn secondary_tray_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowRect};

    let ctx = &mut *(lparam.0 as *mut TrayScan);
    let mut class = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut class);
    if len > 0 && wide_to_string(&class[..len as usize]) == "Shell_SecondaryTrayWnd" {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok()
            && intersect_area(&Rect::from(rect), &ctx.monitor_bounds) > 0
        {
            ctx.found = Some(hwnd);
            return BOOL(0);
        }
    }
    BOOL(1)
}

#[cfg(windows)]
pub fn taskbar_for(monitor: &MonitorInfo) -> Option<TaskbarInfo> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW, GetWindowRect};

    unsafe {
        let mut chosen: Option<HWND> = None;

        if let Ok(hwnd) = FindWindowW(w!("Shell_TrayWnd"), None) {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok()
                && intersect_area(&Rect::from(rect), &monitor.bounds) > 0
            {
                chosen = Some(hwnd);
            }
        }

        if chosen.is_none() {
            let mut ctx = TrayScan {
                monitor_bounds: monitor.bounds,
                found: None,
            };
            let _ = EnumWindows(Some(secondary_tray_proc), LPARAM(&mut ctx as *mut _ as isize));
            chosen = ctx.found;
        }

        let hwnd = chosen?;
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }

        // The notification area on the primary strip, the clock on secondary
        // ones. If neither is found a reasonable margin is reserved instead.
        let tray_left = tray_left_of(hwnd).unwrap_or_else(|| rect.right - (200.0 * monitor.scale()).round() as i32);

        // The start button keeps an HWND of its own even on the XAML taskbar.
        // When it is absent, or centred rather than hugging the left edge,
        // the left end of the strip is free to use and this parks at rect.left.
        let start_right = FindWindowExW(Some(hwnd), None, w!("Start"), None)
            .ok()
            .and_then(|child| {
                let mut child_rect = RECT::default();
                GetWindowRect(child, &mut child_rect).ok()?;
                (child_rect.right > child_rect.left).then_some(child_rect.right)
            })
            .unwrap_or(rect.left);

        let visible_area = intersect_area(&Rect::from(rect), &monitor.bounds);
        let total = (width as i64) * (height as i64);

        // The shell knows exactly where its buttons are and will say so to an
        // accessibility client. Where it will not, counting the desktop's
        // windows is the fallback — good enough to keep the old overlay out of
        // the way, not good enough to lay the widget out beside.
        let (task_items_left, task_items_right) = match crate::taskbar_layout::cluster(hwnd.0 as isize) {
            Some(measured) => (Some(measured.left), measured.right),
            None => (
                None,
                detect_task_items_right(hwnd, monitor, start_right, tray_left),
            ),
        };

        Some(TaskbarInfo {
            rect: rect.into(),
            tray_left,
            start_right,
            horizontal: width >= height,
            auto_hide: taskbar_autohide(),
            on_screen: total > 0 && visible_area * 2 >= total,
            taskbar_hwnd: hwnd.0 as isize,
            task_items_right,
            task_items_left,
        })
    }
}

/// Left edge of the notification area on a strip, when the strip has one.
///
/// The primary strip keeps the tray in a `TrayNotifyWnd` child; a secondary one
/// carries a `ClockButton` instead. Either marks where the shell's own right
/// hand end of the strip begins, which is the wall the widget docks against and
/// the boundary past which its buttons are not the widget's to share.
#[cfg(windows)]
pub(crate) fn tray_left_of(taskbar: windows::Win32::Foundation::HWND) -> Option<i32> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowRect};

    unsafe {
        [w!("TrayNotifyWnd"), w!("ClockButton")]
            .into_iter()
            .find_map(|class| {
                let child = FindWindowExW(Some(taskbar), None, class, None).ok()?;
                let mut rect = RECT::default();
                GetWindowRect(child, &mut rect).ok()?;
                (rect.right > rect.left).then_some(rect.left)
            })
    }
}

/// Every taskbar strip on the desktop: the primary one and one per extra
/// monitor. Measured by whoever needs to know what is drawn inside them.
#[cfg(windows)]
pub(crate) fn taskbar_windows() -> Vec<windows::Win32::Foundation::HWND> {
    use windows::core::{w, BOOL};
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, FindWindowW, GetClassNameW};

    struct Scan {
        found: Vec<HWND>,
    }

    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Scan);
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class);
        if len > 0 && wide_to_string(&class[..len as usize]) == "Shell_SecondaryTrayWnd" {
            ctx.found.push(hwnd);
        }
        BOOL(1)
    }

    let mut strips = Vec::new();
    unsafe {
        if let Ok(primary) = FindWindowW(w!("Shell_TrayWnd"), None) {
            strips.push(primary);
        }
        let mut ctx = Scan { found: Vec::new() };
        let _ = EnumWindows(Some(each), LPARAM(&mut ctx as *mut _ as isize));
        strips.extend(ctx.found);
    }
    strips
}

#[cfg(windows)]
fn detect_task_items_right(
    tray_hwnd: windows::Win32::Foundation::HWND,
    monitor: &MonitorInfo,
    start_right: i32,
    tray_left: i32,
) -> i32 {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, FindWindowExW, GetWindowLongPtrW, GetWindowRect, IsWindowVisible,
        GWL_EXSTYLE, GWL_STYLE, WS_CHILD, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };
    use windows::core::w;

    // 1. 尝试直接获取经典 Win10 任务列表窗口
    unsafe {
        if let Ok(rebar) = FindWindowExW(Some(tray_hwnd), None, w!("ReBarWindow32"), None) {
            if let Ok(task_sw) = FindWindowExW(Some(rebar), None, w!("MSTaskSwWClass"), None) {
                if let Ok(task_list) = FindWindowExW(Some(task_sw), None, w!("MSTaskListWClass"), None) {
                    let mut r = RECT::default();
                    if GetWindowRect(task_list, &mut r).is_ok() && r.right > start_right {
                        return r.right.min(tray_left);
                    }
                }
            }
        }
    }

    // 2. 针对现代 Windows 11 XAML 任务栏：
    // 通过统计当前桌面在任务栏显示窗口项的应用数量，推算任务项占用的右边界
    struct WindowCountContext {
        monitor_bounds: Rect,
        count: usize,
    }

    unsafe extern "system" fn count_taskbar_windows(hwnd: windows::Win32::Foundation::HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut WindowCountContext);
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if (style & WS_CHILD.0) != 0 {
            return BOOL(1);
        }
        if (ex & WS_EX_TOOLWINDOW.0) != 0 && (ex & WS_EX_APPWINDOW.0) == 0 {
            return BOOL(1);
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() || r.right <= r.left || r.bottom <= r.top {
            return BOOL(1);
        }
        let cx = (r.left + r.right) / 2;
        let cy = (r.top + r.bottom) / 2;
        if cx >= ctx.monitor_bounds.left
            && cx < ctx.monitor_bounds.right
            && cy >= ctx.monitor_bounds.top
            && cy < ctx.monitor_bounds.bottom
        {
            ctx.count += 1;
        }
        BOOL(1)
    }

    let mut ctx = WindowCountContext {
        monitor_bounds: monitor.bounds,
        count: 0,
    };
    unsafe {
        let _ = EnumWindows(Some(count_taskbar_windows), LPARAM(&mut ctx as *mut _ as isize));
    }

    let scale = monitor.scale();
    let item_w = (140.0 * scale).round() as i32;
    let estimated = start_right + (ctx.count as i32) * item_w;
    estimated.min(tray_left).max(start_right)
}

/// Taskbar placement grows with the number of columns instead of reserving a
/// fixed block of the strip: every column is exactly the row it holds, snug at
/// both ends.
///
/// Compact mode once asked for the plain column while the others had extra
/// room; with the strip hugging its content now, both spellings are one.
pub fn taskbar_width_logical(columns: usize, compact: bool) -> f64 {
    let count = columns.max(1);
    let bonus = if compact {
        0.0
    } else {
        TASKBAR_COLUMN_BONUS.get(count - 1).copied().unwrap_or(0.0)
    };
    let n = count as f64;
    let column = if compact { TASKBAR_COMPACT_COLUMN_W } else { TASKBAR_COLUMN_W };
    TASKBAR_PADDING + n * (column + bonus) + (n - 1.0) * TASKBAR_COLUMN_GAP
}

/// Pinned into the strip, centred in its height. `left` picks the end: the
/// start button side, otherwise just left of the notification area. The
/// widget is an ordinary overlay window here, never an appbar, so it can sit at
/// either end without moving anything the shell owns. A vertical taskbar cannot
/// fit the widget, so the caller falls back to floating placement.
pub fn taskbar_position(
    monitor: &MonitorInfo,
    bar: &TaskbarInfo,
    tray_gap_logical: i32,
    columns: usize,
    compact: bool,
    left: bool,
) -> Option<(i32, i32, i32, i32)> {
    if !bar.horizontal {
        return None;
    }
    let scale = monitor.scale();
    let strip_h = bar.rect.bottom - bar.rect.top;
    let h = (strip_h - (4.0 * scale).round() as i32).max((24.0 * scale).round() as i32);
    let w = (taskbar_width_logical(columns, compact) * scale).round() as i32;
    let x = dock_x(monitor, bar, tray_gap_logical, w, left);

    // 智能避让：当靠右端且任务栏软件窗口项已向右延伸占用到浮窗区域时，
    // 自动上浮贴齐任务栏上边缘，把任务栏内的空间 100% 留给软件窗口项，绝不遮挡窗口 bar
    let y = if !left && bar.task_items_right > 0 && bar.task_items_right + (8.0 * scale).round() as i32 > x {
        let mid = (monitor.bounds.top + monitor.bounds.bottom) / 2;
        let on_bottom = (bar.rect.top + bar.rect.bottom) / 2 >= mid;
        if on_bottom {
            bar.rect.top - h - (2.0 * scale).round() as i32
        } else {
            bar.rect.bottom + (2.0 * scale).round() as i32
        }
    } else {
        bar.rect.top + (strip_h - h) / 2
    };

    Some((x, y, w, h))
}

/// Where along a horizontal strip the widget sits: the end it docks against,
/// with the kept gap measured to whatever the shell owns at that end.
///
/// Shared by the two pinned placements so that "靠右端" cannot mean one thing
/// when the widget is laid over the strip and another when it is docked beside
/// it.
fn dock_x(monitor: &MonitorInfo, bar: &TaskbarInfo, tray_gap_logical: i32, w: i32, left: bool) -> i32 {
    let scale = monitor.scale();
    let gap = (tray_gap_logical as f64 * scale).round() as i32;

    if !left {
        return bar.tray_left - w - gap;
    }

    // Park after the start button when it hugs the left edge; when it is
    // centred instead, the whole left end is empty and gets used as is.
    let after_start =
        if bar.start_right > bar.rect.left && bar.start_right - bar.rect.left < (320.0 * scale).round() as i32 {
            bar.start_right
        } else {
            bar.rect.left
        };
    (after_start + gap).min(bar.tray_left - gap - w).max(bar.rect.left)
}

/// Which edge of the display a reserved strip holds, spelled without the Win32
/// constant so the arithmetic stays testable off Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockEdge {
    Top,
    Bottom,
}

/// A strip to ask the shell for: the edge it claims and the rectangle to
/// propose against that edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockPlan {
    pub edge: DockEdge,
    pub rect: Rect,
}

/// The strip to ask the shell to reserve for the widget while it is pinned.
///
/// This is the pinned placement that can actually keep its ground. The overlay
/// `taskbar_position` produces is laid on top of the strip and the shell has no
/// idea it is there: open enough windows and the task button list grows into
/// the same pixels, ending up underneath the widget. A reserved strip is the
/// other way round — the shell is told the space is taken, so nothing it draws
/// is ever allowed into it, and maximised windows are pushed out of it too.
///
/// The strip sits on the far side of the taskbar, flush against it, and is as
/// tall as the content needs rather than a second full taskbar height. The
/// reservation costs the whole width of the display: a work area is one
/// rectangle and cannot have a notch cut into it, so every pixel of height is
/// paid for across the entire monitor.
///
/// The taskbar's own rectangle is the anchor, deliberately not the work area —
/// reserving this strip changes the work area, and feeding that back would walk
/// the strip a few pixels up the screen every time the placement was recomputed.
///
/// A vertical taskbar cannot carry the widget at either end, and a strip that
/// has slid away would take the widget with it, so both answer `None` and leave
/// the caller to the overlay or to floating placement.
pub fn appbar_plan(
    monitor: &MonitorInfo,
    bar: &TaskbarInfo,
    tray_gap_logical: i32,
    columns: usize,
    compact: bool,
    left: bool,
    height_logical: f64,
) -> Option<DockPlan> {
    if !bar.horizontal || !bar.on_screen {
        return None;
    }
    let scale = monitor.scale();
    let w = (taskbar_width_logical(columns, compact) * scale).round() as i32;
    let h = (height_logical.clamp(if compact { COMPACT_SINGLE_LOGICAL_H } else { MIN_LOGICAL_H }, MAX_LOGICAL_H) * scale).round() as i32;
    if w <= 0 || h <= 0 {
        return None;
    }

    // Whichever half of the display the strip lies in is the side it is
    // attached to, so the widget docks against the taskbar rather than across
    // the screen from it.
    let mid = (monitor.bounds.top + monitor.bounds.bottom) / 2;
    let on_bottom = (bar.rect.top + bar.rect.bottom) / 2 >= mid;
    let (edge, top) = if on_bottom {
        (DockEdge::Bottom, bar.rect.top - h)
    } else {
        (DockEdge::Top, bar.rect.bottom)
    };

    // The proposal is clamped into the display. The shell will not reserve a
    // strip hanging off the side of it, and a proposal it refuses looks exactly
    // like the whole feature being broken.
    let left = dock_x(monitor, bar, tray_gap_logical, w, left)
        .clamp(monitor.bounds.left, (monitor.bounds.right - w).max(monitor.bounds.left));

    Some(DockPlan {
        edge,
        rect: Rect {
            left,
            top,
            right: left + w,
            bottom: top + h,
        },
    })
}

/// The widget laid *inside* the strip, on the ground the buttons are not using.
///
/// This is pinned placement as it was always meant to look: the same row as the
/// task buttons, the strip's own height, at the end the user asked for. Nothing
/// is reserved, because nothing needs reserving — the buttons were measured
/// rather than guessed, so the ground taken here is ground the shell was not
/// using. Compare `appbar_plan`, which is what happens when they cannot be
/// measured: the widget leaves the row and asks the shell to keep a strip clear,
/// paying for it across the whole width of the display.
///
/// `left` picks the preferred end — the start of the strip or the notification
/// area — and each end is anchored to the wall it belongs to rather than to the
/// buttons, so the widget holds still while they grow. When they grow into the
/// preferred end it crosses to the other one. When neither can carry it the
/// answer is `None` and the caller reserves a strip: a widget jammed between the
/// buttons and the tray, or overlapping either, is the exact failure this
/// placement exists to avoid.
///
/// `None` also answers when the buttons could not be measured at all. The
/// estimate in `task_items_right` has been wrong in precisely the way that buries
/// the widget, so it is trusted to dodge an overlay and never to park something
/// inside the row.
pub fn inrow_plan(
    monitor: &MonitorInfo,
    bar: &TaskbarInfo,
    tray_gap_logical: i32,
    columns: usize,
    compact: bool,
    left: bool,
    height_logical: f64,
) -> Option<Rect> {
    if !bar.horizontal || !bar.on_screen {
        return None;
    }
    let items_left = bar.task_items_left?;

    let scale = monitor.scale();
    let gap = (tray_gap_logical as f64 * scale).round() as i32;
    let w = (taskbar_width_logical(columns, compact) * scale).round() as i32;
    let band_h = bar.rect.height();
    let content = (height_logical.clamp(if compact { COMPACT_SINGLE_LOGICAL_H } else { MIN_LOGICAL_H }, MAX_LOGICAL_H) * scale).round() as i32;
    // Never taller than the strip, so the widget reads as part of it; shorter
    // when the content is, so the strip stays clickable above and below.
    let h = band_h.min(content).max(1);
    if w <= 0 || band_h <= 0 {
        return None;
    }

    let at_start = bar.rect.left + gap;
    let at_tray = bar.tray_left - gap - w;
    let fits_start = at_start + w + gap <= items_left;
    let fits_tray = at_tray >= bar.task_items_right + gap;

    let x = match (left, fits_start, fits_tray) {
        (true, true, _) | (false, true, false) => at_start,
        (false, _, true) | (true, false, true) => at_tray,
        _ => return None,
    };
    let x = x.clamp(bar.rect.left, (bar.tray_left - w).max(bar.rect.left));
    let top = bar.rect.top + (band_h - h) / 2;

    Some(Rect {
        left: x,
        top,
        right: x + w,
        bottom: top + h,
    })
}

#[cfg(windows)]
fn taskbar_autohide() -> bool {
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        (SHAppBarMessage(ABM_GETSTATE, &mut data) as u32) & ABS_AUTOHIDE != 0
    }
}

#[cfg(windows)]
fn taskbar_rect() -> Option<Rect> {
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        if SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) == 0 {
            return None;
        }
        Some(data.rc.into())
    }
}

/// There is no shell panel to dock into on Linux. Both of these say so, which
/// is what makes `target_position` and `reposition` compile and behave without
/// a second version of either.
#[cfg(not(windows))]
fn taskbar_autohide() -> bool {
    false
}

#[cfg(not(windows))]
fn taskbar_rect() -> Option<Rect> {
    None
}

#[cfg(not(windows))]
pub fn taskbar_for(_monitor: &MonitorInfo) -> Option<TaskbarInfo> {
    None
}

/// Whether this desktop has a panel the widget can be pinned into. Only the
/// Windows taskbar is understood, so pinned placement is offered nowhere else.
pub fn supports_panel_docking() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(windows)]
struct DisplayName {
    gdi_name: String,
    friendly_name: String,
    device_path: String,
}

#[cfg(windows)]
fn query_display_names() -> Vec<DisplayName> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    unsafe {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            != ERROR_SUCCESS
        {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return Vec::new();
        }
        paths.truncate(path_count as usize);

        let mut out = Vec::new();
        for path in paths {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != ERROR_SUCCESS.0 as i32 {
                continue;
            }

            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            let has_target =
                DisplayConfigGetDeviceInfo(&mut target.header) == ERROR_SUCCESS.0 as i32;

            out.push(DisplayName {
                gdi_name: wide_to_string(&source.viewGdiDeviceName),
                friendly_name: if has_target {
                    wide_to_string(&target.monitorFriendlyDeviceName)
                } else {
                    String::new()
                },
                device_path: if has_target {
                    wide_to_string(&target.monitorDevicePath)
                } else {
                    String::new()
                },
            });
        }
        out
    }
}

/// Rebuilt from the raw pointer because Tauri returns the `HWND` type from its
/// own version of the windows crate, which can drift from the one used here.
#[cfg(windows)]
#[allow(clippy::unnecessary_cast)]
pub fn hwnd_of(window: &tauri::WebviewWindow) -> Option<HWND> {
    window.hwnd().ok().map(|h| HWND(h.0 as *mut c_void))
}

// ---------------------------------------------------------------- Linux side

/// Takes a copy of the display layout. On Windows this is unnecessary, because
/// `EnumDisplayMonitors` is safe to call from anywhere; on Linux the only
/// source is GDK by way of Tauri, and GDK belongs to the thread that owns the
/// main loop. The reconciler does not run there, so it reads a copy instead.
pub fn refresh() {
    #[cfg(not(windows))]
    linux::refresh();
}

/// Records the handle the Linux backend needs and takes the first copy of the
/// layout. Called once from setup, on the main thread. A no-op on Windows.
pub fn init(app: &tauri::AppHandle) {
    #[cfg(not(windows))]
    linux::init(app);
    #[cfg(windows)]
    let _ = app;
}

#[cfg(not(windows))]
pub fn enumerate() -> Vec<MonitorInfo> {
    linux::snapshot()
}

/// The display under a point, or the primary when the point is off every
/// screen. Windows answers this with `MonitorFromPoint`; here it is a search of
/// the same list everything else reads, which keeps the two consistent even
/// mid-hotplug.
#[cfg(not(windows))]
pub fn monitor_at(x: i32, y: i32) -> Option<MonitorInfo> {
    let all = enumerate();
    all.iter()
        .find(|m| {
            x >= m.bounds.left && x < m.bounds.right && y >= m.bounds.top && y < m.bounds.bottom
        })
        .cloned()
        .or_else(|| all.iter().find(|m| m.primary).cloned())
        .or_else(|| all.first().cloned())
}

#[cfg(not(windows))]
mod linux {
    use std::sync::OnceLock;

    use parking_lot::RwLock;
    use tauri::AppHandle;

    use super::{MonitorInfo, Rect};

    static APP: OnceLock<AppHandle> = OnceLock::new();
    static CACHE: RwLock<Vec<MonitorInfo>> = RwLock::new(Vec::new());

    pub fn init(app: &AppHandle) {
        let _ = APP.set(app.clone());
        // Setup runs on the main thread, so the first copy can be taken here
        // rather than posted, which matters: the very first `reposition` is
        // called before any posted work could have run.
        *CACHE.write() = read(app);
    }

    pub fn refresh() {
        let Some(app) = APP.get() else { return };
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let fresh = read(&handle);
            // An empty answer is a transient state during a mode set, not a
            // desk with no monitors on it. Keeping the previous list means a
            // hotplug cannot briefly strand the widget.
            if !fresh.is_empty() {
                *CACHE.write() = fresh;
            }
        });
    }

    pub fn snapshot() -> Vec<MonitorInfo> {
        CACHE.read().clone()
    }

    fn read(app: &AppHandle) -> Vec<MonitorInfo> {
        let primary_name = app
            .primary_monitor()
            .ok()
            .flatten()
            .and_then(|m| m.name().cloned());

        let connectors = super::connected_displays();

        app.available_monitors()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, monitor)| {
                let position = *monitor.position();
                let size = *monitor.size();
                let area = monitor.work_area();
                let name = monitor.name().cloned().unwrap_or_default();

                let display = super::match_connector(&connectors, &name);

                let stable_id = display.and_then(|d| d.identity.clone()).unwrap_or_else(|| {
                    if name.is_empty() {
                        format!("linux-monitor-{index}")
                    } else {
                        name.clone()
                    }
                });

                let friendly_name = display
                    .and_then(|d| d.friendly.clone())
                    .filter(|f| !f.trim().is_empty())
                    .unwrap_or_else(|| {
                        if name.is_empty() {
                            format!("Display {}", index + 1)
                        } else {
                            name.clone()
                        }
                    });

                MonitorInfo {
                    stable_id,
                    friendly_name,
                    gdi_name: name.clone(),
                    // Wayland refuses to say which output is primary, and the
                    // fallbacks in `primary` and `resolve` already cover a list
                    // where nothing claims it.
                    primary: primary_name.as_deref() == Some(name.as_str()),
                    work: Rect::from_size(
                        area.position.x,
                        area.position.y,
                        area.size.width as i32,
                        area.size.height as i32,
                    ),
                    bounds: Rect::from_size(
                        position.x,
                        position.y,
                        size.width as i32,
                        size.height as i32,
                    ),
                    // GTK 3 only ever reports whole numbers here, so a display
                    // running at 125% is indistinguishable from one at 100%.
                    dpi: ((monitor.scale_factor() * 96.0).round() as u32).max(48),
                }
            })
            .collect()
    }
}

/// One display the kernel says is plugged in.
#[derive(Debug, Clone, Default)]
pub struct ConnectedDisplay {
    /// Connector name as the kernel spells it, for example `HDMI-A-1`.
    pub connector: String,
    /// Vendor, product and serial, taken from the EDID.
    pub identity: Option<String>,
    /// The name printed on the monitor, when the EDID carries one.
    pub friendly: Option<String>,
}

/// Everything under `/sys/class/drm` that currently has a display on it.
#[cfg(not(windows))]
fn connected_displays() -> Vec<ConnectedDisplay> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            continue;
        };
        if status.trim() != "connected" {
            continue;
        }

        // The directory is named `card0-DP-1`; the connector is what follows
        // the card, and the card number is not stable across boots.
        let dir = entry.file_name().to_string_lossy().into_owned();
        let connector = match dir.split_once('-') {
            Some((_, rest)) => rest.to_string(),
            None => dir.clone(),
        };

        let edid = std::fs::read(path.join("edid")).unwrap_or_default();
        out.push(ConnectedDisplay {
            connector,
            identity: edid_identity(&edid),
            friendly: edid_display_name(&edid),
        });
    }
    out
}

#[cfg(windows)]
#[allow(dead_code)]
fn connected_displays() -> Vec<ConnectedDisplay> {
    Vec::new()
}

/// Pairs a connector the kernel reported with the name the window system uses.
///
/// The two do not always agree: the kernel says `HDMI-A-1` where XRandR says
/// `HDMI-1`, so an exact match is tried first and a normalised one second.
/// Anything left unmatched simply keeps the window system's own name, which is
/// still stable for as long as the cable stays where it is.
#[cfg_attr(windows, allow(dead_code))]
fn match_connector<'a>(
    displays: &'a [ConnectedDisplay],
    name: &str,
) -> Option<&'a ConnectedDisplay> {
    if name.is_empty() {
        return None;
    }
    displays.iter().find(|d| d.connector == name).or_else(|| {
        displays
            .iter()
            .find(|d| normalise_connector(&d.connector) == normalise_connector(name))
    })
}

/// Drops the connector-type suffix the kernel adds and nobody else does, so
/// `HDMI-A-1` and `HDMI-1` compare equal.
#[cfg_attr(windows, allow(dead_code))]
fn normalise_connector(name: &str) -> String {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() == 3 && parts[1].len() == 1 {
        format!("{}-{}", parts[0], parts[2]).to_ascii_uppercase()
    } else {
        name.to_ascii_uppercase()
    }
}

/// Vendor, product and serial out of an EDID block.
///
/// Bytes 8 and 9 hold three five bit letters, big endian, with 1 meaning A.
/// Bytes 10 and 11 are the product code and 12 to 15 the serial, both little
/// endian. Together they identify the panel rather than the port, which is what
/// makes the result survive a replug into a different socket.
/// Kept out of a `cfg` gate so that its tests run on both hosts. The EDID
/// layout is byte arithmetic and does not need a Linux machine to be wrong on.
#[cfg_attr(windows, allow(dead_code))]
fn edid_identity(edid: &[u8]) -> Option<String> {
    if edid.len() < 16 || edid[..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }
    let packed = u16::from_be_bytes([edid[8], edid[9]]);
    let letter = |shift: u32| -> char {
        let value = ((packed >> shift) & 0x1F) as u8;
        if (1..=26).contains(&value) {
            (b'A' + value - 1) as char
        } else {
            '?'
        }
    };
    let vendor: String = [letter(10), letter(5), letter(0)].iter().collect();
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);

    Some(format!("{vendor}{product:04X}-{serial:08X}"))
}

/// The name printed on the monitor, when one of the four descriptor blocks
/// carries it. Descriptors start at byte 54 and are eighteen bytes each; a
/// display name is tagged 0xFC and its text is padded with a newline.
#[cfg_attr(windows, allow(dead_code))]
fn edid_display_name(edid: &[u8]) -> Option<String> {
    if edid.len() < 126 {
        return None;
    }
    for block in 0..4 {
        let start = 54 + block * 18;
        let descriptor = &edid[start..start + 18];
        if descriptor[0..3] != [0, 0, 0] || descriptor[3] != 0xFC {
            continue;
        }
        let text: String = descriptor[5..18]
            .iter()
            .take_while(|&&b| b != 0x0A)
            .map(|&b| b as char)
            .collect();
        let text = text.trim().to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The desk this widget is actually developed on: one 2560x1440 landscape
    /// display at the origin with a reserved taskbar strip, and one portrait
    /// display to the left of it, offset vertically. The offset is the point:
    /// the two do not share a top edge, so there is a gap above and below the
    /// overlap that belongs to no display at all.
    pub(crate) fn desk() -> Vec<MonitorInfo> {
        vec![
            MonitorInfo {
                stable_id: "LG".into(),
                friendly_name: "LG ULTRAGEAR".into(),
                gdi_name: r"\\.\DISPLAY1".into(),
                primary: true,
                work: Rect::from_size(0, 0, 2560, 1392),
                bounds: Rect::from_size(0, 0, 2560, 1440),
                dpi: 96,
            },
            MonitorInfo {
                stable_id: "PORT".into(),
                friendly_name: "Portrait".into(),
                gdi_name: r"\\.\DISPLAY5".into(),
                primary: false,
                work: Rect::from_size(-1080, -441, 1080, 1920),
                bounds: Rect::from_size(-1080, -441, 1080, 1920),
                dpi: 96,
            },
        ]
    }

    /// `containing` is allowed to say nothing, which is the whole reason it
    /// exists next to `monitor_at`.
    #[test]
    fn a_point_in_the_gap_between_two_screens_belongs_to_neither() {
        let desk = desk();
        assert!(containing(&desk, 1000, -250).is_none());
        assert_eq!(
            containing(&desk, 100, 100).map(|m| m.stable_id.as_str()),
            Some("LG")
        );
        assert_eq!(
            containing(&desk, -500, 100).map(|m| m.stable_id.as_str()),
            Some("PORT")
        );
        // A shared edge belongs to exactly one display: right and bottom are
        // exclusive, so the pixel at x=0 is the landscape one's.
        assert_eq!(
            containing(&desk, 0, 700).map(|m| m.stable_id.as_str()),
            Some("LG")
        );
        assert!(containing(&desk, 2560, 700).is_none());
    }

    /// A minimal but structurally real EDID: the magic header, ACME as the
    /// vendor, product 0x1234, serial 0xDEADBEEF, and a display name
    /// descriptor in the first slot.
    fn sample_edid() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);

        // A=1, C=3, M=13, E=5 packed as three five bit fields, big endian.
        let packed: u16 = (1 << 10) | (3 << 5) | 13;
        edid[8..10].copy_from_slice(&packed.to_be_bytes());
        edid[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        edid[12..16].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        edid[54..57].copy_from_slice(&[0, 0, 0]);
        edid[57] = 0xFC;
        let name = b"Studio 27\x0A     ";
        edid[59..59 + name.len()].copy_from_slice(name);
        edid
    }

    #[test]
    fn an_edid_identifies_the_panel_rather_than_the_port() {
        assert_eq!(
            edid_identity(&sample_edid()).as_deref(),
            Some("ACM1234-DEADBEEF")
        );
        assert_eq!(
            edid_display_name(&sample_edid()).as_deref(),
            Some("Studio 27")
        );
    }

    /// A blank read is what an unpowered or unplugged display gives back, and
    /// it must not turn into an identity every such display would share.
    #[test]
    fn a_blank_edid_has_no_identity() {
        assert_eq!(edid_identity(&[]), None);
        assert_eq!(edid_identity(&[0u8; 128]), None);
        assert_eq!(edid_display_name(&[0u8; 128]), None);
    }

    #[test]
    fn the_kernel_and_the_window_system_spell_hdmi_differently() {
        let displays = vec![
            ConnectedDisplay {
                connector: "HDMI-A-1".into(),
                identity: Some("ACM1234-DEADBEEF".into()),
                friendly: Some("Studio 27".into()),
            },
            ConnectedDisplay {
                connector: "DP-2".into(),
                identity: Some("XYZ0001-00000001".into()),
                friendly: None,
            },
        ];

        assert_eq!(
            match_connector(&displays, "HDMI-1").and_then(|d| d.identity.clone()),
            Some("ACM1234-DEADBEEF".into())
        );
        assert_eq!(
            match_connector(&displays, "DP-2").and_then(|d| d.identity.clone()),
            Some("XYZ0001-00000001".into())
        );
        assert!(match_connector(&displays, "DP-9").is_none());
        assert!(match_connector(&displays, "").is_none());
    }

    /// Two rectangles that share an edge overlap by nothing, and a window is
    /// only on a screen if half of it is.
    #[test]
    fn touching_rectangles_do_not_overlap() {
        let a = Rect::from_size(0, 0, 100, 100);
        let b = Rect::from_size(100, 0, 100, 100);
        assert_eq!(intersect_area(&a, &b), 0);
        assert_eq!(intersect_area(&a, &Rect::from_size(50, 0, 100, 100)), 5000);
    }

    #[test]
    fn a_column_per_provider_widens_the_strip() {
        let one = taskbar_width_logical(1, true);
        let two = taskbar_width_logical(2, true);
        let three = taskbar_width_logical(3, true);
        assert!(three > two && two > one);
        assert_eq!(three - two, two - one);
        // A count of zero is still one column wide rather than negative.
        assert_eq!(taskbar_width_logical(0, true), one);
    }

    /// A single provider pinned to the taskbar gets exactly the strip its
    /// content fills: the reset time ends as far from the right edge as the
    /// label starts from the left one, and adding a provider adds one column.
    #[test]
    fn a_lone_provider_is_exactly_what_it_shows() {
        let alone = taskbar_width_logical(1, false);
        assert!(alone > taskbar_width_logical(1, true));
        assert_eq!(alone, TASKBAR_PADDING + TASKBAR_COLUMN_W);
        let pair = taskbar_width_logical(2, false);
        assert_eq!(pair - alone, TASKBAR_COLUMN_W + TASKBAR_COLUMN_GAP);
    }

    /// The strip is a guest in the notification area, so a widget showing
    /// everything must not be wider than the one that already fitted.
    #[test]
    fn a_full_strip_is_no_wider_than_it_has_always_been() {
        for columns in 3..=6 {
            assert!(taskbar_width_logical(columns, false) > taskbar_width_logical(columns, true));
        }
    }

    /// Compact mode is the small version and has to stay the size it was.
    #[test]
    fn compact_mode_keeps_the_strip_it_had() {
        for columns in 1..=4 {
            let n = columns.max(1) as f64;
            let plain =
                TASKBAR_PADDING + n * TASKBAR_COMPACT_COLUMN_W + (n - 1.0) * TASKBAR_COLUMN_GAP;
            assert_eq!(taskbar_width_logical(columns, true), plain);
        }
    }

    #[test]
    fn compact_capsule_has_no_extra_minimum_height_in_any_placement() {
        let monitor = lg();
        let bar = measured(600, 1200);
        for height in [22.0, 38.0] {
            assert_eq!(target_position(&monitor, 0, 0, height, true).3, height as i32);
            assert_eq!(target_position(&monitor, 0, 0, height, true).2, COMPACT_LOGICAL_W as i32);
            assert_eq!(appbar_plan(&monitor, &bar, 8, 1, true, false, height).unwrap().rect.height(), height as i32);
            assert_eq!(inrow_plan(&monitor, &bar, 8, 1, true, false, height).unwrap().height(), height as i32);
        }
        assert_eq!(target_position(&monitor, 0, 0, 18.0, false).3, MIN_LOGICAL_H as i32);
        assert_eq!(taskbar_width_logical(1, true), COMPACT_LOGICAL_W);
    }

    /// Two providers are wider than one and three than two, whichever mode is
    /// on. A strip that shrank when a provider was switched on would read as a
    /// bug however well the arithmetic behind it was meant.
    #[test]
    fn the_strip_never_narrows_when_a_provider_is_added() {
        for compact in [false, true] {
            for columns in 1..6 {
                assert!(
                    taskbar_width_logical(columns + 1, compact)
                        > taskbar_width_logical(columns, compact),
                    "{columns} to {} narrowed with compact={compact}",
                    columns + 1
                );
            }
        }
    }

    #[test]
    fn pinning_to_a_panel_is_a_windows_only_offer() {
        assert_eq!(supports_panel_docking(), cfg!(windows));
    }

    /// A bottom strip on the landscape display of `desk()`: 48px tall, the
    /// notification area starting 200px from the right edge, and a start button
    /// hugging the left. Unmeasured, as a strip is before the shell has been
    /// asked where its buttons are.
    fn strip() -> TaskbarInfo {
        TaskbarInfo {
            rect: Rect::from_size(0, 1392, 2560, 48),
            tray_left: 2360,
            start_right: 48,
            horizontal: true,
            auto_hide: false,
            on_screen: true,
            taskbar_hwnd: 0,
            task_items_right: 0,
            task_items_left: None,
        }
    }

    /// The same strip with its buttons measured, which is the only state in
    /// which laying the widget out beside them is allowed.
    fn measured(left: i32, right: i32) -> TaskbarInfo {
        TaskbarInfo {
            task_items_left: Some(left),
            task_items_right: right,
            ..strip()
        }
    }

    fn lg() -> MonitorInfo {
        desk().into_iter().find(|m| m.stable_id == "LG").unwrap()
    }

    /// The whole point of asking the shell for a strip: it is flush against the
    /// taskbar and not one pixel of it lies inside the taskbar, which is where
    /// the task button list lives.
    #[test]
    fn a_docked_strip_sits_against_the_taskbar_not_inside_it() {
        let plan = appbar_plan(&lg(), &strip(), 8, 1, false, false, 40.0).expect("plans");
        assert_eq!(plan.edge, DockEdge::Bottom);
        assert_eq!(plan.rect.bottom, 1392, "flush with the strip's inner edge");
        assert_eq!(plan.rect.height(), 40);
        assert_eq!(
            intersect_area(&plan.rect, &strip().rect),
            0,
            "the task buttons must never end up underneath the widget"
        );
    }

    /// A taskbar along the top edge gets the strip below it rather than one
    /// pushed off the top of the screen.
    #[test]
    fn a_strip_on_a_top_taskbar_hangs_below_it() {
        let bar = TaskbarInfo {
            rect: Rect::from_size(0, 0, 2560, 48),
            ..strip()
        };
        let plan = appbar_plan(&lg(), &bar, 8, 1, false, false, 40.0).expect("plans");
        assert_eq!(plan.edge, DockEdge::Top);
        assert_eq!(plan.rect.top, 48);
        assert_eq!(intersect_area(&plan.rect, &bar.rect), 0);
    }

    /// The content's height, not the taskbar's: a reservation costs the whole
    /// width of the display, so a row taller than what is drawn in it is paid
    /// for twice.
    #[test]
    fn the_strip_is_as_tall_as_the_content_rather_than_the_taskbar() {
        let bar = strip();
        let plan = appbar_plan(&lg(), &bar, 8, 2, false, false, 40.0).expect("plans");
        assert_eq!(plan.rect.height(), 40);
        assert!(plan.rect.height() < bar.rect.height());

        // A measurement that has not landed yet still gives a usable row, and a
        // broken one is clamped back into the range the window allows.
        let unmeasured = appbar_plan(&lg(), &bar, 8, 1, false, false, 0.0).expect("plans");
        assert_eq!(unmeasured.rect.height(), (MIN_LOGICAL_H) as i32);
        let absurd = appbar_plan(&lg(), &bar, 8, 1, false, false, 9000.0).expect("plans");
        assert_eq!(absurd.rect.height(), MAX_LOGICAL_H as i32);
    }

    /// Nothing is reserved for a strip that cannot carry the widget: a vertical
    /// taskbar, and one an auto hiding taskbar has slid off screen with.
    #[test]
    fn a_strip_that_cannot_carry_the_widget_docks_nothing() {
        let vertical = TaskbarInfo {
            rect: Rect::from_size(0, 0, 48, 1440),
            horizontal: false,
            ..strip()
        };
        assert!(appbar_plan(&lg(), &vertical, 8, 1, false, false, 40.0).is_none());

        let slid_away = TaskbarInfo {
            on_screen: false,
            ..strip()
        };
        assert!(appbar_plan(&lg(), &slid_away, 8, 1, false, false, 40.0).is_none());
    }

    /// A crowded notification area pushes the proposal off the left of the
    /// display. The shell would refuse that outright, and a refusal reads to
    /// everyone who sees the log as the feature being broken rather than as a
    /// layout that has run out of room.
    #[test]
    fn the_proposal_is_kept_inside_the_display() {
        let crowded = TaskbarInfo {
            tray_left: 200,
            ..strip()
        };
        let plan = appbar_plan(&lg(), &crowded, 8, 4, false, false, 40.0).expect("plans");
        assert!(plan.rect.left >= lg().bounds.left);
        assert!(plan.rect.right <= lg().bounds.right);
    }

    /// "靠左端" has to mean the same pixels whether the widget is laid over the
    /// strip or docked beside it, or the two placements would be two different
    /// settings wearing one label.
    #[test]
    fn docking_and_the_overlay_agree_on_which_end_of_the_strip() {
        for left in [false, true] {
            for columns in 1..=3 {
                let bar = strip();
                let overlay =
                    taskbar_position(&lg(), &bar, 8, columns, false, left).expect("positions");
                let docked = appbar_plan(&lg(), &bar, 8, columns, false, left, 40.0).expect("plans");
                assert_eq!(
                    docked.rect.left, overlay.0,
                    "columns={columns} left={left} landed on different ends"
                );
                assert_eq!(docked.rect.width(), overlay.2);
            }
        }
    }

    /// What the whole exercise is for: the widget in the same row as the
    /// buttons, in ground the shell is not drawing in, touching neither.
    #[test]
    fn the_widget_lies_inside_the_row_when_the_buttons_leave_room() {
        let bar = measured(600, 1200);
        let plan = inrow_plan(&lg(), &bar, 8, 1, true, false, 30.0).expect("fits");

        assert_eq!(plan.height(), (30.0 * lg().scale()).round() as i32);
        assert_eq!(
            plan.top,
            bar.rect.top + (bar.rect.height() - plan.height()) / 2,
            "centred in the row rather than jammed against one edge of it"
        );
        assert_eq!(plan.right, bar.tray_left - 8, "flush against the tray");
        assert!(
            plan.left >= bar.task_items_right + 8,
            "and clear of the buttons, which is the complaint this answers"
        );
        assert_eq!(
            plan.width(),
            (taskbar_width_logical(1, true) * lg().scale()).round() as i32
        );
    }

    /// Both ends have room, and the user's setting decides — not whichever
    /// happens to be wider.
    #[test]
    fn it_parks_at_the_end_the_user_asked_for() {
        let bar = measured(600, 1200);
        let at_start = inrow_plan(&lg(), &bar, 8, 1, true, true, 20.0).expect("fits");
        let at_tray = inrow_plan(&lg(), &bar, 8, 1, true, false, 20.0).expect("fits");
        assert_eq!(at_start.left, bar.rect.left + 8);
        assert!(at_tray.left > at_start.left);
    }

    /// Buttons grow, the preferred end runs out, and the widget crosses the
    /// strip rather than being drawn over them. Each end is anchored to its own
    /// wall, so nothing moves until an end actually stops fitting.
    #[test]
    fn it_crosses_to_the_other_end_when_the_buttons_grow_into_it() {
        let roomy = measured(600, 1200);
        let preferred = inrow_plan(&lg(), &roomy, 8, 1, true, false, 20.0).expect("fits");
        assert_eq!(preferred.right, roomy.tray_left - 8);

        let crowded = measured(600, 2300);
        let moved = inrow_plan(&lg(), &crowded, 8, 1, true, false, 20.0).expect("the other end");
        assert_eq!(moved.left, crowded.rect.left + 8, "now at the start of the strip");
        assert!(moved.right + 8 <= crowded.task_items_left.unwrap());
    }

    /// Nowhere left in the row: the caller reserves a strip instead. Guessing a
    /// spot and hoping is the behaviour that put buttons under the widget.
    #[test]
    fn it_gives_up_when_neither_end_can_carry_it() {
        let bar = measured(4, 2356);
        assert!(inrow_plan(&lg(), &bar, 8, 1, true, false, 20.0).is_none());
        assert!(inrow_plan(&lg(), &bar, 8, 1, true, true, 20.0).is_none());
    }

    /// The estimate from counting desktop windows is not a measurement, and the
    /// difference between the two is a taskbar full of buttons under the widget.
    #[test]
    fn an_unmeasured_strip_is_never_laid_out_beside() {
        assert!(inrow_plan(&lg(), &strip(), 8, 1, true, false, 20.0).is_none());
    }

    /// The strip's own height so it reads as part of the row, with the content
    /// height as the ceiling so a short widget leaves the row clickable, and a
    /// strip that slid away or runs vertically carrying nothing at all.
    #[test]
    fn it_is_the_rows_height_or_the_contents_whichever_is_less() {
        let bar = measured(600, 1200);
        let tall = inrow_plan(&lg(), &bar, 8, 1, true, false, 9000.0).expect("fits");
        assert_eq!(tall.height(), bar.rect.height(), "never taller than the row");
        assert_eq!(tall.top, bar.rect.top);

        let short = inrow_plan(&lg(), &bar, 8, 1, true, false, 20.0).expect("fits");
        assert!(short.height() < bar.rect.height());
        assert_eq!(short.top, bar.rect.top + (bar.rect.height() - short.height()) / 2);

        let vertical = TaskbarInfo {
            rect: Rect::from_size(0, 0, 48, 1440),
            horizontal: false,
            ..measured(600, 1200)
        };
        assert!(inrow_plan(&lg(), &vertical, 8, 1, true, false, 20.0).is_none());

        let slid_away = TaskbarInfo {
            on_screen: false,
            ..measured(600, 1200)
        };
        assert!(inrow_plan(&lg(), &slid_away, 8, 1, true, false, 20.0).is_none());
    }
}
