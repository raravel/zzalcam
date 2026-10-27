//! Tray icon and menu.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::AppState;
use crate::i18n::I18n;
use crate::recorder::Start;

const TRAY_ID: &str = "main";

fn menu(app: &AppHandle, i: &I18n) -> tauri::Result<Menu<Wry>> {
    let item = |id: &str, key: &str| MenuItem::with_id(app, id, i.t(key), true, None::<&str>);
    Menu::with_items(
        app,
        &[
            &item("record", "tray.record")?,
            &item("window", "tray.window")?,
            &item("control", "tray.control")?,
            &item("region", "tray.region")?,
            &item("all", "tray.all")?,
            &PredefinedMenuItem::separator(app)?,
            &item("settings", "tray.settings")?,
            &item("quit", "tray.quit")?,
        ],
    )
}

pub fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn create(app: &AppHandle, i: &I18n) -> tauri::Result<TrayIcon> {
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("bundle has an icon"),
        )
        .tooltip("zzalcam")
        .menu(&menu(app, i)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| {
            let c = app.state::<AppState>().controller.clone();
            match ev.id().as_ref() {
                "record" | "window" => c.start(Start::Smart),
                "control" => c.start(Start::Control),
                "region" => c.start(Start::Region),
                "all" => c.start(Start::AllMonitors),
                "settings" => show_settings(app),
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_settings(tray.app_handle());
            }
        })
        .build(app)
}

/// Rebuild the menu after a language change.
pub fn relabel(app: &AppHandle, i: &I18n) {
    if let (Some(tray), Ok(m)) = (app.tray_by_id(TRAY_ID), menu(app, i)) {
        let _ = tray.set_menu(Some(m));
    }
}
