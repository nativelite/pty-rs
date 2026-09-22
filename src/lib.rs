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

pub mod cmdline;

#[cfg(windows)]
#[path = "sys_windows.rs"]
mod sys;
#[cfg(unix)]
#[path = "sys_unix.rs"]
mod sys;

/// Host every pseudo-console this process creates from now on with the ConPTY
/// implementation in `dll`, instead of the system's `conhost.exe` (Windows
/// only).
///
/// Windows Terminal's `conpty.dll` (MIT, also published on NuGet as
/// `Microsoft.Windows.Console.ConPTY`) starts the `OpenConsole.exe` next to
/// it, a newer console host than the one in Windows. Call this before the
/// first spawn; it is process-wide and cannot be undone. Calling it again with
/// the same path is a no-op, and with a different path is an error.
#[cfg(windows)]
pub fn use_conpty_library(dll: &std::path::Path) -> io::Result<()> {
    sys::use_conpty_library(dll)
}

/// The ConPTY library set with [`use_conpty_library`], or `None` when the
/// system's console host is in use (Windows only).
#[cfg(windows)]
pub fn conpty_library() -> Option<std::path::PathBuf> {
    sys::conpty_library()
}

/// A child process attached to a pseudo-terminal.
///
/// Dropping a `Pty` releases the terminal and our handles but, like
/// `std::process::Child`, does **not** kill the child; call
/// [`kill`](Pty::kill) or [`wait`](Pty::wait) for lifecycle control.
pub struct Pty {
    sys: sys::Sys,
}

/// A blocking reader over a [`Pty`]'s output. See [`Pty::reader`].
pub struct PtyReader {
    sys: sys::Reader,
}

impl PtyReader {
    /// Block until the child writes, and return how many bytes were read into
    /// `buf`. `Ok(0)` is end of stream (see [`Pty::reader`]).
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.sys.read(buf)
    }
}

impl io::Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.sys.read(buf)
    }
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
            sys: sys::Sys::spawn_full(cmd, args, rows, cols, env, cwd, false)?,
        })
    }

    /// Windows only: [`spawn_full`](Pty::spawn_full), but the child is created
    /// **suspended** (`CREATE_SUSPENDED`): it exists and has a [`pid`](Pty::pid),
    /// but runs no code until [`resume`](Pty::resume).
    ///
    /// This closes a race a caller can't close any other way: a process assigned
    /// to a Job Object after it starts may already have created children, and
    /// those are never in the job. Spawn suspended, assign, then resume, and the
    /// whole tree is inside from its first instruction.
    ///
    /// A suspended child that is never resumed stays suspended until it is
    /// [`kill`](Pty::kill)ed; dropping the `Pty` does not resume it.
    ///
    /// Unix has no equivalent here: `std::process::Command::spawn` returns only
    /// after the child has exec'd, so a child can't be held before it runs.
    #[cfg(windows)]
    pub fn spawn_suspended(
        cmd: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        env: &[(String, String)],
        cwd: Option<&str>,
    ) -> io::Result<Pty> {
        Ok(Pty {
            sys: sys::Sys::spawn_full(cmd, args, rows, cols, env, cwd, true)?,
        })
    }

    /// Windows only: let a child from [`spawn_suspended`](Pty::spawn_suspended)
    /// start running. Resuming a child that is already running changes nothing.
    #[cfg(windows)]
    pub fn resume(&mut self) -> io::Result<()> {
        self.sys.resume()
    }

    /// A reader over this terminal's output that **blocks** until bytes arrive,
    /// for a thread of its own: a host multiplexing many terminals then wakes
    /// the moment any of them writes, instead of polling each with
    /// [`read_timeout`](Pty::read_timeout).
    ///
    /// The reader is independent of the `Pty` (it holds a duplicate of the
    /// output handle) and is [`Send`]. Rules, stated plainly:
    ///
    /// - Use one or the other. A reader and `read_timeout` on the same `Pty`
    ///   would split the stream between them.
    /// - [`PtyReader::read`] returns `Ok(0)` at end of stream: the child's side
    ///   closed, or (unix) the `Pty` was dropped, noticed within 100 ms.
    /// - **Windows:** keep reading, or drop the reader, before dropping the
    ///   `Pty`. Dropping a `Pty` waits for the console host to finish writing, and
    ///   a reader that holds the pipe open without reading can leave it waiting.
    pub fn reader(&self) -> io::Result<PtyReader> {
        Ok(PtyReader {
            sys: self.sys.reader()?,
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
