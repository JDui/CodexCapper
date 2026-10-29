//! A native child of Explorer's taskbar. The WebView remains a top-level
//! window for floating mode; it is never reparented across DPI contexts.
//!
//! All native window creation/destruction runs on Tauri's UI thread. A hidden
//! message-only window outlives the taskbar child, so Explorer replacing its
//! window cannot take the recovery timer with it.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock};

use tauri::{AppHandle, Manager};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, GetWindowDpiAwarenessContext, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::{Source, WindowSnapshot};
use crate::monitor::Rect;
use crate::state::{AppState, Refresh};

const SYNC: u32 = WM_APP + 41;
static APP: OnceLock<AppHandle> = OnceLock::new();
static MANAGER: AtomicIsize = AtomicIsize::new(0);
static CHILD: AtomicIsize = AtomicIsize::new(0);
static FAILED_PARENT: AtomicIsize = AtomicIsize::new(0);
static ENGAGED: AtomicBool = AtomicBool::new(false);
static REQUEST: parking_lot::Mutex<Option<Placement>> = parking_lot::Mutex::new(None);

#[derive(Clone, Copy)]
struct Placement {
    parent: isize,
    rect: Rect,
    visible: bool,
}

fn handle(value: isize) -> HWND {
    HWND(value as *mut core::ffi::c_void)
}

pub fn init(app: &AppHandle) -> windows::core::Result<()> {
    let _ = APP.set(app.clone());
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let classes: [(windows::core::PCWSTR, WNDPROC); 2] = [
            (w!("CodexCapperTaskbarHost"), Some(child_proc)),
            (w!("CodexCapperTaskbarManager"), Some(manager_proc)),
        ];
        for (name, proc) in classes {
            let class = WNDCLASSW {
                style: CS_DBLCLKS,
                hInstance: instance,
                lpszClassName: name,
                lpfnWndProc: proc,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_thread());
            }
        }
        let manager = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("CodexCapperTaskbarManager"),
            w!("CodexCapper taskbar lifecycle"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )?;
        if SetTimer(Some(manager), 1, 1000, None) == 0 {
            let error = windows::core::Error::from_thread();
            let _ = DestroyWindow(manager);
            return Err(error);
        }
        MANAGER.store(manager.0 as isize, Ordering::SeqCst);
    }
    Ok(())
}

/// Queue placement without making an async provider or reconciler thread own
/// a native window. Failure leaves the existing visible fallback available.
pub fn dock(parent: isize, rect: Rect, visible: bool) -> bool {
    if MANAGER.load(Ordering::SeqCst) == 0 || FAILED_PARENT.load(Ordering::SeqCst) == parent {
        ENGAGED.store(false, Ordering::SeqCst);
        return false;
    }
    *REQUEST.lock() = Some(Placement {
        parent,
        rect,
        visible,
    });
    ENGAGED.store(true, Ordering::SeqCst);
    wake();
    true
}

fn wake() {
    let manager = MANAGER.load(Ordering::SeqCst);
    if manager != 0 {
        unsafe {
            let _ = PostMessageW(Some(handle(manager)), SYNC, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn refresh() {
    wake();
}

pub fn hide() {
    if let Some(request) = REQUEST.lock().as_mut() {
        request.visible = false;
    }
    wake();
}

pub fn disengage() {
    ENGAGED.store(false, Ordering::SeqCst);
    hide();
}

pub fn engaged() -> bool {
    ENGAGED.load(Ordering::SeqCst)
}

pub fn is_visible() -> bool {
    let child = CHILD.load(Ordering::SeqCst);
    child != 0 && unsafe { IsWindowVisible(handle(child)).as_bool() }
}

pub fn shutdown() {
    crate::taskbar_reservation::release();
    disengage();
    let manager = MANAGER.swap(0, Ordering::SeqCst);
    if manager != 0 {
        unsafe {
            let _ = PostMessageW(Some(handle(manager)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe fn destroy_child() {
    let child = CHILD.swap(0, Ordering::SeqCst);
    if child != 0 && IsWindow(Some(handle(child))).as_bool() {
        let _ = DestroyWindow(handle(child));
    }
}

unsafe fn apply() -> windows::core::Result<()> {
    let Some(request) = *REQUEST.lock() else {
        return Ok(());
    };
    let parent = handle(request.parent);
    let desired = APP
        .get()
        .map(crate::window::wants_widget_visible)
        .unwrap_or(false);
    if !engaged() || !request.visible || !desired {
        let child = CHILD.load(Ordering::SeqCst);
        if child != 0 {
            let _ = ShowWindow(handle(child), SW_HIDE);
        }
        return Ok(());
    }
    if !IsWindow(Some(parent)).as_bool() {
        destroy_child();
        return Ok(());
    }

    // Windows requires a child to share its parent's awareness context. Set
    // it only for this native call, then restore the WebView UI thread's mode.
    let previous = SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(parent));
    let result = (|| {
        let mut child = handle(CHILD.load(Ordering::SeqCst));
        if !IsWindow(Some(child)).as_bool() || GetParent(child).ok() != Some(parent) {
            destroy_child();
            child = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                w!("CodexCapperTaskbarHost"),
                w!("Codex 额度"),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                Some(HINSTANCE(GetModuleHandleW(None)?.0)),
                None,
            )?;
            if let Err(error) = SetLayeredWindowAttributes(child, COLORREF(0), 255, LWA_ALPHA) {
                let _ = DestroyWindow(child);
                return Err(error);
            }
            CHILD.store(child.0 as isize, Ordering::SeqCst);
            tracing::info!(
                "native taskbar child created: child={:#x}, parent={:#x}, WS_CHILD",
                child.0 as isize,
                request.parent
            );
        }
        let mut origin = POINT {
            x: request.rect.left,
            y: request.rect.top,
        };
        if !ScreenToClient(parent, &mut origin).as_bool() {
            return Err(windows::core::Error::from_thread());
        }
        SetWindowPos(
            child,
            Some(HWND_TOP),
            origin.x,
            origin.y,
            request.rect.width(),
            request.rect.height(),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )?;
        let click_through = APP
            .get()
            .map(|app| {
                app.state::<Arc<AppState>>()
                    .settings
                    .get()
                    .widget
                    .click_through
            })
            .unwrap_or(false);
        let old_style = GetWindowLongPtrW(child, GWL_EXSTYLE);
        let mask = WS_EX_TRANSPARENT.0 as isize;
        let style = if click_through {
            old_style | mask
        } else {
            old_style & !mask
        };
        if style != old_style {
            SetWindowLongPtrW(child, GWL_EXSTYLE, style);
        }
        let _ = InvalidateRect(Some(child), None, false);
        Ok(())
    })();
    if !previous.0.is_null() {
        SetThreadDpiAwarenessContext(previous);
    }
    result
}

unsafe extern "system" fn manager_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        SYNC => {
            if let Err(error) = apply() {
                if let Some(request) = *REQUEST.lock() {
                    FAILED_PARENT.store(request.parent, Ordering::SeqCst);
                }
                disengage();
                destroy_child();
                tracing::warn!("native taskbar child failed, using visible fallback: {error}");
                if let Some(app) = APP.get() {
                    crate::window::reposition(app);
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if let Some(app) = APP.get() {
                if !crate::window::wants_widget_visible(app)
                    || !app
                        .state::<Arc<AppState>>()
                        .settings
                        .get()
                        .widget
                        .taskbar_mode()
                {
                    crate::taskbar_reservation::release();
                }
                if crate::taskbar_reservation::heartbeat() {
                    crate::window::reposition(app);
                }
                if engaged() {
                    let child = handle(CHILD.load(Ordering::SeqCst));
                    // The manager survives destruction of the Explorer child.
                    // Re-resolve the taskbar instead of trusting its old HWND.
                    let parent = REQUEST.lock().map(|r| handle(r.parent));
                    if !IsWindow(Some(child)).as_bool()
                        || parent.map_or(true, |parent| {
                            !IsWindow(Some(parent)).as_bool()
                                || GetParent(child).ok() != Some(parent)
                        })
                    {
                        crate::window::reposition(app);
                    } else {
                        let _ = SetWindowPos(
                            child,
                            Some(HWND_TOP),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                        );
                        let _ = InvalidateRect(Some(child), None, false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = KillTimer(Some(hwnd), 1);
            destroy_child();
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

unsafe extern "system" fn child_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            let pass = APP
                .get()
                .map(|app| {
                    app.state::<Arc<AppState>>()
                        .settings
                        .get()
                        .widget
                        .click_through
                })
                .unwrap_or(false);
            LRESULT(if pass {
                HTTRANSPARENT as isize
            } else {
                HTCLIENT as isize
            })
        }
        WM_CONTEXTMENU => {
            if let Some(app) = APP.get() {
                if let Some(window) = crate::window::widget(app) {
                    crate::show_context_menu(app.clone(), window);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            if let Some(app) = APP.get() {
                app.state::<Arc<AppState>>().request(Refresh::Manual);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let _ = CHILD.compare_exchange(hwnd.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst);
            DefWindowProcW(hwnd, msg, w, l)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

/// One or two actual windows, including weekly-only accounts. Compact mode
/// selects the most consumed window without assuming either slot is present.
fn rows(
    payload: Option<&crate::model::UsagePayload>,
    compact: bool,
) -> Vec<(&str, &WindowSnapshot)> {
    let mut rows: Vec<_> = payload
        .into_iter()
        .flat_map(|p| &p.providers)
        .filter(|p| p.enabled)
        .flat_map(|p| [("5h", p.five_hour.as_ref()), ("7d", p.weekly.as_ref())])
        .filter_map(|(label, window)| window.map(|w| (w.label.as_deref().unwrap_or(label), w)))
        .collect();
    if compact && rows.len() > 1 {
        rows.sort_by(|a, b| b.1.utilization.total_cmp(&a.1.utilization));
        rows.truncate(1);
    }
    rows
}

fn reset_text(
    window: &WindowSnapshot,
    style: &str,
    now: chrono::DateTime<chrono::Utc>,
    compact: bool,
) -> String {
    let Some(reset) = window.resets_at else {
        return String::new();
    };
    let seconds = (reset - now).num_seconds();
    if seconds <= 0 {
        return "即将重置".into();
    }
    if style != "countdown" {
        return reset
            .with_timezone(&chrono::Local)
            .format(if compact { "%H:%M" } else { "%m/%d %H:%M" })
            .to_string();
    }
    let seconds = ((seconds + 59) / 60) * 60;
    if compact && seconds >= 86400 {
        format!("{}d {}h", seconds / 86400, seconds % 86400 / 3600)
    } else if seconds >= 86400 {
        format!(
            "{}d {}h {}m",
            seconds / 86400,
            seconds % 86400 / 3600,
            seconds % 3600 / 60
        )
    } else if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else {
        format!("{}m", (seconds + 59) / 60)
    }
}

unsafe fn fill(dc: HDC, rect: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    if !brush.0.is_null() {
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

unsafe fn text(dc: HDC, value: &str, mut rect: RECT, color: COLORREF) {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    SetTextColor(dc, color);
    DrawTextW(
        dc,
        &mut wide,
        &mut rect,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
}

fn rgb(value: u32) -> COLORREF {
    COLORREF((value & 0xff) << 16 | (value & 0xff00) | (value >> 16 & 0xff))
}

fn plan_name(plan: Option<&str>) -> String {
    match plan.unwrap_or("").to_ascii_lowercase().as_str() {
        "prolite" | "pro_lite" | "pro-lite" => "Pro Lite".into(),
        "pro" => "Pro".into(),
        "plus" => "Plus".into(),
        "business" => "Business".into(),
        "team" => "Team".into(),
        "enterprise" => "Enterprise".into(),
        "free" => "Free".into(),
        "demo" => "Demo".into(),
        "" => "Codex".into(),
        _ => plan.unwrap_or("Codex").to_string(),
    }
}

unsafe fn rounded(dc: HDC, rect: RECT, radius: i32, color: COLORREF) {
    let region = CreateRoundRectRgn(
        rect.left,
        rect.top,
        rect.right + 1,
        rect.bottom + 1,
        radius * 2,
        radius * 2,
    );
    let brush = CreateSolidBrush(color);
    let _ = FillRgn(dc, region, brush);
    let _ = DeleteObject(brush.into());
    let _ = DeleteObject(region.into());
}

unsafe fn gradient(dc: HDC, rect: RECT, radius: i32, start: u32, end: u32) {
    let width = rect.right - rect.left;
    if width <= 0 {
        return;
    }
    let saved = SaveDC(dc);
    let region = CreateRoundRectRgn(
        rect.left,
        rect.top,
        rect.right + 1,
        rect.bottom + 1,
        radius * 2,
        radius * 2,
    );
    SelectClipRgn(dc, Some(region));
    for x in 0..width {
        let mut color = 0;
        for shift in [0, 8, 16] {
            let a = (start >> shift & 255) as f64;
            let b = (end >> shift & 255) as f64;
            color |= ((a + (b - a) * x as f64 / (width - 1).max(1) as f64).round() as u32) << shift;
        }
        fill(
            dc,
            RECT {
                left: rect.left + x,
                right: rect.left + x + 1,
                ..rect
            },
            rgb(color),
        );
    }
    let _ = RestoreDC(dc, saved);
    let _ = DeleteObject(region.into());
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let target = BeginPaint(hwnd, &mut ps);
    if target.0.is_null() {
        return;
    }
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let dc = CreateCompatibleDC(Some(target));
    let bitmap = CreateCompatibleBitmap(target, rect.right.max(1), rect.bottom.max(1));
    if dc.0.is_null() || bitmap.0.is_null() {
        if !bitmap.0.is_null() {
            let _ = DeleteObject(bitmap.into());
        }
        if !dc.0.is_null() {
            let _ = DeleteDC(dc);
        }
        let _ = EndPaint(hwnd, &ps);
        return;
    }
    let previous_bitmap = SelectObject(dc, bitmap.into());
    if let Some(app) = APP.get() {
        let state = app.state::<Arc<AppState>>();
        let settings = state.settings.get();
        let dark = crate::theme::resolve(&settings.appearance.theme) == "dark";
        let background = rgb(if dark { 0x202020 } else { 0xf3f3f3 });
        let foreground = COLORREF(if dark { 0xf5f5f5 } else { 0x202020 });
        let muted = COLORREF(if dark { 0xaaaaaa } else { 0x666666 });
        fill(dc, rect, background);
        SetBkMode(dc, TRANSPARENT);
        let scale = GetDpiForWindow(hwnd).max(96) as f64 / 96.0;
        let px = |v: f64| (v * scale).round() as i32;
        rounded(
            dc,
            RECT {
                left: 1,
                top: 1,
                right: rect.right - 1,
                bottom: rect.bottom - 1,
            },
            px(8.0),
            rgb(if dark { 0x232323 } else { 0xfafafa }),
        );
        let font = CreateFontW(
            -px(10.0),
            0,
            0,
            0,
            500,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Microsoft YaHei UI"),
        );
        let old_font = SelectObject(dc, font.into());
        let payload = state.payload.read().clone();
        let rows = rows(payload.as_ref(), settings.widget.compact_mode);
        let now = chrono::Utc::now();
        if rows.is_empty() {
            let status = payload
                .as_ref()
                .and_then(|p| p.providers.iter().find(|p| p.enabled))
                .and_then(|p| p.detail.as_deref())
                .unwrap_or(if payload.is_none() {
                    "加载额度…"
                } else {
                    "暂无额度数据"
                });
            text(
                dc,
                status,
                RECT {
                    left: px(5.0),
                    ..rect
                },
                foreground,
            );
        } else {
            let header_height = if settings.widget.compact_mode {
                0
            } else {
                px(13.0)
            };
            let row_height =
                px(16.0).min((rect.bottom - header_height - px(2.0)).max(1) / rows.len() as i32);
            let top = (rect.bottom - header_height - row_height * rows.len() as i32) / 2;
            let provider = payload
                .as_ref()
                .and_then(|p| p.providers.iter().find(|p| p.enabled));
            if !settings.widget.compact_mode {
                text(
                    dc,
                    &plan_name(provider.and_then(|p| p.plan_type.as_deref())),
                    RECT {
                        left: px(10.0),
                        top,
                        right: px(130.0),
                        bottom: top + header_height,
                    },
                    foreground,
                );
            }
            if !settings.widget.compact_mode {
                text(
                    dc,
                    "剩余额度",
                    RECT {
                        left: rect.right - px(54.0),
                        top,
                        right: rect.right - px(8.0),
                        bottom: top + header_height,
                    },
                    muted,
                );
            }
            for (i, (label, window)) in rows.iter().enumerate() {
                let y = top + header_height + i as i32 * row_height;
                let line = |left: f64, right: i32| RECT {
                    left: px(left),
                    top: y,
                    right,
                    bottom: y + row_height,
                };
                let remaining = (100.0 - window.utilization).clamp(0.0, 100.0);
                let color = if remaining <= 10.0 {
                    rgb(0xef5350)
                } else if remaining <= 30.0 {
                    rgb(if dark { 0xf5b84e } else { 0xb07d22 })
                } else {
                    foreground
                };
                let compact = settings.widget.compact_mode;
                text(
                    dc,
                    label,
                    line(
                        if compact { 4.0 } else { 10.0 },
                        px(if compact { 20.0 } else { 32.0 }),
                    ),
                    muted,
                );
                let track = RECT {
                    left: px(if compact { 24.0 } else { 36.0 }),
                    right: px(if compact { 54.0 } else { 102.0 }),
                    top: y + row_height / 2 - px(3.0),
                    bottom: y + row_height / 2 + px(3.0),
                };
                rounded(
                    dc,
                    track,
                    px(3.0),
                    rgb(if dark { 0x383b42 } else { 0xe6e7eb }),
                );
                let (start, end) = if window.stale {
                    (0x888888, 0xaaaaaa)
                } else if *label == "7d" {
                    if dark {
                        (0x9691bd, 0xc5b5dc)
                    } else {
                        (0x77729c, 0xb3a5c9)
                    }
                } else if dark {
                    (0x6da5af, 0xa6c9bd)
                } else {
                    (0x467f88, 0x8bb6ad)
                };
                gradient(
                    dc,
                    RECT {
                        right: track.left
                            + ((track.right - track.left) as f32 * remaining / 100.0).round()
                                as i32,
                        ..track
                    },
                    px(3.0),
                    start,
                    end,
                );
                let estimate = if window.source == Source::Official {
                    ""
                } else {
                    "估"
                };
                text(
                    dc,
                    &format!("{remaining:.0}%{estimate}"),
                    line(
                        if compact { 58.0 } else { 110.0 },
                        px(if compact { 88.0 } else { 148.0 }),
                    ),
                    if remaining <= 30.0 { color } else { foreground },
                );
                text(
                    dc,
                    &reset_text(window, &settings.widget.reset_time_style, now, compact),
                    line(
                        if compact { 90.0 } else { 150.0 },
                        rect.right - px(if compact { 4.0 } else { 8.0 }),
                    ),
                    muted,
                );
            }
        }
        SelectObject(dc, old_font);
        let _ = DeleteObject(font.into());
    }
    let _ = BitBlt(
        target,
        0,
        0,
        rect.right,
        rect.bottom,
        Some(dc),
        0,
        0,
        SRCCOPY,
    );
    SelectObject(dc, previous_bitmap);
    let _ = DeleteObject(bitmap.into());
    let _ = DeleteDC(dc);
    let _ = EndPaint(hwnd, &ps);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderSnapshot, UsagePayload};

    #[test]
    fn weekly_only_is_visible_in_both_native_layouts() {
        let mut provider = ProviderSnapshot::empty("codex", "Codex");
        provider.weekly = Some(WindowSnapshot::official(37.0, None));
        let payload = UsagePayload {
            providers: vec![provider],
            generated_at: chrono::Utc::now(),
            onboarding: false,
        };
        for compact in [false, true] {
            let actual = rows(Some(&payload), compact);
            assert_eq!(actual.len(), 1);
            assert_eq!(actual[0].0, "7d");
            assert_eq!(actual[0].1.utilization, 37.0);
        }
    }

    #[test]
    fn compact_native_layout_selects_the_more_consumed_window() {
        let mut provider = ProviderSnapshot::empty("codex", "Codex");
        provider.five_hour = Some(WindowSnapshot::official(12.0, None));
        provider.weekly = Some(WindowSnapshot::official(91.0, None));
        let payload = UsagePayload {
            providers: vec![provider],
            generated_at: chrono::Utc::now(),
            onboarding: false,
        };
        assert_eq!(rows(Some(&payload), false).len(), 2);
        assert_eq!(rows(Some(&payload), true)[0].0, "7d");
        assert!(rows(None, false).is_empty());
    }
}
