//! Recording state machine: pick a target → countdown → record → encode → save, copy, notify.
//!
//! Every phase runs on its own thread; hotkeys, the tray and the control bar only flip flags.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use zzalcam_capture::{CaptureOptions, RecordControl, ScreenRect, Target};
use zzalcam_core::{FrameSource, SessionReader, SessionWriter};
use zzalcam_overlay::{Mode, RecordingFrame, SelectHandle};

use crate::disk;
use crate::i18n::I18n;
use crate::settings::Settings;
use crate::ui::{self, Toast};
use crate::{AppState, editor, output};

const COUNTDOWN_FROM: u32 = 3;
const STATS_INTERVAL: Duration = Duration::from_millis(250);
/// Check free disk space every this many stats ticks (2 s).
const DISK_CHECK_TICKS: u32 = 8;
/// Rough output size per byte of LZ4 session data, measured on UI recordings (docs/design.md §9).
const GIF_BYTES_PER_SESSION_BYTE: f64 = 0.045;
const WEBP_BYTES_PER_SESSION_BYTE: f64 = 0.03;

#[derive(Debug, Clone, Copy)]
pub enum Start {
    Smart,
    Control,
    Region,
    AllMonitors,
}

#[derive(Clone)]
struct LastTarget {
    target: Target,
    rect: ScreenRect,
    follow: Option<isize>,
}

struct Active {
    ctl: RecordControl,
    cancelled: AtomicBool,
    /// Recording stopped because the session drive is nearly full.
    low_disk: AtomicBool,
    frame: Mutex<Option<RecordingFrame>>,
}

enum Phase {
    Idle,
    /// The picker is open. `handle` arrives once its thread is up; a cancel requested before that
    /// is remembered in `cancel` and applied as soon as the handle arrives.
    Selecting {
        handle: Option<SelectHandle>,
        cancel: bool,
    },
    Recording(Arc<Active>),
    Encoding,
}

impl Phase {
    /// Cancel the picker if it is open (now, or as soon as it can be).
    fn cancel_selecting(&mut self) {
        if let Phase::Selecting { handle, cancel } = self {
            match handle {
                Some(h) => h.cancel(),
                None => *cancel = true,
            }
        }
    }
}

pub struct Controller {
    app: AppHandle,
    phase: Mutex<Phase>,
    last: Mutex<Option<LastTarget>>,
    /// Session of the most recently cancelled recording, kept for "Undo".
    undo: Mutex<Option<PathBuf>>,
}

#[derive(Serialize, Clone)]
struct Stats {
    elapsed_ms: u64,
    frames: usize,
    est_bytes: u64,
    paused: bool,
}

impl Controller {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            phase: Mutex::new(Phase::Idle),
            last: Mutex::new(None),
            undo: Mutex::new(None),
        })
    }

    fn settings(&self) -> Settings {
        self.app
            .state::<AppState>()
            .settings
            .lock()
            .unwrap()
            .clone()
    }

    fn i18n(&self) -> I18n {
        I18n::new(&self.settings().language)
    }

    fn set_idle(&self) {
        *self.phase.lock().unwrap() = Phase::Idle;
    }

    /// The main hotkey: pick a target when idle, stop when recording, back out of the picker.
    pub fn toggle(self: &Arc<Self>) {
        let mut phase = self.phase.lock().unwrap();
        match &mut *phase {
            Phase::Idle => {
                drop(phase);
                self.start(Start::Smart);
            }
            p @ Phase::Selecting { .. } => p.cancel_selecting(),
            Phase::Recording(a) => a.ctl.stop.store(true, Ordering::Relaxed),
            Phase::Encoding => {}
        }
    }

    pub fn start(self: &Arc<Self>, how: Start) {
        let mode = match how {
            Start::AllMonitors => {
                if !matches!(*self.phase.lock().unwrap(), Phase::Idle) {
                    return;
                }
                match zzalcam_capture::virtual_desktop() {
                    Ok(rect) => self.begin(
                        LastTarget {
                            target: Target::AllMonitors,
                            rect,
                            follow: None,
                        },
                        false,
                    ),
                    Err(e) => self.fail(&e.to_string()),
                }
                return;
            }
            Start::Smart => Mode::Smart,
            Start::Control => Mode::Control,
            Start::Region => Mode::RegionOnly,
        };
        {
            let mut phase = self.phase.lock().unwrap();
            if !matches!(*phase, Phase::Idle) {
                return;
            }
            *phase = Phase::Selecting {
                handle: None,
                cancel: false,
            };
        }
        let this = self.clone();
        let strings = self.i18n().overlay();
        std::thread::spawn(move || {
            let picked = zzalcam_overlay::select(mode, strings, |h| {
                if let Phase::Selecting { handle, cancel } = &mut *this.phase.lock().unwrap() {
                    if *cancel {
                        h.cancel();
                    }
                    *handle = Some(h);
                }
            });
            match picked {
                Ok(Some(sel)) => {
                    let follow = match sel.target {
                        Target::Window(h) => Some(h),
                        _ => None,
                    };
                    // Straight from Selecting to Recording, so nothing can slip in between.
                    this.begin(
                        LastTarget {
                            target: sel.target,
                            rect: sel.rect,
                            follow,
                        },
                        true,
                    );
                }
                Ok(None) => this.set_idle(),
                Err(e) => {
                    this.set_idle();
                    this.fail(&e.to_string());
                }
            }
        });
    }

    pub fn repeat(self: &Arc<Self>) {
        let last = self.last.lock().unwrap().clone();
        match last {
            Some(mut t) => {
                // Windows may have moved and the monitor layout may have changed since.
                let fresh = match t.target {
                    Target::Window(h) => zzalcam_capture::window_bounds(h),
                    Target::Monitor(i) => zzalcam_capture::monitors()
                        .ok()
                        .and_then(|m| m.get(i).map(|m| m.rect)),
                    Target::AllMonitors => zzalcam_capture::virtual_desktop().ok(),
                    Target::Region(r) => Some(r),
                };
                match fresh {
                    Some(r) => t.rect = r,
                    None => return self.start(Start::Smart),
                }
                self.begin(t, false);
            }
            None => self.start(Start::Smart),
        }
    }

    pub fn pause(&self) {
        if let Phase::Recording(a) = &*self.phase.lock().unwrap() {
            let paused = !a.ctl.paused.fetch_xor(true, Ordering::Relaxed);
            if let Some(f) = &*a.frame.lock().unwrap() {
                f.set_paused(paused);
            }
        }
    }

    pub fn stop(&self) {
        if let Phase::Recording(a) = &*self.phase.lock().unwrap() {
            a.ctl.stop.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel(&self) {
        match &mut *self.phase.lock().unwrap() {
            Phase::Recording(a) => {
                a.cancelled.store(true, Ordering::Relaxed);
                a.ctl.stop.store(true, Ordering::Relaxed);
            }
            p @ Phase::Selecting { .. } => p.cancel_selecting(),
            _ => {}
        }
    }

    /// On app exit: abandon any recording, wait briefly for it to wind down, and drop the
    /// undo session (it can't be undone after exit).
    pub fn shutdown(&self) {
        self.cancel();
        for _ in 0..60 {
            if matches!(*self.phase.lock().unwrap(), Phase::Idle | Phase::Encoding) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(s) = self.undo.lock().unwrap().take() {
            std::fs::remove_file(s).ok();
        }
    }

    /// Encode the last cancelled recording after all.
    pub fn undo_cancel(self: &Arc<Self>) {
        let Some(session) = self.undo.lock().unwrap().take() else {
            return;
        };
        {
            let mut phase = self.phase.lock().unwrap();
            if !matches!(*phase, Phase::Idle) {
                *self.undo.lock().unwrap() = Some(session);
                return;
            }
            *phase = Phase::Encoding;
        }
        let this = self.clone();
        std::thread::spawn(move || this.deliver(session, None));
    }

    /// Hand a finished session to the editor or save it right away, per settings.
    /// Expects the phase to be leaving Recording (or Encoding for undo).
    /// `note` explains an early stop (low disk); it goes on the saved toast, or on its own toast
    /// when the editor opens instead.
    fn deliver(&self, session: PathBuf, note: Option<String>) {
        if self.settings().after_record == "editor" {
            self.set_idle();
            if let Some(n) = note {
                ui::toast(&self.app, Toast::warning(n));
            }
            if let Err(e) = editor::open(&self.app, session.clone()) {
                self.fail_keeping(&e.to_string(), &session);
            }
        } else {
            *self.phase.lock().unwrap() = Phase::Encoding;
            self.encode_and_save(&session, note.as_deref());
        }
    }

    /// Start recording `t`; `from_selecting` says which phase we expect to leave.
    fn begin(self: &Arc<Self>, t: LastTarget, from_selecting: bool) {
        let active = Arc::new(Active {
            ctl: RecordControl::default(),
            cancelled: AtomicBool::new(false),
            low_disk: AtomicBool::new(false),
            frame: Mutex::new(None),
        });
        {
            let mut phase = self.phase.lock().unwrap();
            let expected = match *phase {
                Phase::Idle => !from_selecting,
                Phase::Selecting { .. } => from_selecting,
                _ => false,
            };
            if !expected {
                return;
            }
            *phase = Phase::Recording(active.clone());
        }
        *self.last.lock().unwrap() = Some(t.clone());
        let this = self.clone();
        std::thread::spawn(move || this.record(t, active));
    }

    fn record(self: &Arc<Self>, t: LastTarget, active: Arc<Active>) {
        let s = self.settings();
        let result = self.record_session(&t, &active, &s);
        // Closes the border.
        drop(active.frame.lock().unwrap().take());
        ui::hide_bar(&self.app);

        match result {
            Ok(None) => self.set_idle(), // stopped during the countdown
            Ok(Some(session)) if active.cancelled.load(Ordering::Relaxed) => {
                *self.undo.lock().unwrap() = Some(session.clone());
                self.set_idle();
                let i = self.i18n();
                ui::toast(&self.app, Toast::cancelled(&i, session));
            }
            Ok(Some(session)) => {
                let note = active
                    .low_disk
                    .load(Ordering::Relaxed)
                    .then(|| self.i18n().t("toast.low_disk"));
                self.deliver(session, note);
            }
            Err(e) => {
                self.set_idle();
                self.fail(&e.to_string());
            }
        }
    }

    /// Returns the session file, or `None` if the user stopped before recording began.
    fn record_session(
        &self,
        t: &LastTarget,
        active: &Active,
        s: &Settings,
    ) -> anyhow::Result<Option<PathBuf>> {
        let frame = RecordingFrame::show(t.rect, t.follow)?;
        let paused = || active.ctl.paused.load(Ordering::Relaxed);
        *active.frame.lock().unwrap() = Some(frame);
        ui::show_bar(&self.app, t.rect);

        if s.countdown {
            for n in (1..=COUNTDOWN_FROM).rev() {
                if let Some(f) = &*active.frame.lock().unwrap() {
                    f.countdown(Some(n));
                }
                for _ in 0..20 {
                    if active.ctl.stop.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if let Some(f) = &*active.frame.lock().unwrap() {
                f.countdown(None);
            }
        }

        let mut cap = zzalcam_capture::open(
            &t.target,
            &CaptureOptions {
                fps: s.fps,
                cursor: s.cursor,
            },
        )?;
        let (w, h) = cap.size();
        let max = s.format().max_dimension();
        anyhow::ensure!(
            w <= max && h <= max,
            "{w}x{h} exceeds the {max}px limit of {}",
            s.format
        );

        let dir = session_dir(&self.app)?;
        if disk::free_bytes(&dir).is_some_and(|f| f < disk::WARN_BELOW) {
            let i = self.i18n();
            ui::toast(&self.app, Toast::warning(i.t("toast.disk_warning")));
        }
        let session = dir.join(format!(
            "{}.zzcs",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
        ));
        let writer = SessionWriter::create(&session, w, h)?;
        // A new recording is under way: the previous cancelled one can no longer be undone.
        if let Some(old) = self.undo.lock().unwrap().take() {
            std::fs::remove_file(old).ok();
        }

        let ratio = if s.format == "webp" {
            WEBP_BYTES_PER_SESSION_BYTE
        } else {
            GIF_BYTES_PER_SESSION_BYTE
        };
        let done = AtomicBool::new(false);
        let recorded = std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut ticks = 0u32;
                while !done.load(Ordering::Relaxed) {
                    ticks += 1;
                    if ticks.is_multiple_of(DISK_CHECK_TICKS)
                        && disk::free_bytes(&dir).is_some_and(|f| f < disk::STOP_BELOW)
                    {
                        active.low_disk.store(true, Ordering::Relaxed);
                        active.ctl.stop.store(true, Ordering::Relaxed);
                    }
                    let c = &active.ctl;
                    let stats = Stats {
                        elapsed_ms: c.elapsed_ms.load(Ordering::Relaxed),
                        frames: c.frames.load(Ordering::Relaxed),
                        est_bytes: (c.session_bytes.load(Ordering::Relaxed) as f64 * ratio) as u64,
                        paused: paused(),
                    };
                    let _ = self.app.emit_to("controlbar", "rec-stats", stats);
                    std::thread::sleep(STATS_INTERVAL);
                }
            });
            let r = zzalcam_capture::record(cap.as_mut(), writer, s.fps, None, &active.ctl);
            done.store(true, Ordering::Relaxed);
            r
        });
        drop(cap);
        // A capture error mid-way still leaves a usable session; only give up if nothing was recorded.
        if let Err(e) = recorded {
            let usable = SessionReader::open(&session).is_ok_and(|r| r.frame_count() > 0);
            if !usable {
                std::fs::remove_file(&session).ok();
                return Err(e.into());
            }
        }
        Ok(Some(session))
    }

    fn encode_and_save(&self, session: &Path, note: Option<&str>) {
        let i = self.i18n();
        ui::toast(&self.app, Toast::encoding(&i));
        let s = self.settings();
        let result = output::save_session(&self.app, &s, &i, session, note);
        self.set_idle();
        if let Err(e) = result {
            self.fail_keeping(&e.to_string(), session);
        }
    }

    /// Report a failure but keep the recording; the toast says where it is.
    fn fail_keeping(&self, error: &str, session: &Path) {
        ui::toast(
            &self.app,
            Toast::failed(&self.i18n(), error).with_detail(session),
        );
    }

    fn fail(&self, error: &str) {
        ui::toast(&self.app, Toast::failed(&self.i18n(), error));
    }
}

pub fn session_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let dir = app.path().app_local_data_dir()?.join("sessions");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
