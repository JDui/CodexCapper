//! Shared application state and the refresh channel.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;

use crate::model::{AppearanceConfig, UsagePayload};
use crate::providers::codex::CodexProvider;
use crate::settings::SettingsStore;
use crate::theme;

/// Set just before a deliberate quit so the exit guard lets it through.
static QUITTING: AtomicBool = AtomicBool::new(false);

pub fn begin_quit() {
    QUITTING.store(true, Ordering::SeqCst);
}

pub fn is_quitting() -> bool {
    QUITTING.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// A watched file changed. Local layers refresh, the network is left alone.
    Scheduled,
    /// The polling interval elapsed.
    Timer,
    /// The user asked for it.
    Manual,
    /// Settings changed and providers need to be reconfigured.
    Settings,
}

pub struct AppState {
    pub settings: Arc<SettingsStore>,
    pub codex: Mutex<CodexProvider>,
    pub payload: RwLock<Option<UsagePayload>>,
    pub tx: UnboundedSender<Refresh>,
}

impl AppState {
    pub fn new(settings: Arc<SettingsStore>) -> (Arc<Self>, UnboundedReceiver<Refresh>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let s = settings.get();
        let state = Arc::new(Self {
            codex: Mutex::new(CodexProvider::new(s.providers.codex.clone())),
            payload: RwLock::new(None),
            settings,
            tx,
        });
        (state, rx)
    }

    pub fn request(&self, reason: Refresh) {
        let _ = self.tx.send(reason);
    }

    pub fn appearance(&self) -> AppearanceConfig {
        let s = self.settings.get();
        AppearanceConfig {
            resolved_theme: theme::resolve(&s.appearance.theme).to_string(),
            theme: s.appearance.theme,
            compact_mode: s.widget.compact_mode,
            show_model_weekly: false,
            placement: s.widget.mode.clone(),
            reset_time_style: s.widget.reset_time_style.clone(),
        }
    }
}
