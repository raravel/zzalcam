//! Startup housekeeping for files a crash or an exit mid-way can leave behind.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// Raw sessions this old are dropped. Recent ones stay for the crash-recovery prompt (M3).
const SESSION_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub fn stale_files(sessions: Option<&Path>, output: &Path) {
    let now = SystemTime::now();
    if let Some(dir) = sessions {
        remove_matching(
            dir,
            |name, age| name.ends_with(".zzcs") && age > SESSION_MAX_AGE,
            now,
        );
    }
    // Half-written outputs from an encode that never finished; only our own naming pattern.
    remove_matching(
        output,
        |name, _| name.starts_with("zzalcam_") && name.ends_with(".part"),
        now,
    );
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
