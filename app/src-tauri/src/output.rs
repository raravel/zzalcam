//! Writing finished recordings: file naming, atomic writes, clipboard and the result toast.

use std::path::{Path, PathBuf};

use tauri::AppHandle;
use zzalcam_core::{FrameSource, SessionReader};
use zzalcam_encode::export::{Adjustment, ExportOptions, ExportReport, export};

use crate::i18n::I18n;
use crate::settings::Settings;
use crate::ui::{self, Toast};
use crate::{cleanup, clipboard};

/// Export options from the user's settings.
pub fn export_options(s: &Settings) -> ExportOptions {
    ExportOptions {
        format: s.format(),
        preset: s.preset(),
        scale: s.export_scale,
        max_fps: None,
        max_bytes: s
            .max_size_mb
            .filter(|mb| *mb > 0.0)
            .map(|mb| (mb * 1024.0 * 1024.0) as u64),
    }
}

/// Expand the filename pattern: `{date}` = yyyyMMdd, `{time}` = HHmmss, and `{yyyy}` `{MM}` `{dd}`
/// `{HH}` `{mm}` `{ss}`. Characters Windows forbids in file names are replaced.
pub fn file_stem(pattern: &str) -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let stem = pattern
        .replace(
            "{date}",
            &format!("{:04}{:02}{:02}", t.wYear, t.wMonth, t.wDay),
        )
        .replace(
            "{time}",
            &format!("{:02}{:02}{:02}", t.wHour, t.wMinute, t.wSecond),
        )
        .replace("{yyyy}", &format!("{:04}", t.wYear))
        .replace("{MM}", &format!("{:02}", t.wMonth))
        .replace("{dd}", &format!("{:02}", t.wDay))
        .replace("{HH}", &format!("{:02}", t.wHour))
        .replace("{mm}", &format!("{:02}", t.wMinute))
        .replace("{ss}", &format!("{:02}", t.wSecond));
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut clean: String = clean
        .trim()
        .trim_end_matches('.')
        .chars()
        .take(MAX_STEM_CHARS)
        .collect();
    if clean.is_empty() {
        clean = "zzalcam".into();
    }
    // Device names are reserved with any extension (CON.gif, LPT1.webp, ...).
    let upper = clean.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if reserved {
        clean.insert(0, '_');
    }
    clean
}

const MAX_STEM_CHARS: usize = 120;
/// Suffix of half-written outputs; startup cleanup removes leftovers.
pub const PART_SUFFIX: &str = ".zzalcam.part";

/// Claim `<dir>/<stem>.<ext>` (or `<stem>_2.<ext>`, ...) by creating it empty, so two saves in the
/// same second never pick the same name. The caller replaces or removes the placeholder.
pub fn reserve_path(dir: &Path, stem: &str, ext: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for n in 1.. {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Encode to `target` atomically: write `<target>.part`, rename on success.
pub fn export_to(
    src: &dyn FrameSource,
    opts: &ExportOptions,
    target: &Path,
) -> anyhow::Result<ExportReport> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (bytes, report) = export(src, opts, &|_, _| {})?;
    let mut part = target.as_os_str().to_owned();
    part.push(PART_SUFFIX);
    let part = PathBuf::from(part);
    if let Err(e) = std::fs::write(&part, &bytes).and_then(|_| std::fs::rename(&part, target)) {
        std::fs::remove_file(&part).ok();
        return Err(e.into());
    }
    Ok(report)
}

/// Short human summary of what a size target changed, e.g. "fps 10 · 75% · 128 colours".
pub fn describe(i: &I18n, adjustments: &[Adjustment]) -> String {
    adjustments
        .iter()
        .map(|a| match a {
            Adjustment::Quality(q) => i.tf("adjust.quality", &[("n", &q.to_string())]),
            Adjustment::Colors(c) => i.tf("adjust.colors", &[("n", &c.to_string())]),
            Adjustment::Tolerance(t) => i.tf("adjust.tolerance", &[("n", &t.to_string())]),
            Adjustment::Fps(f) => i.tf("adjust.fps", &[("n", &f.to_string())]),
            Adjustment::Scale(s) => i.tf(
                "adjust.scale",
                &[("n", &((s * 100.0).round() as u32).to_string())],
            ),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Encode a whole session with the user's settings into the output folder and announce it.
/// The session is deleted (or moved to the kept folder) afterwards.
pub fn save_session(
    app: &AppHandle,
    s: &Settings,
    i: &I18n,
    session: &Path,
    note: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let opts = export_options(s);
    let target = reserve_path(
        &s.output_dir,
        &file_stem(&s.filename_pattern),
        opts.format.extension(),
    )?;
    let result = SessionReader::open(session)
        .map_err(anyhow::Error::from)
        .and_then(|src| export_to(&src, &opts, &target));
    match result {
        Ok(report) => {
            cleanup::retire_session(session, s.keep_sessions);
            announce(app, i, &target, &report, note);
            Ok(target)
        }
        Err(e) => {
            std::fs::remove_file(&target).ok(); // the empty placeholder
            Err(e)
        }
    }
}

/// After a successful save: copy to the clipboard and tell the user.
/// `note` is appended to the details (e.g. why recording stopped early).
pub fn announce(app: &AppHandle, i: &I18n, path: &Path, report: &ExportReport, note: Option<&str>) {
    let copied = clipboard::copy_file(path).is_ok();
    let mut toast = Toast::saved(i, path, copied);
    if let Some(n) = note {
        toast.detail = format!("{n} · {}", toast.detail);
    }
    if !report.adjustments.is_empty() {
        toast.detail = format!("{} · {}", toast.detail, describe(i, &report.adjustments));
    }
    if report.over_target {
        toast.title = i.t("toast.over_target");
    }
    ui::toast(app, toast);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_expands_tokens_and_strips_invalid_chars() {
        let s = file_stem("clip_{date}-{time}");
        assert!(
            s.starts_with("clip_20") && s.len() == "clip_yyyyMMdd-HHmmss".len(),
            "{s}"
        );
        assert_eq!(file_stem("a/b:c*"), "a_b_c_");
        assert_eq!(file_stem("  ..  "), "zzalcam");
        assert_eq!(file_stem("con"), "_con");
        assert_eq!(file_stem("LPT1"), "_LPT1");
        assert_eq!(file_stem("console"), "console");
    }
}
