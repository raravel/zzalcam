//! Persistent user settings (`%APPDATA%\dev.zzalcam.app\settings.json`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use zzalcam_encode::{Format, Preset};

const DEFAULT_PATTERN: &str = "zzalcam_{date}_{time}";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Hotkeys {
    pub record: String,
    pub pause: String,
    pub repeat: String,
    pub cancel: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        // One-handed (left hand) and clear of Windows / common app shortcuts; see docs/design.md.
        Self {
            record: "Alt+Shift+1".into(),
            pause: "Alt+Shift+2".into(),
            repeat: "Alt+Shift+3".into(),
            cancel: "Alt+Shift+X".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `auto`, `ko` or `en`.
    pub language: String,
    pub fps: u32,
    pub cursor: bool,
    pub countdown: bool,
    /// `gif` or `webp`.
    pub format: String,
    /// `best`, `balanced` or `smallest`.
    pub preset: String,
    pub output_dir: PathBuf,
    pub hotkeys: Hotkeys,
    /// `editor` (open the editor) or `save` (encode right away).
    pub after_record: String,
    /// Keep raw sessions after saving so they can be edited again.
    pub keep_sessions: bool,
    /// See `output::file_stem` for tokens.
    pub filename_pattern: String,
    /// Default export scale, 0.1–1.0.
    pub export_scale: f32,
    /// Default size target in MB; `None` = no limit.
    pub max_size_mb: Option<f64>,
    pub autostart: bool,
    /// Check GitHub Releases for updates (ignored in portable mode).
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            fps: 15,
            cursor: true,
            countdown: true,
            format: "gif".into(),
            preset: "balanced".into(),
            output_dir: PathBuf::new(),
            hotkeys: Hotkeys::default(),
            after_record: "editor".into(),
            keep_sessions: false,
            filename_pattern: DEFAULT_PATTERN.into(),
            export_scale: 1.0,
            max_size_mb: None,
            autostart: false,
            auto_update: true,
        }
    }
}

impl Settings {
    pub fn format(&self) -> Format {
        if self.format == "webp" {
            Format::WebP
        } else {
            Format::Gif
        }
    }

    pub fn preset(&self) -> Preset {
        match self.preset.as_str() {
            "best" => Preset::Best,
            "smallest" => Preset::Smallest,
            _ => Preset::Balanced,
        }
    }

    /// Clamp values a hand-edited file could break.
    fn sanitize(mut self, app: &AppHandle) -> Self {
        self.fps = self.fps.clamp(5, 60);
        // Normalise free-form strings a hand-edited file could break (the format becomes a file
        // extension).
        self.format = self.format().extension().to_string();
        self.preset = match self.preset() {
            Preset::Best => "best",
            Preset::Balanced => "balanced",
            Preset::Smallest => "smallest",
        }
        .to_string();
        if !matches!(self.language.as_str(), "auto" | "ko" | "en") {
            self.language = "auto".into();
        }
        if self.after_record != "save" {
            self.after_record = "editor".into();
        }
        if self.filename_pattern.trim().is_empty() {
            self.filename_pattern = DEFAULT_PATTERN.into();
        }
        self.export_scale = if self.export_scale.is_finite() {
            self.export_scale.clamp(0.1, 1.0)
        } else {
            1.0
        };
        self.max_size_mb = self.max_size_mb.filter(|m| m.is_finite() && *m > 0.0);
        if self.output_dir.as_os_str().is_empty() {
            self.output_dir = default_output_dir(app);
        }
        self
    }
}

fn default_output_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .picture_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("zzalcam")
}

fn file(app: &AppHandle) -> PathBuf {
    crate::paths::config_dir(app).join("settings.json")
}

pub fn load(app: &AppHandle) -> Settings {
    let s = std::fs::read_to_string(file(app))
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .unwrap_or_default();
    s.sanitize(app)
}

pub fn save(app: &AppHandle, s: Settings) -> anyhow::Result<Settings> {
    let s = s.sanitize(app);
    let path = file(app);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&path, serde_json::to_string_pretty(&s)?.as_bytes())?;
    Ok(s)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}
