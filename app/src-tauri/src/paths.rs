//! Where zzalcam keeps its files: per-user app folders, or `data\` next to the executable in
//! portable mode (a `zzalcam.portable` file beside the exe).

use std::path::PathBuf;
use std::sync::OnceLock;

use tauri::{AppHandle, Manager};

const PORTABLE_MARKER: &str = "zzalcam.portable";

/// `<exe dir>\data` when running portable.
fn portable_root() -> Option<&'static PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        exe_dir
            .join(PORTABLE_MARKER)
            .is_file()
            .then(|| exe_dir.join("data"))
    })
    .as_ref()
}

pub fn is_portable() -> bool {
    portable_root().is_some()
}

/// Settings live here.
pub fn config_dir(app: &AppHandle) -> PathBuf {
    match portable_root() {
        Some(root) => root.clone(),
        None => app
            .path()
            .app_config_dir()
            .unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// Sessions and other bulky local data live here.
pub fn data_dir(app: &AppHandle) -> PathBuf {
    match portable_root() {
        Some(root) => root.clone(),
        None => app
            .path()
            .app_local_data_dir()
            .unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// WebView2 profile folder; `None` keeps Tauri's default (under %LOCALAPPDATA%).
pub fn webview_dir() -> Option<PathBuf> {
    portable_root().map(|r| r.join("webview"))
}
