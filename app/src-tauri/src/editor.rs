//! Editor windows: one per session, fed frames through the `zzalcam://` protocol.
//!
//! `zzalcam://localhost/<session-id>/frame/<i>?w=<width>` returns frame `i` as a lossy WebP, scaled
//! down to `width` when given (thumbnails). Edits stay in the web UI until export, where they become
//! an [`EditSpec`] applied on the fly; the session file itself is never rewritten.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use zzalcam_core::{EditSpec, EditedSource, FrameSource, Rect, SessionCursor, SessionReader};
use zzalcam_encode::export::{ExportOptions, estimate_size};
use zzalcam_encode::transform::resize_rgba;
use zzalcam_encode::{Format, Preset};

use crate::AppState;
use crate::i18n::I18n;
use crate::output;

const PREVIEW_QUALITY: f32 = 85.0;
const THUMB_QUALITY: f32 = 70.0;

pub struct EditorSession {
    pub path: PathBuf,
    /// Separate cursors so filmstrip thumbnails don't make the preview replay from keyframes.
    preview: Mutex<SessionCursor>,
    thumbs: Mutex<SessionCursor>,
    /// Set when the window closed: `Some(keep_sessions)`; applied when the session is dropped.
    retire: Mutex<Option<bool>>,
}

#[derive(Default)]
pub struct Editors(Mutex<HashMap<String, Arc<EditorSession>>>);

impl Editors {
    fn get(&self, id: &str) -> Result<Arc<EditorSession>, String> {
        self.0
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown editor session {id}"))
    }
}

/// Window label / protocol id for a session: its file stem, restricted to safe characters.
fn session_id(session: &std::path::Path) -> String {
    let stem = session
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Open an editor window for a recorded session (focus it if already open).
/// Must not be called from a synchronous command: creating windows there deadlocks on Windows.
pub fn open(app: &AppHandle, session: PathBuf) -> anyhow::Result<()> {
    let id = session_id(&session);
    let label = format!("editor-{id}");
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }
    let reader = SessionReader::open(&session)?;
    anyhow::ensure!(reader.frame_count() > 0, "nothing was recorded");
    let editor = Arc::new(EditorSession {
        preview: Mutex::new(SessionCursor::new(SessionReader::open(&session)?)?),
        thumbs: Mutex::new(SessionCursor::new(reader)?),
        path: session,
        retire: Mutex::new(None),
    });
    let editors = &app.state::<AppState>().editors;
    editors.0.lock().unwrap().insert(id.clone(), editor);

    let i = I18n::new(&app.state::<AppState>().settings.lock().unwrap().language);
    let mut builder = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("index.html?editor={id}").into()),
    )
    .title(i.t("editor.title"))
    .inner_size(1120.0, 760.0)
    .min_inner_size(760.0, 520.0)
    .center();
    if let Some(dir) = crate::paths::webview_dir() {
        builder = builder.data_directory(dir);
    }
    let built = builder.build();
    if let Err(e) = built {
        editors.0.lock().unwrap().remove(&id);
        return Err(e.into());
    }
    Ok(())
}

/// A request waiting for a protocol worker.
pub type Job = Box<dyn FnOnce() + Send>;

/// A few workers serve frame requests: the filmstrip fires dozens at once, and one OS thread per
/// request would decode them all in parallel (and out of order) for no gain.
pub fn spawn_workers(n: usize) -> std::sync::mpsc::Sender<Job> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..n {
        let rx = rx.clone();
        std::thread::spawn(move || {
            loop {
                let job = rx.lock().unwrap().recv();
                match job {
                    // A panicking request must not take the worker down with it.
                    Ok(job) => drop(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))),
                    Err(_) => break,
                }
            }
        });
    }
    tx
}

/// Serve `zzalcam://localhost/<id>/{frame|thumb}/<i>[?w=<width>]`.
pub fn protocol(app: &AppHandle, req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let respond = |status: StatusCode, body: Vec<u8>, mime: &str, cache: &str| {
        Response::builder()
            .status(status)
            .header("Content-Type", mime)
            .header("Cache-Control", cache)
            .body(body)
            .unwrap()
    };
    match frame_image(app, req) {
        // A session's frames never change, so they can be cached for good.
        Ok(webp) => respond(
            StatusCode::OK,
            webp,
            "image/webp",
            "max-age=31536000, immutable",
        ),
        // Failures can be transient (editor closing, I/O): never cache them.
        Err(e) => respond(
            StatusCode::NOT_FOUND,
            e.into_bytes(),
            "text/plain",
            "no-store",
        ),
    }
}

fn frame_image(app: &AppHandle, req: &Request<Vec<u8>>) -> Result<Vec<u8>, String> {
    let uri = req.uri();
    let parts: Vec<&str> = uri.path().trim_start_matches('/').split('/').collect();
    let [id, kind @ ("frame" | "thumb"), index] = parts[..] else {
        return Err("bad path".into());
    };
    let index: usize = index.parse().map_err(|_| "bad frame index")?;
    let width: Option<u32> = uri
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("w=")))
        .and_then(|w| w.parse().ok())
        .filter(|w| *w >= 1);

    let editor = app.state::<AppState>().editors.get(id)?;
    let (rgba, w, h) = {
        let mut cur = if kind == "thumb" {
            editor.thumbs.lock().unwrap()
        } else {
            editor.preview.lock().unwrap()
        };
        let (w, h) = (cur.reader().width(), cur.reader().height());
        (cur.seek(index).map_err(|e| e.to_string())?.to_vec(), w, h)
    };
    match width.filter(|tw| *tw < w) {
        Some(tw) => {
            let th = ((h as u64 * tw as u64) / w as u64).max(1) as u32;
            let mut resizer = fast_image_resize::Resizer::new();
            let small = resize_rgba(rgba, w, h, tw, th, &mut resizer).map_err(|e| e.to_string())?;
            zzalcam_encode::webp::encode_still(&small, tw, th, THUMB_QUALITY)
                .map_err(|e| e.to_string())
        }
        None => zzalcam_encode::webp::encode_still(&rgba, w, h, PREVIEW_QUALITY)
            .map_err(|e| e.to_string()),
    }
}

#[derive(Serialize)]
pub struct EditorInfo {
    width: u32,
    height: u32,
    /// `(timestamp_ms, duration_ms)` per frame.
    timings: Vec<(u64, u64)>,
    defaults: ExportDto,
}

#[derive(Deserialize)]
pub struct EditDto {
    trim: Option<(usize, usize)>,
    #[serde(default)]
    deleted: Vec<usize>,
    /// `[x, y, w, h]` in recorded pixels.
    crop: Option<[u32; 4]>,
}

impl From<EditDto> for EditSpec {
    fn from(d: EditDto) -> Self {
        EditSpec {
            trim: d.trim,
            deleted: d.deleted,
            crop: d.crop.map(|[x, y, w, h]| Rect::new(x, y, w, h)),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ExportDto {
    format: String,
    preset: String,
    scale: f32,
    /// Maximum file size in MB, if any.
    max_mb: Option<f64>,
}

impl ExportDto {
    fn options(&self) -> ExportOptions {
        ExportOptions {
            format: if self.format == "webp" {
                Format::WebP
            } else {
                Format::Gif
            },
            preset: match self.preset.as_str() {
                "best" => Preset::Best,
                "smallest" => Preset::Smallest,
                _ => Preset::Balanced,
            },
            scale: self.scale.clamp(0.1, 1.0),
            max_fps: None,
            max_bytes: self
                .max_mb
                .filter(|m| *m > 0.0)
                .map(|m| (m * 1024.0 * 1024.0) as u64),
        }
    }
}

#[tauri::command]
pub fn editor_info(state: State<AppState>, id: String) -> Result<EditorInfo, String> {
    let editor = state.editors.get(&id)?;
    let cur = editor.preview.lock().unwrap();
    let s = state.settings.lock().unwrap();
    Ok(EditorInfo {
        width: cur.reader().width(),
        height: cur.reader().height(),
        timings: cur.reader().timings(),
        defaults: ExportDto {
            format: s.format.clone(),
            preset: s.preset.clone(),
            scale: s.export_scale,
            max_mb: s.max_size_mb,
        },
    })
}

/// Predicted size of the export with these edits and settings (ignores the size target).
#[tauri::command]
pub async fn editor_estimate(
    state: State<'_, AppState>,
    id: String,
    edit: EditDto,
    export: ExportDto,
) -> Result<u64, String> {
    let editor = state.editors.get(&id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let reader = SessionReader::open(&editor.path).map_err(|e| e.to_string())?;
        let edited = EditedSource::new(&reader, &edit.into()).map_err(|e| e.to_string())?;
        if edited.frame_count() == 0 {
            return Ok(0);
        }
        estimate_size(&edited, &export.options()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Saved {
    path: PathBuf,
    bytes: u64,
}

/// Export with the edits; `path` = None saves to the output folder with the filename pattern.
#[tauri::command]
pub async fn editor_save(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    edit: EditDto,
    export: ExportDto,
    path: Option<PathBuf>,
) -> Result<Saved, String> {
    let editor = state.editors.get(&id)?;
    let s = state.settings.lock().unwrap().clone();
    let busy = crate::ExportGuard::new(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        let opts = export.options();
        let ext = opts.format.extension();
        // The file always gets the extension of the format actually written.
        let path = match path {
            Some(p) if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) => {
                // The dialog confirmed overwriting the name it showed, not this one: never clobber.
                let dir = p
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default();
                let stem = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("zzalcam")
                    .to_string();
                let reserved = output::reserve_path(&dir, &stem, ext).map_err(|e| e.to_string())?;
                Some(reserved)
            }
            other => other,
        };
        let reader = SessionReader::open(&editor.path).map_err(|e| e.to_string())?;
        let edited = EditedSource::new(&reader, &edit.into()).map_err(|e| e.to_string())?;
        if edited.frame_count() == 0 {
            return Err(I18n::new(&s.language).t("editor.empty"));
        }
        // Without an explicit path, claim a name in the output folder (placeholder file).
        let (target, reserved) = match path {
            Some(p) => (p, false),
            None => {
                let stem = output::file_stem(&s.filename_pattern);
                (
                    output::reserve_path(&s.output_dir, &stem, ext).map_err(|e| e.to_string())?,
                    true,
                )
            }
        };
        let report = match output::export_to(&edited, &opts, &target) {
            Ok(r) => r,
            Err(e) => {
                if reserved {
                    std::fs::remove_file(&target).ok();
                }
                return Err(e.to_string());
            }
        };
        output::announce(&app, &I18n::new(&s.language), &target, &report, None);
        let bytes = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
        Ok(Saved {
            path: target,
            bytes,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The editor window closed: forget the session. Its file is deleted (or kept, per settings) once
/// the last user is gone, so an estimate or save still running keeps working until it finishes.
#[tauri::command]
pub fn editor_close(state: State<AppState>, id: String) {
    if let Some(editor) = state.editors.0.lock().unwrap().remove(&id) {
        let keep = state.settings.lock().unwrap().keep_sessions;
        *editor.retire.lock().unwrap() = Some(keep);
    }
}

impl Drop for EditorSession {
    fn drop(&mut self) {
        if let Some(keep) = *self.retire.lock().unwrap() {
            crate::cleanup::retire_session(&self.path, keep);
        }
    }
}
