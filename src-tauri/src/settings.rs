//! Persistent settings, stored at `%APPDATA%\CodexCapper\settings.json` on
//! Windows and `~/.config/CodexCapper/settings.json` on Linux.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetSettings {
    pub visible: bool,
    /// Persistent monitor identity. Raw coordinates are never stored, so a
    /// display coming back on a different arrangement cannot strand the window.
    pub monitor_stable_id: Option<String>,
    pub monitor_friendly_name: Option<String>,
    pub corner: String,
    pub edge_offset_x: i32,
    pub edge_offset_y: i32,
    pub compact_mode: bool,
    pub click_through: bool,
    /// "float" for a free floating window, "taskbar" to pin into the strip.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Logical gap between the widget and the notification area.
    #[serde(default = "default_tray_gap")]
    pub tray_gap: i32,
    /// Which end of the strip pinned placement docks against: "right" sits
    /// just left of the notification area, "left" sits at the start button
    /// side of the strip. Only meaningful in taskbar mode.
    #[serde(default = "default_dock_side")]
    pub dock_side: String,
    /// How the widget prints a window's reset time after the percentage:
    /// "absolute" for a clock time, "countdown" for time remaining.
    #[serde(default = "default_reset_time_style")]
    pub reset_time_style: String,
    #[serde(default = "yes")]
    pub hide_on_fullscreen: bool,
}

fn default_mode() -> String {
    // A fresh install sits in the taskbar strip beside the notification
    // area, which is what the widget is for; floating is the opt-in.
    "taskbar".into()
}

fn default_tray_gap() -> i32 {
    8
}

fn default_dock_side() -> String {
    "right".into()
}

fn default_reset_time_style() -> String {
    // How far off the reset is reads better as a countdown; the exact clock
    // time stays one click away in the tray.
    "countdown".into()
}

impl Default for WidgetSettings {
    fn default() -> Self {
        Self {
            visible: true,
            monitor_stable_id: None,
            monitor_friendly_name: None,
            corner: "bottom-right".into(),
            edge_offset_x: 0,
            edge_offset_y: 0,
            compact_mode: false,
            click_through: false,
            mode: default_mode(),
            tray_gap: default_tray_gap(),
            dock_side: default_dock_side(),
            reset_time_style: default_reset_time_style(),
            hide_on_fullscreen: true,
        }
    }
}

impl WidgetSettings {
    pub fn taskbar_mode(&self) -> bool {
        self.mode == "taskbar"
    }

    pub fn dock_left(&self) -> bool {
        self.dock_side == "left"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// "system", "dark" or "light".
    pub theme: String,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollingSettings {
    pub http_interval_secs: u64,
}

impl Default for PollingSettings {
    fn default() -> Self {
        Self {
            http_interval_secs: 180,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexSettings {
    pub enabled: bool,
    /// Undocumented HTTP fallback, off by default.
    pub allow_http_fallback: bool,
}

impl Default for CodexSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_http_fallback: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderSettings {
    #[serde(default)]
    pub codex: CodexSettings,
}

impl ProviderSettings {
    /// How many provider columns the layout has to hold. It lives here rather
    /// than at the call site so that the taskbar strip cannot be sized for a
    /// provider that is no longer drawn.
    pub fn enabled_count(&self) -> usize {
        usize::from(self.codex.enabled)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateSettings {
    #[serde(default = "yes")]
    pub check: bool,
    pub last_check: Option<DateTime<Utc>>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub schema_version: u32,
    #[serde(default)]
    pub widget: WidgetSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub polling: PollingSettings,
    #[serde(default)]
    pub providers: ProviderSettings,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub updates: UpdateSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            widget: Default::default(),
            appearance: Default::default(),
            polling: Default::default(),
            providers: Default::default(),
            autostart: false,
            updates: UpdateSettings {
                check: true,
                last_check: None,
            },
        }
    }
}

pub fn data_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("CodexCapper");
    migrate_old_name(&base, &dir);
    dir
}

/// The software is called CodexCapper now. Everything the old name had
/// collected — settings, state, logs — comes along with it, once, the first
/// time the new directory is asked for and does not exist yet.
fn migrate_old_name(base: &std::path::Path, dir: &std::path::Path) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) || dir.exists() {
        return;
    }
    let old = base.join("FluxDock");
    if old.is_dir() {
        if std::fs::rename(&old, dir).is_ok() {
            tracing::info!("carried {} over to {}", old.display(), dir.display());
        }
    }
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// Presence of this file tells the watchdog task that the app was closed on
/// purpose and should stay closed.
pub fn stay_closed_marker() -> PathBuf {
    data_dir().join("stay-closed")
}

/// Called when the user quits from the tray menu.
pub fn mark_stay_closed() {
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(stay_closed_marker(), "quit from the tray menu\n");
}

/// Called on every start, so a watchdog restart or a manual launch clears it.
pub fn clear_stay_closed() {
    let _ = std::fs::remove_file(stay_closed_marker());
}

/// Single instance for the lifetime of the process. Every mutation is written
/// through immediately.
pub struct SettingsStore {
    inner: RwLock<Settings>,
    path: PathBuf,
}

impl SettingsStore {
    pub fn load() -> Self {
        let path = settings_path();
        let settings = std::fs::read_to_string(&path)
            .ok()
            .map(|s| strip_bom(&s).to_string())
            .and_then(|s| match serde_json::from_str::<Settings>(&s) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("settings.json could not be read, using defaults: {e}");
                    None
                }
            })
            .unwrap_or_default();

        let store = Self {
            inner: RwLock::new(settings),
            path,
        };
        let _ = store.persist();
        store
    }

    pub fn get(&self) -> Settings {
        self.inner.read().clone()
    }

    pub fn update<F: FnOnce(&mut Settings)>(&self, f: F) -> Settings {
        let snapshot = {
            let mut guard = self.inner.write();
            f(&mut guard);
            guard.clone()
        };
        if let Err(e) = self.persist() {
            tracing::error!("settings could not be written: {e}");
        }
        snapshot
    }

    fn persist(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(&*self.inner.read())?;
        write_atomic(&self.path, json.as_bytes())
    }
}

/// The settings file is meant to be hand edited, and both Notepad and
/// PowerShell write UTF-8 with a byte order mark that serde refuses to parse.
/// Dropping it quietly avoids resetting every preference.
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Temp file plus rename, so a crash never leaves a half written file behind.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The taskbar strip is sized from this number, so a provider that is not
    /// counted is a column drawn outside the window.
    #[test]
    fn every_provider_counts_towards_the_strip_width() {
        let mut providers = ProviderSettings::default();
        assert_eq!(providers.enabled_count(), 1);

        providers.codex.enabled = false;
        assert_eq!(providers.enabled_count(), 0);
    }

    #[test]
    fn a_byte_order_mark_does_not_reset_settings() {
        let json = "\u{feff}{\"schema_version\":1,\"widget\":{\"visible\":false,\"monitor_stable_id\":null,\"monitor_friendly_name\":null,\"corner\":\"bottom-right\",\"edge_offset_x\":0,\"edge_offset_y\":0,\"compact_mode\":false,\"click_through\":false}}";
        let parsed: Settings = serde_json::from_str(strip_bom(json)).expect("parses");
        assert!(!parsed.widget.visible);
    }

    #[test]
    fn missing_sections_fall_back_to_defaults() {
        let parsed: Settings = serde_json::from_str(r#"{"schema_version":1}"#).expect("parses");
        assert!(parsed.widget.visible);
        assert_eq!(parsed.polling.http_interval_secs, 180);
        assert!(parsed.providers.codex.enabled);
        assert!(parsed.widget.taskbar_mode());
    }
}
