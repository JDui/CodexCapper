//! A native child of Explorer's taskbar. The WebView remains a top-level
//! window for floating mode; it is never reparented across DPI contexts.
//!
//! All native window creation/destruction runs on Tauri's UI thread. A hidden
//! message-only window outlives the taskbar child, so Explorer replacing its
//! window cannot take the recovery timer with it.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetWindowDpiAwarenessContext, SetThreadDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::model::WindowSnapshot;
use crate::monitor::Rect;
use crate::state::{AppState, Refresh};

const SYNC: u32 = WM_APP + 41;
static APP: OnceLock<AppHandle> = OnceLock::new();
static MANAGER: AtomicIsize = AtomicIsize::new(0);
static CHILD: AtomicIsize = AtomicIsize::new(0);
static HEALTH: parking_lot::Mutex<HostHealth> = parking_lot::Mutex::new(HostHealth::new());
static SYNC_PENDING: AtomicBool = AtomicBool::new(false);
static ENGAGED: AtomicBool = AtomicBool::new(false);
static ROW_COUNT: AtomicUsize = AtomicUsize::new(0);
static REQUEST: parking_lot::Mutex<Option<Placement>> = parking_lot::Mutex::new(None);

const RETRY_DELAY: Duration = Duration::from_secs(10);

struct HostHealth {
    parent: isize,
    failures: u8,
    retry_after: Option<Instant>,
}

impl HostHealth {
    const fn new() -> Self {
        Self { parent: 0, failures: 0, retry_after: None }
    }

    fn allow(&mut self, parent: isize, now: Instant) -> bool {
        if self.parent != parent || self.retry_after.is_some_and(|after| now >= after) {
            *self = Self::new();
            self.parent = parent;
        }
        self.retry_after.is_none()
    }

    fn failed(&mut self, parent: isize, now: Instant) -> bool {
        if self.parent != parent {
            *self = Self::new();
            self.parent = parent;
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= 3 {
            self.retry_after = Some(now + RETRY_DELAY);
            true
        } else {
            false
        }
    }

    fn retry_due(&self, now: Instant) -> bool {
        self.retry_after.is_some_and(|after| now >= after)
    }
}

thread_local! {
    static UI_BUSY: Cell<bool> = const { Cell::new(false) };
}

// COM may pump messages while a drawing factory is being initialized. Never
// reenter placement/drawing with a RefCell borrow or native surface in flight.
struct UiWork;

impl UiWork {
    fn enter() -> Option<Self> {
        UI_BUSY.with(|busy| if busy.replace(true) { None } else { Some(Self) })
    }
}

impl Drop for UiWork {
    fn drop(&mut self) {
        UI_BUSY.with(|busy| busy.set(false));
    }
}

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
    if MANAGER.load(Ordering::SeqCst) == 0 || !HEALTH.lock().allow(parent, Instant::now()) {
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
    if manager != 0 && !SYNC_PENDING.swap(true, Ordering::SeqCst) {
        unsafe {
            if PostMessageW(Some(handle(manager)), SYNC, WPARAM(0), LPARAM(0)).is_err() {
                SYNC_PENDING.store(false, Ordering::SeqCst);
            }
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
        let settings = APP
            .get()
            .map(|app| app.state::<Arc<AppState>>().settings.get());
        SetWindowPos(
            child,
            Some(HWND_TOP),
            origin.x,
            origin.y,
            request.rect.width(),
            request.rect.height(),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )?;
        let click_through = settings.is_some_and(|s| s.widget.click_through);
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
        if let Some(app) = APP.get() {
            crate::taskbar_render::render(child, app)?;
        }
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
            SYNC_PENDING.store(false, Ordering::SeqCst);
            let Some(_work) = UiWork::enter() else { return LRESULT(0) };
            if let Err(error) = apply() {
                let request = *REQUEST.lock();
                let fallback = request.is_some_and(|r| HEALTH.lock().failed(r.parent, Instant::now()));
                tracing::warn!("native taskbar update failed (fallback={fallback}): {error}");
                if fallback {
                    disengage();
                    destroy_child();
                    if let Some(app) = APP.get() {
                        crate::window::reposition(app);
                    }
                }
                // One transient graphics error leaves the last bitmap visible.
                // The independent timer retries; repeated failures use a timed
                // fallback rather than blacklisting this taskbar forever.
            } else if engaged() && is_visible() {
                *HEALTH.lock() = HostHealth::new();
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let Some(_work) = UiWork::enter() else { return LRESULT(0) };
            if let Some(app) = APP.get() {
                let wants_host = crate::window::wants_widget_visible(app)
                    && app
                        .state::<Arc<AppState>>()
                        .settings
                        .get()
                        .widget
                        .taskbar_mode();
                if !wants_host {
                    crate::taskbar_reservation::release();
                }
                if crate::taskbar_reservation::heartbeat() {
                    crate::window::reposition(app);
                }
                let retry_due = HEALTH.lock().retry_due(Instant::now());
                if wants_host && !engaged() && retry_due {
                    crate::window::reposition(app);
                }
                if engaged() {
                    let count = rows(app.state::<Arc<AppState>>().payload.read().as_ref()).len();
                    if ROW_COUNT.swap(count, Ordering::SeqCst) != count {
                        crate::window::reposition(app);
                    }
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
                    }
                    wake();
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

/// Keep every actual window, including weekly-only accounts, in slot order.
pub(crate) fn rows(payload: Option<&crate::model::UsagePayload>) -> Vec<(&str, &WindowSnapshot)> {
    payload
        .into_iter()
        .flat_map(|p| &p.providers)
        .filter(|p| p.enabled)
        .flat_map(|p| [("5h", p.five_hour.as_ref()), ("7d", p.weekly.as_ref())])
        .filter_map(|(label, window)| window.map(|w| (w.label.as_deref().unwrap_or(label), w)))
        .collect()
}

pub(crate) fn compact_height(count: usize) -> f64 {
    if count <= 1 {
        crate::monitor::COMPACT_SINGLE_LOGICAL_H
    } else {
        18.0 * count as f64 + 2.0 * (count - 1) as f64
    }
}

pub fn content_height(app: &AppHandle, compact: bool) -> f64 {
    if compact {
        compact_height(rows(app.state::<Arc<AppState>>().payload.read().as_ref()).len())
    } else {
        44.0
    }
}

pub(crate) fn plan_name(plan: Option<&str>) -> String {
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

unsafe fn paint(hwnd: HWND) {
    // Per-pixel alpha is submitted by UpdateLayeredWindow. Validate WM_PAINT's
    // update region without mixing GDI text or color-key transparency into it.
    let mut ps = PAINTSTRUCT::default();
    BeginPaint(hwnd, &mut ps);
    let _ = EndPaint(hwnd, &ps);
    // The lifecycle manager owns rendering. A nested WM_PAINT during a COM
    // call only validates the region and cannot recursively initialize COM.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderSnapshot, UsagePayload};

    #[test]
    fn transient_graphics_failures_retry_and_repeated_failures_have_a_cooldown() {
        let now = Instant::now();
        let mut health = HostHealth::new();
        assert!(health.allow(1, now));
        assert!(!health.failed(1, now));
        assert!(!health.failed(1, now));
        assert!(health.failed(1, now));
        assert!(!health.allow(1, now + Duration::from_secs(9)));
        assert!(!health.retry_due(now + Duration::from_secs(9)));
        assert!(health.retry_due(now + RETRY_DELAY));
        assert!(health.allow(1, now + RETRY_DELAY));
        assert!(!health.failed(1, now + RETRY_DELAY));
        assert!(health.allow(2, now));
        assert_eq!(health.failures, 0);
    }

    #[test]
    fn native_ui_work_cannot_reenter_while_com_pumps_messages() {
        let work = UiWork::enter().expect("first native update");
        assert!(UiWork::enter().is_none());
        assert!(UiWork::enter().is_none());
        drop(work);
        assert!(UiWork::enter().is_some());
    }

    #[test]
    fn weekly_only_is_visible_in_both_native_layouts() {
        let mut provider = ProviderSnapshot::empty("codex", "Codex");
        provider.weekly = Some(WindowSnapshot::official(37.0, None));
        let payload = UsagePayload {
            providers: vec![provider],
            generated_at: chrono::Utc::now(),
            onboarding: false,
        };
        let actual = rows(Some(&payload));
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].0, "7d");
        assert_eq!(actual[0].1.utilization, 37.0);
        assert_eq!(compact_height(actual.len()), 22.0);
    }

    #[test]
    fn compact_native_layout_keeps_both_windows_and_sizes_two_capsules() {
        let mut provider = ProviderSnapshot::empty("codex", "Codex");
        provider.five_hour = Some(WindowSnapshot::official(12.0, None));
        provider.weekly = Some(WindowSnapshot::official(91.0, None));
        let payload = UsagePayload {
            providers: vec![provider],
            generated_at: chrono::Utc::now(),
            onboarding: false,
        };
        let actual = rows(Some(&payload));
        assert_eq!(actual.len(), 2);
        assert_eq!(actual[0].0, "5h");
        assert_eq!(actual[1].0, "7d");
        assert_eq!(compact_height(actual.len()), 38.0);
        assert!(rows(None).is_empty());
        assert_eq!(compact_height(0), 22.0);
    }
}
