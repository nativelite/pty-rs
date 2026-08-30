//! pty — spawn a child process on a real pseudo-terminal, on the Rust
//! standard library alone. Zero dependencies, including no `libc`/`windows`
//! crates: the OS boundary is this crate's own `extern` blocks — **ConPTY**
//! (`CreatePseudoConsole`) on Windows, the classic POSIX pty
//! (`posix_openpt`/`grantpt`/`unlockpt`) on Unix.
//!
//! A process on a PTY believes it is talking to a real terminal: it enables
//! colors, renders progress UIs, and emits the VT byte stream a terminal
//! would receive. That byte stream is exactly what this crate hands you —
//! parse it with the `ansi` crate, or pipe it somewhere. This is the
//! enabler for hosting interactive CLIs (a coding agent, a shell) inside
//! another program.
//!
//! ```no_run
//! use std::time::Duration;
//!
//! let mut pty = pty::Pty::spawn("cmd", &["/C", "echo hello"], 24, 80)?;
//! let mut buf = [0u8; 4096];
//! while let Some(n) = pty.read_timeout(&mut buf, Duration::from_secs(2))? {
//!     if n == 0 { break; }              // EOF: the console closed
//!     print!("{}", String::from_utf8_lossy(&buf[..n]));
//! }
//! let code = pty.wait()?;
//! # std::io::Result::Ok(())
//! ```
//!
//! Per-child environment injection is supported via
//! [`Pty::spawn_with_env`]: the child sees the parent's environment merged
//! with caller-supplied overrides (overrides win), so a credential can be
//! set for one child alone. Working-directory customization stays out of
//! scope (children inherit the parent's cwd), as do multiplexing, layout,
//! scrollback, and session persistence — those are the `amux` product's
//! concerns. One child, one PTY, bytes in, bytes out.

use std::io;
use std::time::Duration;

#[cfg(windows)]
#[path = "sys_windows.rs"]
mod sys;
#[cfg(unix)]
#[path = "sys_unix.rs"]
mod sys;

/// A child process attached to a pseudo-terminal.
///
/// Dropping a `Pty` releases the terminal and our handles but — like
/// `std::process::Child` — does **not** kill the child; call
/// [`kill`](Pty::kill) or [`wait`](Pty::wait) for lifecycle control.
pub struct Pty {
    sys: sys::Sys,
}

impl Pty {
    /// Spawn `cmd` with `args` on a fresh pseudo-terminal of the given
    /// size. The child inherits this process's environment and working
    /// directory.
    ///
    /// Equivalent to [`spawn_with_env`](Pty::spawn_with_env) with no
    /// overrides.
    pub fn spawn(cmd: &str, args: &[&str], rows: u16, cols: u16) -> io::Result<Pty> {
        Self::spawn_with_env(cmd, args, rows, cols, &[])
    }

    /// Spawn `cmd` with `args` on a fresh pseudo-terminal of the given size,
    /// with per-child environment overrides.
    ///
    /// The child's environment is **this process's environment merged with
    /// `env`**, where an entry in `env` overrides (or adds) the value for its
    /// key. Inherited variables the overrides do not name — `PATH`,
    /// `SystemRoot`, and everything else — are preserved, so the child still
    /// finds its runtime. This is the enabler for injecting credentials into
    /// one child without leaking them into the parent or its siblings.
    ///
    /// Key handling matches the platform: case-sensitive on Unix,
    /// case-insensitive on Windows (where `Path` and `PATH` are the same
    /// variable). The child's working directory is still inherited.
    pub fn spawn_with_env(
        cmd: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        env: &[(String, String)],
    ) -> io::Result<Pty> {
        Ok(Pty {
            sys: sys::Sys::spawn_with_env(cmd, args, rows, cols, env)?,
        })
    }

    /// Read output the child wrote to its terminal.
    ///
    /// Returns `None` on timeout, `Some(0)` on end-of-stream (the terminal
    /// closed), and `Some(n)` when `n` bytes were read into `buf`.
    ///
    /// End-of-stream timing is platform-dependent: on Unix it follows the
    /// child closing the slave side; on Windows some builds keep the
    /// console open until this `Pty` is dropped. Output written before the
    /// child exited is always still drainable after [`wait`](Pty::wait) —
    /// drain with short timeouts rather than waiting for `Some(0)`.
    pub fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        self.sys.read_timeout(buf, timeout)
    }

    /// Write bytes to the child's terminal input (what it reads as
    /// keystrokes). Returns the number of bytes written.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.sys.write(bytes)
    }

    /// Tell the terminal (and therefore the child, via its size query /
    /// SIGWINCH) that it is now `rows` x `cols`.
    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        self.sys.resize(rows, cols)
    }

    /// Wait for the child to exit and return its exit code. A child killed
    /// by a signal (Unix) reports `128 + signo` by convention.
    pub fn wait(&mut self) -> io::Result<i32> {
        self.sys.wait()
    }

    /// Exit code if the child has already exited, else `None`.
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        self.sys.try_wait()
    }

    /// Forcibly terminate the child.
    pub fn kill(&mut self) -> io::Result<()> {
        self.sys.kill()
    }
}
