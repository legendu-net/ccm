//! Raw terminal mode for the message review prompt's single-keypress reads
//! (`picker::prompt_action`) — the only place `ccm` touches termios. Clears `ICANON`
//! and `ECHO` only — `ISIG` stays set, so Ctrl-C still interrupts normally — and
//! restores the original settings on drop, so `$EDITOR` and the jj commit-command
//! picker (both of which need ordinary canonical line input) see the terminal back to
//! normal once the single-byte read is done.

use nix::sys::termios::{self, LocalFlags, SetArg, SpecialCharacterIndices, Termios};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

/// RAII guard: puts a terminal fd into raw-ish mode (no `ICANON`, no `ECHO`; `VMIN=1`,
/// `VTIME=0` so a single `read()` blocks for exactly one byte) for the scope of a
/// single keypress read, restoring the original [`Termios`] on drop.
pub struct RawGuard {
    fd: RawFd,
    saved: Termios,
}

impl RawGuard {
    /// Attempts to enable raw-ish mode on `fd`. Returns `None` (not an error) if
    /// `tcgetattr`/`tcsetattr` fail — the caller falls back to ordinary line-mode
    /// reading rather than failing the run, e.g. when `fd` isn't actually backed by a
    /// terminal despite `Environment::stdin_is_terminal` reporting so.
    #[must_use]
    pub fn enable(fd: impl AsRawFd) -> Option<Self> {
        let raw_fd = fd.as_raw_fd();
        // SAFETY: `raw_fd` is borrowed only for the duration of these two calls, both
        // of which complete before this function returns.
        let borrowed = unsafe { BorrowedFd::borrow_raw(raw_fd) };
        let saved = termios::tcgetattr(borrowed).ok()?;
        let mut raw = saved.clone();
        raw.local_flags
            .remove(LocalFlags::ICANON | LocalFlags::ECHO);
        raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        termios::tcsetattr(borrowed, SetArg::TCSANOW, &raw).ok()?;
        Some(Self { fd: raw_fd, saved })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // SAFETY: `self.fd` was validated as a real terminal fd in `enable`, and stays
        // open for the guard's whole lifetime (it's always the process's own stdin).
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = termios::tcsetattr(borrowed, SetArg::TCSANOW, &self.saved);
    }
}
