//! Native terminal cancellation only sets a process flag. Durable transitions
//! and partial reports remain ordinary dispatcher/runner work outside handlers.
use crate::domain::{ErrorCode, Result, WikiError};

#[cfg(unix)]
extern "C" fn interrupted(_: libc::c_int) {
    crate::jobs::types::record_native_interrupt();
}
#[cfg(windows)]
unsafe extern "system" fn interrupted(kind: u32) -> i32 {
    if kind == 0 || kind == 1 {
        crate::jobs::types::record_native_interrupt();
        1
    } else {
        0
    }
}
/// Install once in the executable before remote work. Library callers keep
/// explicit cancellation tokens and do not change process signal dispositions.
pub fn install() -> Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: sigaction is fully initialized; the C handler performs only a
        // lock-free atomic store and has no allocation, I/O, or lock operations.
        let result = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = interrupted as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = libc::SA_RESTART;
            libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut())
        };
        if result != 0 {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "terminal cancellation handler unavailable",
            ));
        }
    }
    #[cfg(windows)]
    {
        // SAFETY: the static system callback has the required ABI and remains
        // valid for the lifetime of this executable.
        if unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(interrupted), 1)
        } == 0
        {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "terminal cancellation handler unavailable",
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    return Err(WikiError::new(
        ErrorCode::CapabilityUnavailable,
        "terminal cancellation is unavailable on this platform",
    ));
    Ok(())
}
