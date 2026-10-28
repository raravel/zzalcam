//! Free disk space checks for the session drive.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::PCWSTR;

/// Warn before recording when less than this is free.
pub const WARN_BELOW: u64 = 2 << 30;
/// Stop a running recording when less than this is free.
pub const STOP_BELOW: u64 = 500 << 20;

/// Bytes available to the current user on the volume holding `dir`.
pub fn free_bytes(dir: &Path) -> Option<u64> {
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}
