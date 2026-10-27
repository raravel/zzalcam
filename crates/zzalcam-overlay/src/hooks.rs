//! Low-level mouse / keyboard hooks for the picker, on a thread of their own.
//!
//! Windows calls LL hook procedures from the installing thread's message loop and holds the whole
//! system's input until they return. This thread therefore does nothing but pump messages; hook
//! procedures only forward events to the picker thread, so drawing or UI Automation there can never
//! stall the cursor (or exceed `LowLevelHooksTimeout`, after which Windows silently drops hooks).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::channel;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, HOOKPROC, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOWS_HOOK_ID, WM_APP, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::Error;

/// Mouse event: `wParam` = message | wheel delta << 16, `lParam` = x | y << 32 (screen pixels).
pub const WM_HOOK_MOUSE: u32 = WM_APP + 2;
/// Key press of one of the picker's keys: `wParam` = virtual-key code.
pub const WM_HOOK_KEY: u32 = WM_APP + 3;

// The packing above needs a 64-bit LPARAM.
const _: () = assert!(size_of::<isize>() == 8);

/// Keys the picker handles; they are swallowed while it is open.
const KEYS: [u16; 9] = [
    VK_ESCAPE.0,
    VK_RETURN.0,
    VK_TAB.0,
    VK_LEFT.0,
    VK_RIGHT.0,
    VK_UP.0,
    VK_DOWN.0,
    b'F' as u16,
    b'A' as u16,
];

/// Thread receiving forwarded events. One picker runs at a time.
static TARGET: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lp.0 as *const MSLLHOOKSTRUCT) };
        let msg = wp.0 as u32;
        let wheel = (info.mouseData >> 16) as u16 as usize;
        let packed =
            LPARAM(((info.pt.x as u32 as u64) | ((info.pt.y as u32 as u64) << 32)) as isize);
        let _ = unsafe {
            PostThreadMessageW(
                TARGET.load(Ordering::Relaxed),
                WM_HOOK_MOUSE,
                WPARAM(msg as usize | (wheel << 16)),
                packed,
            )
        };
        // Buttons and wheel belong to the picker; movement must still move the cursor.
        if matches!(
            msg,
            WM_LBUTTONDOWN
                | WM_LBUTTONUP
                | WM_RBUTTONDOWN
                | WM_RBUTTONUP
                | WM_MBUTTONDOWN
                | WM_MBUTTONUP
                | WM_XBUTTONDOWN
                | WM_XBUTTONUP
                | WM_MOUSEWHEEL
        ) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

unsafe extern "system" fn key_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let vk = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) }.vkCode as u16;
        if KEYS.contains(&vk) {
            if matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
                let _ = unsafe {
                    PostThreadMessageW(
                        TARGET.load(Ordering::Relaxed),
                        WM_HOOK_KEY,
                        WPARAM(vk as usize),
                        LPARAM(0),
                    )
                };
            }
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

struct Hook(HHOOK);

impl Hook {
    fn install(id: WINDOWS_HOOK_ID, proc: HOOKPROC) -> windows::core::Result<Self> {
        unsafe {
            let module = GetModuleHandleW(None)?;
            Ok(Self(SetWindowsHookExW(id, proc, Some(module.into()), 0)?))
        }
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        let _ = unsafe { UnhookWindowsHookEx(self.0) };
    }
}

/// Installed hooks; dropping this removes them and ends the thread.
pub struct InputHooks {
    thread: u32,
    join: Option<JoinHandle<()>>,
}

impl InputHooks {
    /// Start forwarding input to `target` (a thread id).
    pub fn start(target: u32) -> Result<Self, Error> {
        TARGET.store(target, Ordering::Relaxed);
        let (tx, rx) = channel();
        let join = std::thread::spawn(move || {
            let mut msg = MSG::default();
            // Create this thread's message queue before anyone posts WM_QUIT to it.
            let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
            let hooks = Hook::install(WH_MOUSE_LL, Some(mouse_hook))
                .and_then(|m| Ok((m, Hook::install(WH_KEYBOARD_LL, Some(key_hook))?)));
            match hooks {
                Ok(_guards) => {
                    let _ = tx.send(Ok(unsafe { GetCurrentThreadId() }));
                    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {}
                    // `_guards` drops here, after the loop, removing both hooks.
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        });
        match rx.recv() {
            Ok(Ok(thread)) => Ok(Self {
                thread,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e.into())
            }
            Err(_) => Err(Error::Thread),
        }
    }
}

impl Drop for InputHooks {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
