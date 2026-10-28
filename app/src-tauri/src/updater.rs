//! Update checks against GitHub Releases (`latest.json`, signed with the project's minisign key).
//!
//! Checked shortly after startup and then daily. A found update waits until nothing is being
//! recorded or encoded, then a toast offers it; installing restarts the app. Portable copies never
//! update themselves.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::i18n::I18n;
use crate::ui::{self, Toast};
use crate::{AppState, paths};

const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const WAIT_FOR_IDLE: Duration = Duration::from_secs(30);

/// An update found but not installed yet.
#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

pub fn spawn(app: AppHandle) {
    if paths::is_portable() {
        return;
    }
    // A thread of its own: it mostly sleeps, and waiting for idle must not hold an async worker.
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        loop {
            if app.state::<AppState>().settings.lock().unwrap().auto_update {
                tauri::async_runtime::block_on(check(&app));
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

async fn check(app: &AppHandle) {
    let Ok(updater) = app.updater() else { return };
    // Network or signature problems are not worth interrupting anyone over; try again tomorrow.
    let Ok(Some(update)) = updater.check().await else {
        return;
    };
    let version = update.version.clone();
    *app.state::<Pending>().0.lock().unwrap() = Some(update);
    // Never interrupt a recording or an encode with an update prompt.
    while busy(app) {
        std::thread::sleep(WAIT_FOR_IDLE); // on the updater's own thread
    }
    let i = I18n::new(&app.state::<AppState>().settings.lock().unwrap().language);
    ui::toast(app, Toast::update(&i, &version));
}

/// Recording, encoding, or an editor export is running: installing would kill it.
fn busy(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    !state.controller.is_idle() || state.exports.load(std::sync::atomic::Ordering::Relaxed) > 0
}

/// Download and install the pending update, then restart.
#[tauri::command]
pub async fn install_update(app: AppHandle, pending: State<'_, Pending>) -> Result<(), String> {
    let i = I18n::new(&app.state::<AppState>().settings.lock().unwrap().language);
    // The prompt may have been sitting there while a new recording started.
    if busy(&app) {
        ui::toast(&app, Toast::warning(i.t("toast.update_busy")));
        return Ok(());
    }
    let Some(update) = pending.0.lock().unwrap().take() else {
        return Ok(());
    };
    ui::toast(&app, Toast::installing(&i));
    if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
        // Keep it so the user can try again without waiting a day.
        *pending.0.lock().unwrap() = Some(update);
        ui::toast(&app, Toast::failed(&i, &e.to_string()));
        return Err(e.to_string());
    }
    app.restart();
}

/// Lets the settings window explain why updates are unavailable.
#[tauri::command]
pub fn is_portable() -> bool {
    paths::is_portable()
}
