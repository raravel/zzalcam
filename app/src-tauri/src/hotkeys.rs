//! Global hotkeys. Registration failures mean another program owns the combination; they are
//! reported to the settings window as conflicts instead of failing startup.

use std::collections::HashMap;
use std::str::FromStr;

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::AppState;
use crate::settings::Hotkeys;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Record,
    Pause,
    Repeat,
    Cancel,
}

impl Action {
    pub fn key(self) -> &'static str {
        match self {
            Action::Record => "record",
            Action::Pause => "pause",
            Action::Repeat => "repeat",
            Action::Cancel => "cancel",
        }
    }
}

/// Replace all registrations with `hk`. Returns the actions whose hotkey could not be registered.
pub fn apply(app: &AppHandle, hk: &Hotkeys) -> Vec<&'static str> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let mut map = HashMap::new();
    let mut conflicts = Vec::new();
    for (action, combo) in [
        (Action::Record, &hk.record),
        (Action::Pause, &hk.pause),
        (Action::Repeat, &hk.repeat),
        (Action::Cancel, &hk.cancel),
    ] {
        if combo.trim().is_empty() {
            continue; // unassigned
        }
        match Shortcut::from_str(combo) {
            Ok(sc) if map.contains_key(&sc.id()) => conflicts.push(action.key()),
            Ok(sc) => match gs.register(sc) {
                Ok(()) => {
                    map.insert(sc.id(), action);
                }
                Err(_) => conflicts.push(action.key()),
            },
            Err(_) => conflicts.push(action.key()),
        }
    }
    let state = app.state::<AppState>();
    *state.hotkeys.lock().unwrap() = map;
    *state.conflicts.lock().unwrap() = conflicts.clone();
    conflicts
}

pub fn on_shortcut(app: &AppHandle, sc: &Shortcut, ev: ShortcutEvent) {
    if ev.state != ShortcutState::Pressed {
        return;
    }
    let state = app.state::<AppState>();
    let Some(action) = state.hotkeys.lock().unwrap().get(&sc.id()).copied() else {
        return;
    };
    let c = &state.controller;
    match action {
        Action::Record => c.toggle(),
        Action::Pause => c.pause(),
        Action::Repeat => c.repeat(),
        Action::Cancel => c.cancel(),
    }
}
