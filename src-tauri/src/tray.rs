//! Tray icon, badge and context menu.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::model::{ProviderStatus, UsagePayload};
use crate::state::{AppState, Refresh};
use crate::{monitor, window};

/// What the session calls the thing this switch turns on.
#[cfg(windows)]
const AUTOSTART_LABEL: &str = "开机自动启动";
#[cfg(not(windows))]
const AUTOSTART_LABEL: &str = "登录时启动";

pub const TRAY_ID: &str = "main";

/// The menus the app owns.
///
/// The tray's menu and the widget's context menu used to be one object shared
/// between them. Showing a menu borrows it and swapping the tray's menu borrows
/// it again, and nothing orders those two against each other: on Linux a popup
/// is not modal and returns while it is still on screen, so a rebuild landing
/// in that gap took a second borrow of the same object and aborted the process.
/// A popup now gets a menu of its own that nothing else ever touches.
pub struct MenuHandle {
    /// Attached to the tray icon.
    pub tray: RwLock<Option<Menu<Wry>>>,
    /// A description of what `tray` currently shows.
    pub signature: RwLock<Option<String>>,
    /// Behind the last widget popup, kept alive for as long as it is displayed.
    pub popup: RwLock<Option<Menu<Wry>>>,
    /// The tray menu one generation back. A swap can land while the menu it
    /// replaces is still being dismissed, and dropping the old object then
    /// tears down GTK widgets out from under a menu that is on screen. Holding
    /// it until the swap after next costs one menu's worth of memory.
    pub retired: RwLock<Option<Menu<Wry>>>,
}

const ICON_GREEN: &[u8] = include_bytes!("../icons/tray-green.png");
const ICON_AMBER: &[u8] = include_bytes!("../icons/tray-amber.png");
const ICON_RED: &[u8] = include_bytes!("../icons/tray-red.png");
const ICON_GRAY: &[u8] = include_bytes!("../icons/tray-gray.png");

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    app.manage(MenuHandle {
        tray: RwLock::new(None),
        signature: RwLock::new(None),
        popup: RwLock::new(None),
        retired: RwLock::new(None),
    });

    let menu = build_menu(app)?;
    let signature = menu_signature(app);
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(ICON_GRAY)?)
        .tooltip("CodexCapper")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                window::toggle_visibility(tray.app_handle());
            }
        })
        .build(app)?;

    let state = app.state::<MenuHandle>();
    state.tray.write().replace(menu);
    state.signature.write().replace(signature);
    Ok(())
}

/// Builds a menu for one popup on the widget.
///
/// Deliberately not the tray's menu. See `MenuHandle`.
pub fn build_popup_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    build_menu(app)
}

/// True while the widget's context menu is being shown.
///
/// Only meaningful where showing a menu blocks. On Windows the popup runs a
/// modal loop and this covers the whole time the menu is up; on Linux the call
/// returns immediately and this covers almost nothing, which is why the popup
/// owns its menu outright rather than relying on this flag. It stays because it
/// is still worth not swapping the tray menu mid modal loop on Windows.
static POPUP_OPEN: AtomicBool = AtomicBool::new(false);
static REBUILD_PENDING: AtomicBool = AtomicBool::new(false);

/// Description of everything the menu renders, so an unchanged menu is never
/// rebuilt. Swapping it has a real cost and a real hazard; doing it only when
/// the contents actually differ removes most of both.
fn menu_signature(app: &AppHandle) -> String {
    let s = app.state::<Arc<AppState>>().settings.get();
    let mut sig = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        s.widget.visible,
        s.widget.reset_time_style,
        s.appearance.theme,
        s.polling.http_interval_secs,
        s.widget.mode,
        s.widget.dock_side,
        s.widget.compact_mode,
        s.widget.click_through,
        s.widget.hide_on_fullscreen,
        s.providers.codex.enabled,
    );
    sig.push('|');
    sig.push_str(s.widget.monitor_stable_id.as_deref().unwrap_or("-"));

    // Only the identity of the attached monitors matters here. Their geometry
    // changes constantly and must not drive menu rebuilds.
    #[cfg(windows)]
    for m in monitor::enumerate() {
        sig.push('|');
        sig.push_str(&m.stable_id);
        sig.push_str(&m.friendly_name);
    }
    sig
}

pub fn rebuild(app: &AppHandle) {
    if POPUP_OPEN.load(Ordering::SeqCst) {
        REBUILD_PENDING.store(true, Ordering::SeqCst);
        return;
    }

    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if POPUP_OPEN.load(Ordering::SeqCst) {
            REBUILD_PENDING.store(true, Ordering::SeqCst);
            return;
        }

        let signature = menu_signature(&handle);
        {
            let last = handle.state::<MenuHandle>();
            if last.signature.read().as_deref() == Some(signature.as_str()) {
                return;
            }
        }

        let Ok(menu) = build_menu(&handle) else {
            return;
        };
        if let Some(tray) = handle.tray_by_id(TRAY_ID) {
            let _ = tray.set_menu(Some(menu.clone()));
        }
        let state = handle.state::<MenuHandle>();
        let previous = state.tray.write().replace(menu);
        *state.retired.write() = previous;
        state.signature.write().replace(signature);
    });
}

/// Marks a context menu as displayed for as long as the guard lives.
pub struct PopupGuard;

impl Default for PopupGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl PopupGuard {
    pub fn new() -> Self {
        POPUP_OPEN.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for PopupGuard {
    fn drop(&mut self) {
        POPUP_OPEN.store(false, Ordering::SeqCst);
    }
}

/// Applies a rebuild that was deferred while a menu was open.
pub fn flush_pending_rebuild(app: &AppHandle) {
    if REBUILD_PENDING.swap(false, Ordering::SeqCst) {
        rebuild(app);
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let state = app.state::<Arc<AppState>>();
    let s = state.settings.get();

    let header =
        MenuItemBuilder::with_id("header", format!("CodexCapper v{} · Codex 额度", env!("CARGO_PKG_VERSION")))
            .enabled(false)
            .build(app)?;

    let refresh = MenuItemBuilder::with_id("refresh", "立即刷新").build(app)?;
    let restart = MenuItemBuilder::with_id("restart", "强制重启").build(app)?;
    let show_widget = CheckMenuItemBuilder::with_id("show_widget", "显示组件")
        .checked(s.widget.visible)
        .build(app)?;

    let mut freq = SubmenuBuilder::new(app, "刷新频率");
    for (secs, label) in [
        (60u64, "1 分钟"),
        (180, "3 分钟"),
        (300, "5 分钟"),
        (900, "15 分钟"),
    ] {
        freq = freq.item(
            &CheckMenuItemBuilder::with_id(format!("freq:{secs}"), label)
                .checked(s.polling.http_interval_secs == secs)
                .build(app)?,
        );
    }
    let freq = freq.build()?;

    // Pinning is only offered where there is a panel this widget understands.
    // Showing the choice and then quietly ignoring it is worse than not
    // offering it, and the Placement submenu with one entry still says which
    // placement is in use.
    let mut placement = SubmenuBuilder::new(app, "位置").item(
        &CheckMenuItemBuilder::with_id("place:float", "悬浮窗口")
            .checked(!s.widget.taskbar_mode())
            .build(app)?,
    );
    if monitor::supports_panel_docking() {
        placement = placement.item(
            &CheckMenuItemBuilder::with_id("place:taskbar", "固定到任务栏")
                .checked(s.widget.taskbar_mode())
                .build(app)?,
        );
        // Which end of the strip the pinned widget hugs. They only mean
        // something while pinned, so they check themselves only then.
        placement = placement
            .separator()
            .item(
                &CheckMenuItemBuilder::with_id("dock:left", "靠左端（开始按钮侧）")
                    .checked(s.widget.taskbar_mode() && s.widget.dock_left())
                    .build(app)?,
            )
            .item(
                &CheckMenuItemBuilder::with_id("dock:right", "靠右端（托盘图标侧）")
                    .checked(s.widget.taskbar_mode() && !s.widget.dock_left())
                    .build(app)?,
            );
    } else {
        // Listed and greyed rather than left out. A Placement submenu holding
        // one placement reads as something that failed to load, and the first
        // thing a Linux user asks is why the widget does not go in the panel
        // the way it does on Windows. The answer belongs where the question is.
        placement = placement.item(
            &MenuItemBuilder::with_id("place:taskbar", "固定到任务栏（仅 Windows）")
                .enabled(false)
                .build(app)?,
        );
    }
    let placement = placement.build()?;

    let mut mon = SubmenuBuilder::new(app, "显示器");
    {
        let monitors = monitor::enumerate();
        let selected = s.widget.monitor_stable_id.clone();
        let mut saved_present = false;

        for (i, m) in monitors.iter().enumerate() {
            let checked = selected.as_deref() == Some(m.stable_id.as_str())
                || (selected.is_none() && m.primary);
            if selected.as_deref() == Some(m.stable_id.as_str()) {
                saved_present = true;
            }
            let label = format!(
                "{}：{}{}",
                i + 1,
                m.friendly_name,
                if m.primary { "（主显示器）" } else { "" }
            );
            mon = mon.item(
                &CheckMenuItemBuilder::with_id(format!("mon:{}", m.stable_id), label)
                    .checked(checked)
                    .build(app)?,
            );
        }

        // A saved monitor that is currently unplugged stays listed so the
        // choice is visible and comes back when the display returns.
        if let (Some(id), false) = (selected.as_deref(), saved_present) {
            let name = s
                .widget
                .monitor_friendly_name
                .clone()
                .unwrap_or_else(|| "已保存的显示器".into());
            mon = mon.item(
                &CheckMenuItemBuilder::with_id(
                    format!("mon:{id}"),
                    format!("（未连接）{name}"),
                )
                .checked(true)
                .build(app)?,
            );
        }
    }
    let mon = mon
        .separator()
        .item(&MenuItemBuilder::with_id("reset_position", "重置位置").build(app)?)
        .build()?;

    let mut appearance = SubmenuBuilder::new(app, "外观");
    for (id, label) in [
        ("theme:system", "主题：跟随系统"),
        ("theme:dark", "主题：深色"),
        ("theme:light", "主题：浅色"),
    ] {
        let key = id.split(':').nth(1).unwrap_or("system");
        appearance = appearance.item(
            &CheckMenuItemBuilder::with_id(id, label)
                .checked(s.appearance.theme == key)
                .build(app)?,
        );
    }
    let appearance = appearance
        .separator()
        .item(
            &CheckMenuItemBuilder::with_id("compact", "紧凑模式")
                .checked(s.widget.compact_mode)
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("click_through", "鼠标穿透")
                .checked(s.widget.click_through)
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("hide_fullscreen", "全屏应用时隐藏")
                .checked(s.widget.hide_on_fullscreen)
                .build(app)?,
        )
        .separator()
        // How the widget prints a window's reset time after the percentage.
        .item(
            &CheckMenuItemBuilder::with_id("tstyle:absolute", "重置时间：绝对时间")
                .checked(s.widget.reset_time_style != "countdown")
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("tstyle:countdown", "重置时间：倒计时")
                .checked(s.widget.reset_time_style == "countdown")
                .build(app)?,
        )
        .build()?;

    let autostart = CheckMenuItemBuilder::with_id("autostart", AUTOSTART_LABEL)
        .checked(s.autostart)
        .build(app)?;

    let builder = MenuBuilder::new(app).item(&header);
    builder
        .separator()
        .item(&refresh)
        .item(&restart)
        .item(&show_widget)
        .separator()
        .item(&freq)
        .item(&placement)
        .item(&mon)
        .item(&appearance)
        .item(&autostart)
        .separator()
        .item(&MenuItemBuilder::with_id("exit", "退出").build(app)?)
        .build()
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().0.clone();
    let state = app.state::<Arc<AppState>>().inner().clone();

    let mut needs_rebuild = true;
    let mut needs_config = false;
    let mut needs_reposition = false;

    match id.as_str() {
        "refresh" => {
            state.request(Refresh::Manual);
            needs_rebuild = false;
        }
        "restart" => {
            // app.restart() hands off to a child that immediately finds this
            // still-running instance through the single instance lock, defers
            // to it, and exits, leaving nothing behind once the parent goes.
            // A detached launcher that waits for this process to disappear is
            // the reliable version.
            relaunch_after_exit();
            crate::state::begin_quit();
            app.exit(0);
            return;
        }
        "show_widget" => window::toggle_visibility(app),
        "reset_position" => {
            state.settings.update(|s| {
                s.widget.monitor_stable_id = None;
                s.widget.monitor_friendly_name = None;
                s.widget.edge_offset_x = 0;
                s.widget.edge_offset_y = 0;
            });
            needs_reposition = true;
        }
        "compact" => {
            state
                .settings
                .update(|s| s.widget.compact_mode = !s.widget.compact_mode);
            needs_config = true;
            // Compact mode decides how wide a pinned column is drawn, so the
            // strip has to be measured again. Without this the setting took
            // effect on the next thing that happened to move the widget.
            needs_reposition = true;
        }
        "click_through" => {
            let now = state
                .settings
                .update(|s| s.widget.click_through = !s.widget.click_through)
                .widget
                .click_through;
            window::set_click_through(app, now);
        }
        "hide_fullscreen" => {
            state
                .settings
                .update(|s| s.widget.hide_on_fullscreen = !s.widget.hide_on_fullscreen);
        }
        "autostart" => {
            let enabled = state
                .settings
                .update(|s| s.autostart = !s.autostart)
                .autostart;
            crate::autostart::apply(app, enabled);
        }
        "exit" => {
            // Quitting here is deliberate, so the watchdog leaves it alone.
            crate::settings::mark_stay_closed();
            crate::state::begin_quit();
            app.exit(0);
            return;
        }
        other => {
            if let Some(secs) = other
                .strip_prefix("freq:")
                .and_then(|v| v.parse::<u64>().ok())
            {
                state
                    .settings
                    .update(|s| s.polling.http_interval_secs = secs);
                state.request(Refresh::Settings);
            } else if let Some(mode) = other.strip_prefix("place:") {
                let mode = mode.to_string();
                state.settings.update(|s| s.widget.mode = mode.clone());
                needs_reposition = true;
                needs_config = true;
            } else if let Some(side) = other.strip_prefix("dock:") {
                // Choosing an end of the strip also pins: nobody asks where
                // in the taskbar to sit while they are floating above it.
                let side = side.to_string();
                state.settings.update(|s| {
                    s.widget.dock_side = side;
                    s.widget.mode = "taskbar".into();
                });
                needs_reposition = true;
                needs_config = true;
            } else if let Some(theme) = other.strip_prefix("theme:") {
                let theme = theme.to_string();
                state
                    .settings
                    .update(|s| s.appearance.theme = theme.clone());
                needs_config = true;
            } else if let Some(mon_id) = other.strip_prefix("mon:") {
                let mon_id = mon_id.to_string();
                #[cfg(windows)]
                let friendly = monitor::enumerate()
                    .into_iter()
                    .find(|m| m.stable_id == mon_id)
                    .map(|m| m.friendly_name);
                #[cfg(not(windows))]
                let friendly: Option<String> = None;
                state.settings.update(|s| {
                    s.widget.monitor_stable_id = Some(mon_id.clone());
                    s.widget.monitor_friendly_name = friendly.clone();
                    s.widget.edge_offset_x = 0;
                    s.widget.edge_offset_y = 0;
                });
                needs_reposition = true;
            } else if let Some(style) = other.strip_prefix("tstyle:") {
                let style = style.to_string();
                state.settings.update(|s| {
                    s.widget.reset_time_style = style;
                });
                needs_config = true;
            } else {
                needs_rebuild = false;
            }
        }
    }

    if needs_reposition {
        window::reposition(app);
    }
    if needs_config {
        let _ = app.emit("config", state.appearance());
    }
    if needs_rebuild {
        rebuild(app);
    }
}

/// The badge shows the highest live window across enabled providers.
pub fn update_badge(app: &AppHandle, payload: &UsagePayload) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    let peak = payload
        .providers
        .iter()
        .filter_map(|p| p.peak())
        .fold(None, |acc: Option<f32>, v| {
            Some(acc.map_or(v, |a| a.max(v)))
        });

    let needs_attention = payload.providers.iter().any(|p| {
        matches!(
            p.status,
            ProviderStatus::ReauthNeeded | ProviderStatus::PlanUnsupported
        )
    });

    let (kind, bytes) = match peak {
        None => (0u8, ICON_GRAY),
        Some(_) if needs_attention => (1, ICON_AMBER),
        Some(v) if v >= 90.0 => (2, ICON_RED),
        Some(v) if v >= 70.0 => (1, ICON_AMBER),
        Some(_) => (3, ICON_GREEN),
    };

    static LAST_ICON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);
    if LAST_ICON.swap(kind, std::sync::atomic::Ordering::Relaxed) != kind {
        if let Ok(icon) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
        }
    }

    let mut parts: Vec<String> = Vec::new();
    for p in &payload.providers {
        // Whatever windows this provider actually has, named as it named them.
        let windows: Vec<String> = [("5h", p.five_hour.as_ref()), ("7d", p.weekly.as_ref())]
            .into_iter()
            .filter_map(|(fallback, w)| {
                let w = w?;
                Some(format!(
                    "{} 剩 {:.0}%",
                    w.label.as_deref().unwrap_or(fallback),
                    100.0 - w.utilization
                ))
            })
            .collect();
        if windows.is_empty() {
            if let Some(d) = &p.detail {
                parts.push(format!("{}：{}", p.label, d));
            }
        } else {
            parts.push(format!("{} {}", p.label, windows.join(" / ")));
        }
    }
    let tooltip = if parts.is_empty() {
        "CodexCapper · Codex 额度".to_string()
    } else {
        parts.join("  |  ")
    };
    let _ = tray.set_tooltip(Some(tooltip));
}

/// Starts a detached copy of this executable that waits for this process to exit
/// and then launches a fresh one. Waiting matters: the single instance lock is
/// only released once this process is gone.
///
/// A PowerShell one liner used to do the waiting. The binary does it itself now,
/// because a console program started in the background can put a window on the
/// user's screen and this app has no business doing that.
fn relaunch_after_exit() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = crate::providers::quiet_command(&exe.to_string_lossy())
        .args(["--relaunch-after", &std::process::id().to_string()])
        .spawn();
}
