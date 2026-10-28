//! Startup housekeeping for files a crash or an exit mid-way can leave behind.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// Raw sessions this old are dropped. Recent ones stay for the crash-recovery prompt (M3).
const SESSION_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub fn stale_files(sessions: Option<&Path>, output: &Path) {
    let now = SystemTime::now();
    if let Some(dir) = sessions {
        let old_session = |name: &str, age| name.ends_with(".zzcs") && age > SESSION_MAX_AGE;
        remove_matching(dir, old_session, now);
        remove_matching(&dir.join(KEPT_DIR), old_session, now);
    }
    // Half-written outputs from an encode that never finished; only our own suffix.
    remove_matching(
        output,
        |name, _| name.ends_with(crate::output::PART_SUFFIX),
        now,
    );
}

/// A session that has been saved: delete it, or with "keep sessions" move it to `kept/` so it can
/// be edited again without showing up as unsaved.
pub fn retire_session(session: &Path, keep: bool) {
    if keep {
        if let Some(dir) = session.parent().map(|d| d.join(KEPT_DIR))
            && std::fs::create_dir_all(&dir).is_ok()
            && let Some(name) = session.file_name()
            && std::fs::rename(session, dir.join(name)).is_ok()
        {
            return;
        }
        return; // moving failed: better a spurious recovery prompt than losing the recording
    }
    std::fs::remove_file(session).ok();
}

const KEPT_DIR: &str = "kept";

/// Session files currently in `dir`, oldest first.
pub fn sessions(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "zzcs"))
        .collect();
    found.sort();
    found
}

fn remove_matching(dir: &Path, wanted: impl Fn(&str, Duration) -> bool, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let age = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .unwrap_or_default();
        if wanted(&name, age) {
            std::fs::remove_file(e.path()).ok();
        }
    }
}
