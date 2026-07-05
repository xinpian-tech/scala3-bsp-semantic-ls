//! The stdout protection for the LSP protocol stream. The JVM and any PC plugin
//! can write to fd 1 (`System.out`, native `printf`); on the shared stdout the
//! editor uses for LSP, that corrupts the protocol. Before the JVM boots, we
//! duplicate the original stdout to a private fd (the real LSP sink) and point
//! fd 1 at stderr, so every stray write to fd 1 lands on stderr and the LSP
//! writer keeps talking to the editor through the private duplicate.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// Holds the private duplicate of the original stdout. Install it before
/// `JNI_CreateJavaVM`; hand [`StdoutGuard::into_lsp_stdout`] to the LSP writer.
#[derive(Debug)]
pub struct StdoutGuard {
    private_stdout: OwnedFd,
}

impl StdoutGuard {
    /// Duplicate the real stdout (fd 1) to a private fd, then alias fd 1 onto
    /// stderr (fd 2). Call once, before booting the JVM.
    pub fn install() -> io::Result<StdoutGuard> {
        StdoutGuard::redirect(libc::STDOUT_FILENO, libc::STDERR_FILENO)
    }

    /// The general form used by [`install`](StdoutGuard::install) and by tests:
    /// duplicate `target` to a fresh private fd, then make `target` alias
    /// `onto`. Afterwards writes to `target` reach `onto`'s destination, while
    /// writes to the returned private fd reach `target`'s original destination.
    pub fn redirect(target: RawFd, onto: RawFd) -> io::Result<StdoutGuard> {
        // SAFETY: `dup`/`dup2` are always safe to call; we check the return
        // codes and take ownership of the new fd exactly once.
        let private = unsafe { libc::dup(target) };
        if private < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: we own `private` from here; wrap it before the next fallible
        // call so it is closed on an early return.
        let private_stdout = unsafe { OwnedFd::from_raw_fd(private) };

        // SAFETY: aliasing `target` onto `onto`; `dup2` closes any prior `target`.
        if unsafe { libc::dup2(onto, target) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(StdoutGuard { private_stdout })
    }

    /// The private duplicate of the original stdout — the LSP writer must send
    /// protocol bytes here, never to fd 1.
    pub fn lsp_stdout(&self) -> &OwnedFd {
        &self.private_stdout
    }

    /// Consume the guard, yielding the private stdout for the LSP writer.
    pub fn into_lsp_stdout(self) -> OwnedFd {
        self.private_stdout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// Creates a `(read, write)` pipe as `OwnedFd`s.
    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` is a valid two-element array.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe: {}", io::Error::last_os_error());
        // SAFETY: pipe returned two fresh, owned fds.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn write_all(fd: RawFd, bytes: &[u8]) {
        // SAFETY: `fd` is open; `bytes` is a valid buffer.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(
            n,
            bytes.len() as isize,
            "write: {}",
            io::Error::last_os_error()
        );
    }

    fn read_some(fd: RawFd, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        // SAFETY: `fd` is open; `buf` has `len` bytes.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n >= 0, "read: {}", io::Error::last_os_error());
        buf.truncate(n as usize);
        buf
    }

    #[test]
    fn redirect_sends_fd_writes_to_stderr_and_preserves_the_original() {
        // Synthetic "stdout" and "stderr" pipes so the test never touches the
        // real process fd 1/2.
        let (out_r, out_w) = pipe();
        let (err_r, err_w) = pipe();

        let guard = StdoutGuard::redirect(out_w.as_raw_fd(), err_w.as_raw_fd()).unwrap();

        // A stray write to the "stdout" fd now lands on the "stderr" pipe.
        write_all(out_w.as_raw_fd(), b"island-noise");
        assert_eq!(read_some(err_r.as_raw_fd(), 64), b"island-noise");

        // The private duplicate still reaches the original stdout pipe, so the
        // LSP writer's bytes are uncorrupted.
        write_all(guard.lsp_stdout().as_raw_fd(), b"Content-Length: 2");
        assert_eq!(read_some(out_r.as_raw_fd(), 64), b"Content-Length: 2");
    }
}
