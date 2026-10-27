//! Put a file on the clipboard the way Explorer does (CF_HDROP), so it pastes into chat apps as a file.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::DROPFILES;

const OPEN_RETRIES: u32 = 10;

pub fn copy_file(path: &Path) -> windows::core::Result<()> {
    // DROPFILES header, then a double-NUL-terminated list of wide paths.
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.extend([0, 0]);
    let header = size_of::<DROPFILES>();
    let bytes = header + wide.len() * 2;

    unsafe {
        let mem: HGLOBAL = GlobalAlloc(GHND, bytes)?;
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err(windows::core::Error::from_thread());
        }
        let df = ptr as *mut DROPFILES;
        (*df).pFiles = header as u32;
        (*df).fWide = true.into();
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header), wide.len() * 2);
        let _ = GlobalUnlock(mem);

        // Clipboard managers briefly hold the clipboard open; retry a little before giving up.
        let mut opened = OpenClipboard(None);
        for _ in 0..OPEN_RETRIES {
            if opened.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            opened = OpenClipboard(None);
        }
        let res = opened.and_then(|_| {
            let r = EmptyClipboard()
                .and_then(|_| SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(mem.0))));
            let _ = CloseClipboard();
            r
        });
        if res.is_err() {
            // Ownership only passes to the clipboard on success.
            let _ = GlobalFree(Some(mem));
        }
        res.map(|_| ())
    }
}
