//! POSIX pty, via our own externs against the platform C library — no
//! `libc` crate. The master comes from `posix_openpt`; the child is spawned
//! with `std::process::Command` whose stdio is the slave, made the
//! controlling terminal in `pre_exec` (`setsid` + `TIOCSCTTY`). Timeouts
//! use `poll` on the master, resize is `ioctl(TIOCSWINSZ)`.

use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::time::Duration;

#[cfg(target_os = "linux")]
mod plat {
    pub const O_NOCTTY: i32 = 0o400;
    pub const TIOCSCTTY: u64 = 0x540E;
    pub const TIOCSWINSZ: u64 = 0x5414;
    pub type Nfds = u64;
}

#[cfg(target_os = "macos")]
mod plat {
    pub const O_NOCTTY: i32 = 0x20000;
    pub const TIOCSCTTY: u64 = 0x2000_7461;
    pub const TIOCSWINSZ: u64 = 0x8008_7467;
    pub type Nfds = u32;
}

use plat::*;

const O_RDWR: i32 = 2;
const POLLIN: i16 = 0x0001;
const POLLHUP: i16 = 0x0010;
const F_SETFD: i32 = 2;
const FD_CLOEXEC: i32 = 1;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

extern "C" {
    fn posix_openpt(flags: i32) -> i32;
    fn grantpt(fd: i32) -> i32;
    fn unlockpt(fd: i32) -> i32;
    fn open(path: *const u8, flags: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    fn poll(fds: *mut PollFd, nfds: Nfds, timeout_ms: i32) -> i32;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn fcntl(fd: i32, cmd: i32, arg: i32) -> i32;
    fn setsid() -> i32;
    #[cfg(target_os = "linux")]
    fn ptsname_r(fd: i32, buf: *mut u8, len: usize) -> i32;
    #[cfg(target_os = "macos")]
    fn ptsname(fd: i32) -> *const u8;
}

fn slave_path(master: i32) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        let mut buf = [0u8; 128];
        if unsafe { ptsname_r(master, buf.as_mut_ptr(), buf.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let len = buf.iter().position(|&b| b == 0).unwrap_or(0);
        let mut out = buf[..len].to_vec();
        out.push(0);
        Ok(out)
    }
    #[cfg(target_os = "macos")]
    {
        let p = unsafe { ptsname(master) };
        if p.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut out = Vec::new();
        let mut i = 0;
        unsafe {
            while *p.add(i) != 0 {
                out.push(*p.add(i));
                i += 1;
            }
        }
        out.push(0);
        Ok(out)
    }
}

pub struct Sys {
    master: i32,
    child: std::process::Child,
}

impl Sys {
    pub fn spawn_with_env(
        cmd: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        env: &[(String, String)],
    ) -> io::Result<Sys> {
        let master = unsafe { posix_openpt(O_RDWR | O_NOCTTY) };
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let cleanup = |e: io::Error| {
            unsafe { close(master) };
            Err(e)
        };
        if unsafe { grantpt(master) } != 0 || unsafe { unlockpt(master) } != 0 {
            return cleanup(io::Error::last_os_error());
        }
        // The master must not leak into the child.
        if unsafe { fcntl(master, F_SETFD, FD_CLOEXEC) } != 0 {
            return cleanup(io::Error::last_os_error());
        }
        let path = match slave_path(master) {
            Ok(p) => p,
            Err(e) => return cleanup(e),
        };
        let slave = unsafe { open(path.as_ptr(), O_RDWR | O_NOCTTY) };
        if slave < 0 {
            return cleanup(io::Error::last_os_error());
        }
        let mut sys = Sys {
            master,
            // placeholder replaced below; construct after fds are ready
            child: {
                let stdin = unsafe { std::fs::File::from_raw_fd(slave) };
                let stdout = match stdin.try_clone() {
                    Ok(f) => f,
                    Err(e) => {
                        unsafe { close(master) };
                        return Err(e);
                    }
                };
                let stderr = match stdin.try_clone() {
                    Ok(f) => f,
                    Err(e) => {
                        unsafe { close(master) };
                        return Err(e);
                    }
                };
                let mut command = std::process::Command::new(cmd);
                command
                    .args(args)
                    .stdin(stdin)
                    .stdout(stdout)
                    .stderr(stderr);
                // Command inherits the parent environment by default; `envs`
                // layers the overrides on top (merge, not replace) so the
                // child keeps PATH and friends while gaining the injected keys.
                command.envs(env.iter().map(|(k, v)| (k, v)));
                unsafe {
                    command.pre_exec(|| {
                        if setsid() < 0 {
                            return Err(io::Error::last_os_error());
                        }
                        // fd 0 is the slave: make it the controlling tty.
                        if ioctl(0, TIOCSCTTY, 0u64) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                match command.spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        unsafe { close(master) };
                        return Err(e);
                    }
                }
            },
        };
        sys.resize(rows, cols)?;
        Ok(sys)
    }

    pub fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut fds = PollFd {
            fd: self.master,
            events: POLLIN,
            revents: 0,
        };
        let n = unsafe { poll(&mut fds, 1, millis) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(e);
        }
        if n == 0 {
            return Ok(None);
        }
        if fds.revents & (POLLIN | POLLHUP) == 0 {
            return Ok(None);
        }
        let got = unsafe { read(self.master, buf.as_mut_ptr(), buf.len()) };
        if got < 0 {
            let e = io::Error::last_os_error();
            // EIO from a pty master means the slave side is gone: EOF.
            return if e.raw_os_error() == Some(5) {
                Ok(Some(0))
            } else {
                Err(e)
            };
        }
        Ok(Some(got as usize))
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = unsafe { write(self.master, bytes.as_ptr(), bytes.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        let ws = WinSize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { ioctl(self.master, TIOCSWINSZ, &ws as *const WinSize) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn wait(&mut self) -> io::Result<i32> {
        let status = self.child.wait()?;
        Ok(exit_code(status))
    }

    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        Ok(self.child.try_wait()?.map(exit_code))
    }

    pub fn kill(&mut self) -> io::Result<()> {
        match self.child.kill() {
            Ok(()) => Ok(()),
            // Already exited is fine.
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(e) => Err(e),
        }
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(-1)
}

impl Drop for Sys {
    fn drop(&mut self) {
        unsafe {
            close(self.master);
        }
    }
}
