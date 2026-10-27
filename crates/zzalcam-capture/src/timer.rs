//! Sub-millisecond sleeping. `std::thread::sleep` on Windows rounds up to the ~15.6 ms system tick,
//! which would make 30/60 fps capture uneven.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};

pub struct HighResTimer(HANDLE);

impl HighResTimer {
    pub fn new() -> windows::core::Result<Self> {
        // The high-resolution flag needs Windows 10 1803+, below our 1903 minimum.
        let h = unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )?
        };
        Ok(Self(h))
    }

    pub fn sleep(&self, d: Duration) -> windows::core::Result<()> {
        // Negative = relative, in 100 ns units.
        let due = -((d.as_nanos() / 100) as i64).max(1);
        unsafe {
            SetWaitableTimer(self.0, &due, 0, None, None, false)?;
            WaitForSingleObject(self.0, INFINITE);
        }
        Ok(())
    }
}

impl Drop for HighResTimer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
