//! Polls the provider, publishes the payload and keeps the tray badge and
//! state file in step.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager};

use crate::model::{ProviderStatus, UsagePayload};
use crate::providers::UsageProvider;
use crate::settings::{data_dir, write_atomic};
use crate::state::{AppState, Refresh};
use crate::tray;

/// Minimum spacing between cycles triggered by the file watcher.
const WATCHER_COALESCE: Duration = Duration::from_secs(3);

pub async fn run(
    app: AppHandle,
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Refresh>,
) {
    cycle(&app, &state, true).await;
    let mut last_cycle = std::time::Instant::now();

    loop {
        let interval = {
            let s = state.settings.get();
            Duration::from_secs(s.polling.http_interval_secs.clamp(60, 3600))
        };

        let reason = tokio::select! {
            r = rx.recv() => match r {
                Some(r) => r,
                None => return,
            },
            _ = tokio::time::sleep(interval) => Refresh::Timer,
        };

        if reason == Refresh::Settings {
            let s = state.settings.get();
            state
                .codex
                .lock()
                .await
                .apply_settings(s.providers.codex.clone());
            let _ = app.emit("config", state.appearance());
        }

        // The watcher fires several times a second during active use, on top of
        // its own debounce.
        if reason == Refresh::Scheduled && last_cycle.elapsed() < WATCHER_COALESCE {
            continue;
        }

        let force = matches!(reason, Refresh::Manual | Refresh::Settings | Refresh::Timer);
        cycle(&app, &state, force).await;
        last_cycle = std::time::Instant::now();
    }
}

pub async fn cycle(app: &AppHandle, state: &Arc<AppState>, force: bool) {
    let settings = state.settings.get();

    let mut codex_snap = if settings.providers.codex.enabled {
        Some(state.codex.lock().await.poll(force).await)
    } else {
        None
    };

    if let Some(s) = codex_snap.as_mut() {
        s.enabled = true;
    }

    let providers: Vec<_> = codex_snap.into_iter().collect();

    let onboarding = !providers.is_empty()
        && providers
            .iter()
            .all(|p| p.status == ProviderStatus::CliNotFound);

    let payload = UsagePayload {
        providers,
        generated_at: Utc::now(),
        onboarding,
    };

    write_state_file(&payload);

    *state.payload.write() = Some(payload.clone());
    let _ = app.emit("usage", &payload);
    tray::update_badge(app, &payload);
}

/// Machine readable status at `state.json` in the data directory,
/// `%APPDATA%\CodexCapper` on Windows and `~/.config/CodexCapper` on Linux.
/// `generated_at` changes every cycle, so the comparison ignores it.
fn write_state_file(payload: &UsagePayload) {
    static LAST: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

    let Ok(json) = serde_json::to_string_pretty(payload) else {
        return;
    };
    let Ok(providers) = serde_json::to_string(&payload.providers) else {
        return;
    };

    {
        let mut last = LAST.lock();
        if last.as_deref() == Some(providers.as_str()) {
            return;
        }
        *last = Some(providers);
    }

    let _ = std::fs::create_dir_all(data_dir());
    let _ = write_atomic(&data_dir().join("state.json"), json.as_bytes());
}

pub fn spawn_watchers(app: &AppHandle, paths: Vec<std::path::PathBuf>) {
    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;

    if paths.is_empty() {
        tracing::info!("no paths to watch, falling back to the timer only");
        return;
    }

    let app = app.clone();
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut debouncer) = new_debouncer(Duration::from_millis(300), None, tx) else {
            return;
        };
        for p in &paths {
            if let Err(e) = debouncer.watch(p, RecursiveMode::Recursive) {
                tracing::warn!("could not watch {p:?}: {e}");
            }
        }
        tracing::info!("watching {} root(s)", paths.len());

        for res in rx {
            if res.is_ok() {
                if let Some(state) = app.try_state::<Arc<AppState>>() {
                    state.request(Refresh::Scheduled);
                }
            }
        }
        drop(debouncer);
    });
}

/// Publishes synthetic values so the visual design can be reviewed without
/// waiting for real usage to build up. The provider is never polled in this
/// mode and nothing is written to disk.
pub async fn run_demo(app: AppHandle, state: Arc<AppState>) {
    use crate::model::{ProviderSnapshot, WindowSnapshot};
    use chrono::Duration as ChronoDuration;

    tracing::info!("demo mode, publishing synthetic data");
    let mut step: f32 = 0.0;

    loop {
        step += 0.6;

        // Placement sizes itself from the enabled providers, so the demo has to
        // honour the same setting or the window and its contents disagree.
        let settings = state.settings.get();

        let ramp = |offset: f32, span: f32| -> f32 {
            let raw = (step + offset) % (span * 2.0);
            if raw > span {
                span * 2.0 - raw
            } else {
                raw
            }
        };

        let window = |util: f32, minutes: i64| WindowSnapshot {
            utilization: util,
            label: None,
            resets_at: Some(Utc::now() + ChronoDuration::minutes(minutes)),
            source: crate::model::Source::Official,
            as_of: Utc::now(),
            stale: false,
            eta: None,
            burn_rate: Some(4.2),
        };

        let mut providers = Vec::new();

        if settings.providers.codex.enabled {
            let mut codex = ProviderSnapshot::empty("codex", "Codex CLI");
            codex.status = ProviderStatus::Ok;
            codex.plan_type = Some("demo".into());
            codex.five_hour = Some(window(ramp(28.0, 100.0), 64));
            codex.weekly = Some(window(ramp(80.0, 100.0), 8300));
            providers.push(codex);
        }

        let payload = UsagePayload {
            providers,
            generated_at: Utc::now(),
            onboarding: false,
        };

        *state.payload.write() = Some(payload.clone());
        let _ = app.emit("usage", &payload);
        tray::update_badge(&app, &payload);

        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}
