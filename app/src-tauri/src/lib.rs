//! zzalcam desktop app: tray, hotkeys, target picker, recording and the settings UI.

mod cleanup;
mod clipboard;
mod hotkeys;
mod i18n;
mod recorder;
mod settings;
mod tray;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

use crate::i18n::I18n;
use crate::recorder::{Controller, Start};
use crate::settings::Settings;

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub controller: Arc<Controller>,
    pub hotkeys: Mutex<HashMap<u32, hotkeys::Action>>,
    /// Actions whose hotkey could not be registered.
    pub conflicts: Mutex<Vec<&'static str>>,
    /// The settings window is capturing a new key combination; hotkeys stay unregistered.
    pub hotkeys_suspended: AtomicBool,
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
                std::thread::spawn(move || cleanup::stale_files(sessions.as_deref(), &output));
            }
            // Windows are created only now: their pages call commands that need AppState.
            for w in &app.config().app.windows {
                tauri::WebviewWindowBuilder::from_config(&handle, w)?.build()?;
            }
            ui::exclude_windows(&handle);
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
            // Closing the settings window keeps the app in the tray.
            if let WindowEvent::CloseRequested { api, .. } = ev {
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
            reveal
        ])
        .build(tauri::generate_context!())
        .expect("error while building zzalcam")
        .run(|app, ev| {
            if let RunEvent::Exit = ev {
                app.state::<AppState>().controller.shutdown();
            }
        });
}
