//! pty: spawn a child process on a real pseudo-terminal, on the Rust
//! standard library alone. Zero dependencies, including no `libc`/`windows`
//! crates: the OS boundary is this crate's own `extern` blocks. **ConPTY**
//! (`CreatePseudoConsole`) on Windows, the classic POSIX pty
//! (`posix_openpt`/`grantpt`/`unlockpt`) on Unix.
//!
//! A process on a PTY believes it is talking to a real terminal: it enables
//! colors, renders progress UIs, and emits the VT byte stream a terminal
//! would receive. That byte stream is exactly what this crate hands you:
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
//! [`Pty::spawn_with_env`], and a per-child working directory via
//! [`Pty::spawn_full`]: the child sees the parent's environment merged with
//! caller-supplied overrides (overrides win), so a credential can be set for
//! one child alone, and can be started in a directory of the caller's
//! choosing: the enabler for each agent auto-loading the `CLAUDE.md` in its
//! own tree. Multiplexing, layout, scrollback, and session persistence stay
//! out of scope; those are the `atrium` product's concerns. One child, one
//! PTY, bytes in, bytes out.

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
/// Dropping a `Pty` releases the terminal and our handles but, like
/// `std::process::Child`, does **not** kill the child; call
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
        Self::spawn_full(cmd, args, rows, cols, &[], None)
    }

    /// Spawn `cmd` with `args` on a fresh pseudo-terminal of the given size,
    /// with per-child environment overrides.
    ///
    /// The child's environment is **this process's environment merged with
    /// `env`**, where an entry in `env` overrides (or adds) the value for its
    /// key. Inherited variables the overrides do not name (`PATH`,
    /// `SystemRoot`, and everything else) are preserved, so the child still
    /// finds its runtime. This is the enabler for injecting credentials into
    /// one child without leaking them into the parent or its siblings.
    ///
    /// Key handling matches the platform: case-sensitive on Unix,
    /// case-insensitive on Windows (where `Path` and `PATH` are the same
    /// variable). The child's working directory is still inherited.
    ///
    /// Equivalent to [`spawn_full`](Pty::spawn_full) with `cwd = None`.
    pub fn spawn_with_env(
        cmd: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        env: &[(String, String)],
    ) -> io::Result<Pty> {
        Self::spawn_full(cmd, args, rows, cols, env, None)
    }

    /// Spawn `cmd` with `args` on a fresh pseudo-terminal of the given size,
    /// with per-child environment overrides **and** a per-child working
    /// directory. This is the full form; [`spawn`](Pty::spawn) and
    /// [`spawn_with_env`](Pty::spawn_with_env) are thin wrappers over it.
    ///
    /// Environment handling is exactly [`spawn_with_env`](Pty::spawn_with_env):
    /// the child's environment is this process's environment merged with `env`
    /// (overrides win by key), so inherited variables are preserved.
    ///
    /// `cwd` sets the child's working directory:
    /// - `Some(dir)` starts the child in `dir`. This is what lets a spawned
    ///   agent auto-load the `CLAUDE.md` sitting in its own tree.
    /// - `None` inherits this process's working directory (today's behavior).
    ///
    /// A `cwd` that does not exist is surfaced as an [`io::Error`] from this
    /// call; the child is not started in a fallback directory.
    pub fn spawn_full(
        cmd: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        env: &[(String, String)],
        cwd: Option<&str>,
    ) -> io::Result<Pty> {
        Ok(Pty {
            sys: sys::Sys::spawn_full(cmd, args, rows, cols, env, cwd)?,
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
    /// child exited is always still drainable after [`wait`](Pty::wait);
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

    /// The child's process id.
    ///
    /// Exposed so a caller can tear down the child's **whole tree**, which
    /// [`Pty::kill`] deliberately does not do: it terminates the direct child
    /// only, leaving anything that child spawned to be reparented and run on.
    /// On unix every child is a session leader (`setsid()` in `pre_exec`), so
    /// this pid doubles as the process-group id for `killpg`; on Windows it is
    /// the id to reopen for a Job Object. The teardown *policy* (which signal,
    /// how long to wait before escalating) belongs to the caller, not here.
    pub fn pid(&self) -> u32 {
        self.sys.pid()
    }

    /// Forcibly terminate the child.
    pub fn kill(&mut self) -> io::Result<()> {
        self.sys.kill()
    }
}
