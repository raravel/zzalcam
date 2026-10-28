//! zzalcam desktop app: tray, hotkeys, target picker, recording and the settings UI.

mod cleanup;
mod clipboard;
mod disk;
mod editor;
mod hotkeys;
mod i18n;
mod output;
mod paths;
mod recorder;
mod settings;
mod tray;
mod ui;
mod updater;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

use crate::i18n::I18n;
use crate::recorder::{Controller, Start};
use crate::settings::Settings;

/// Threads serving editor frame requests.
const PROTOCOL_WORKERS: usize = 3;

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub controller: Arc<Controller>,
    pub hotkeys: Mutex<HashMap<u32, hotkeys::Action>>,
    /// Actions whose hotkey could not be registered.
    pub conflicts: Mutex<Vec<&'static str>>,
    /// The settings window is capturing a new key combination; hotkeys stay unregistered.
    pub hotkeys_suspended: AtomicBool,
    pub editors: editor::Editors,
    /// Unsaved sessions found at startup, frozen so the recovery prompt never touches files
    /// created later (a recording in progress, an open editor).
    pub recovery: Mutex<Vec<PathBuf>>,
    /// The toast page is listening; before that, emitted toasts would be lost.
    pub toast_ready: AtomicBool,
    /// Saves running outside the recorder (editor exports, recovery saves).
    pub exports: std::sync::atomic::AtomicUsize,
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

/// Resolved UI language (`ko` / `en`).
#[tauri::command]
fn get_language(state: State<AppState>) -> &'static str {
    i18n::resolve(&state.settings.lock().unwrap().language)
}

#[tauri::command]
fn get_conflicts(state: State<AppState>) -> Vec<&'static str> {
    state.conflicts.lock().unwrap().clone()
}

/// Save settings and re-apply hotkeys; returns the saved settings and any hotkey conflicts.
#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<AppState>,
    settings: Settings,
) -> Result<(Settings, Vec<&'static str>), String> {
    let saved = settings::save(&app, settings).map_err(|e| e.to_string())?;
    let language_changed = state.settings.lock().unwrap().language != saved.language;
    if state.settings.lock().unwrap().autostart != saved.autostart {
        use tauri_plugin_autostart::ManagerExt;
        let launcher = app.autolaunch();
        let toggled = if saved.autostart {
            launcher.enable()
        } else {
            launcher.disable()
        };
        toggled.map_err(|e| e.to_string())?;
    }
    *state.settings.lock().unwrap() = saved.clone();
    // While a combination is being captured, registering would swallow the very keys being typed;
    // `suspend_hotkeys(false)` applies the saved set afterwards.
    let conflicts = if state.hotkeys_suspended.load(Ordering::Relaxed) {
        state.conflicts.lock().unwrap().clone()
    } else {
        hotkeys::apply(&app, &saved.hotkeys)
    };
    if language_changed {
        tray::relabel(&app, &I18n::new(&saved.language));
        let _ = app.emit("language-changed", i18n::resolve(&saved.language));
    }
    Ok((saved, conflicts))
}

/// Stop listening to hotkeys while the settings window records a new combination.
#[tauri::command]
fn suspend_hotkeys(app: AppHandle, suspend: bool) {
    set_hotkeys_suspended(&app, suspend);
}

fn set_hotkeys_suspended(app: &AppHandle, suspend: bool) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let state = app.state::<AppState>();
    let was = state.hotkeys_suspended.swap(suspend, Ordering::Relaxed);
    if suspend {
        let _ = app.global_shortcut().unregister_all();
    } else if was {
        let hk = state.settings.lock().unwrap().hotkeys.clone();
        hotkeys::apply(app, &hk);
    }
}

#[tauri::command]
fn start(state: State<AppState>, mode: String) {
    let how = match mode.as_str() {
        "control" => Start::Control,
        "region" => Start::Region,
        "all" => Start::AllMonitors,
        _ => Start::Smart,
    };
    state.controller.start(how);
}

/// Control bar buttons.
#[tauri::command]
fn control(state: State<AppState>, action: String) {
    let c = &state.controller;
    match action.as_str() {
        "pause" => c.pause(),
        "stop" => c.stop(),
        "cancel" => c.cancel(),
        _ => {}
    }
}

#[tauri::command]
fn undo_cancel(state: State<AppState>) {
    state.controller.undo_cancel();
}

/// Counts a running export in `AppState::exports` for as long as it lives.
pub struct ExportGuard(AppHandle);

impl ExportGuard {
    pub fn new(app: &AppHandle) -> Self {
        app.state::<AppState>()
            .exports
            .fetch_add(1, Ordering::Relaxed);
        Self(app.clone())
    }
}

impl Drop for ExportGuard {
    fn drop(&mut self) {
        self.0
            .state::<AppState>()
            .exports
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Show the recovery prompt if startup found unsaved sessions.
fn offer_recovery(app: &AppHandle) {
    let count = app.state::<AppState>().recovery.lock().unwrap().len();
    if count > 0 {
        let i = I18n::new(&app.state::<AppState>().settings.lock().unwrap().language);
        ui::toast(app, ui::Toast::recovery(&i, count));
    }
}

/// The toast page has subscribed; anything queued at startup can be shown now.
#[tauri::command]
fn toast_ready(app: AppHandle, state: State<AppState>) {
    state.toast_ready.store(true, Ordering::Relaxed);
    offer_recovery(&app);
}

/// Act on the sessions found at startup (and only those): open them in editors, save them with
/// the current settings, or delete them. Unreadable or empty sessions are removed either way.
/// Async on purpose: creating windows from a synchronous command deadlocks on Windows.
#[tauri::command]
async fn recover(app: AppHandle, action: String) -> Result<(), String> {
    if !matches!(action.as_str(), "open" | "save" | "delete") {
        return Err(format!("unknown recovery action {action}"));
    }
    let sessions = std::mem::take(&mut *app.state::<AppState>().recovery.lock().unwrap());
    let s = app.state::<AppState>().settings.lock().unwrap().clone();
    let i = I18n::new(&s.language);
    let mut failures: Vec<(String, PathBuf)> = Vec::new();
    for session in sessions {
        let usable = zzalcam_core::SessionReader::open(&session).is_ok_and(|r| {
            use zzalcam_core::FrameSource;
            r.frame_count() > 0
        });
        if !usable || action == "delete" {
            std::fs::remove_file(&session).ok();
            continue;
        }
        let result = if action == "open" {
            editor::open(&app, session.clone())
        } else {
            // Encoding takes a while: show progress and keep it off the async runtime.
            ui::toast(&app, ui::Toast::encoding(&i));
            let _busy = ExportGuard::new(&app);
            let (app, s, i2, path) = (
                app.clone(),
                s.clone(),
                I18n::new(&s.language),
                session.clone(),
            );
            tauri::async_runtime::spawn_blocking(move || {
                output::save_session(&app, &s, &i2, &path, None).map(|_| ())
            })
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r)
        };
        if let Err(e) = result {
            failures.push((e.to_string(), session));
        }
    }
    // The toast window is hidden by now, so report failures with a toast of their own.
    if let Some((error, session)) = failures.first() {
        ui::toast(&app, ui::Toast::failed(&i, error).with_detail(session));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures
            .into_iter()
            .map(|(e, _)| e)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Open Explorer with `path` selected.
#[tauri::command]
fn reveal(path: PathBuf) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // Explorer wants `/select,"C:\a b\c.gif"`; default quoting would wrap the whole argument.
    std::process::Command::new("explorer")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn run() {
    zzalcam_capture::init_dpi_awareness();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_settings(app)
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(hotkeys::on_shortcut)
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .register_asynchronous_uri_scheme_protocol("zzalcam", {
            // Decoding and encoding a frame takes milliseconds; keep it off the main thread.
            let workers = Mutex::new(editor::spawn_workers(PROTOCOL_WORKERS));
            move |ctx, req, responder| {
                let app = ctx.app_handle().clone();
                let job: editor::Job =
                    Box::new(move || responder.respond(editor::protocol(&app, &req)));
                let _ = workers.lock().unwrap().send(job);
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let s = settings::load(&handle);
            let lang = I18n::new(&s.language);
            let hk = s.hotkeys.clone();
            app.manage(AppState {
                settings: Mutex::new(s),
                controller: Controller::new(handle.clone()),
                hotkeys: Mutex::new(HashMap::new()),
                conflicts: Mutex::new(Vec::new()),
                hotkeys_suspended: AtomicBool::new(false),
                editors: editor::Editors::default(),
                recovery: Mutex::new(Vec::new()),
                toast_ready: AtomicBool::new(false),
                exports: Default::default(),
            });
            {
                let output = handle
                    .state::<AppState>()
                    .settings
                    .lock()
                    .unwrap()
                    .output_dir
                    .clone();
                let sessions = recorder::session_dir(&handle).ok();
                let app = handle.clone();
                std::thread::spawn(move || {
                    cleanup::stale_files(sessions.as_deref(), &output);
                    // Whatever is left is a recording that was never saved (crash, power loss).
                    let found = sessions
                        .as_deref()
                        .map(cleanup::sessions)
                        .unwrap_or_default();
                    let state = app.state::<AppState>();
                    *state.recovery.lock().unwrap() = found;
                    if state.toast_ready.load(Ordering::Relaxed) {
                        offer_recovery(&app);
                    }
                });
            }
            // Windows are created only now: their pages call commands that need AppState.
            for w in &app.config().app.windows {
                let mut builder = tauri::WebviewWindowBuilder::from_config(&handle, w)?;
                if let Some(dir) = paths::webview_dir() {
                    builder = builder.data_directory(dir);
                }
                builder.build()?;
            }
            ui::exclude_windows(&handle);
            app.manage(updater::Pending::default());
            updater::spawn(handle.clone());
            std::thread::spawn(zzalcam_overlay::warm_up);
            tray::create(&handle, &lang)?;
            let conflicts = hotkeys::apply(&handle, &hk);
            if !conflicts.is_empty() {
                // Make the problem visible right away instead of silently ignoring a hotkey.
                tray::show_settings(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, ev| {
            // App windows hide instead of closing (the bar and toast are reused, the settings
            // window lives in the tray); editors close normally.
            let app_window = matches!(window.label(), "main" | "controlbar" | "toast");
            if let (WindowEvent::CloseRequested { api, .. }, true) = (ev, app_window) {
                api.prevent_close();
                let _ = window.hide();
                // A hotkey capture interrupted by closing the window must not leave hotkeys off.
                set_hotkeys_suspended(window.app_handle(), false);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_language,
            get_conflicts,
            save_settings,
            suspend_hotkeys,
            start,
            control,
            undo_cancel,
            reveal,
            recover,
            toast_ready,
            updater::install_update,
            updater::is_portable,
            editor::editor_info,
            editor::editor_estimate,
            editor::editor_save,
            editor::editor_close
        ])
        .build(tauri::generate_context!())
        .expect("error while building zzalcam")
        .run(|app, ev| {
            if let RunEvent::Exit = ev {
                app.state::<AppState>().controller.shutdown();
            }
        });
}
