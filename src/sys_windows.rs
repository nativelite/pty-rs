//! Windows ConPTY, via our own kernel32 externs (Windows 10 1809+).
//!
//! Shape: two anonymous pipes bridge us to the pseudoconsole
//! (`CreatePseudoConsole`); the child is spawned with the
//! `PSEUDOCONSOLE` proc-thread attribute so its console *is* the pty.
//! Timeouts on the output pipe use `PeekNamedPipe` polling (anonymous
//! pipes cannot do overlapped I/O); `ERROR_BROKEN_PIPE` on read is the
//! end-of-stream signal.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

type Handle = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    cb_reserved2: u16,
    reserved2: *mut u8,
    std_input: Handle,
    std_output: Handle,
    std_error: Handle,
}

#[repr(C)]
struct StartupInfoExW {
    startup_info: StartupInfoW,
    attribute_list: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreatePipe(read: *mut Handle, write: *mut Handle, attributes: *mut c_void, size: u32)
        -> i32;
    fn CreatePseudoConsole(
        size: Coord,
        input: Handle,
        output: Handle,
        flags: u32,
        hpc: *mut Handle,
    ) -> i32;
    fn ResizePseudoConsole(hpc: Handle, size: Coord) -> i32;
    fn ClosePseudoConsole(hpc: Handle);
    fn CloseHandle(handle: Handle) -> i32;
    fn ReadFile(
        handle: Handle,
        buf: *mut u8,
        len: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn WriteFile(
        handle: Handle,
        buf: *const u8,
        len: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn PeekNamedPipe(
        handle: Handle,
        buf: *mut c_void,
        len: u32,
        read: *mut u32,
        available: *mut u32,
        left: *mut u32,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
    fn GetExitCodeProcess(handle: Handle, code: *mut u32) -> i32;
    fn TerminateProcess(handle: Handle, code: u32) -> i32;
    fn InitializeProcThreadAttributeList(
        list: *mut c_void,
        count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *mut c_void,
        size: usize,
        previous: *mut c_void,
        return_size: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
    fn CreateProcessW(
        application: *const u16,
        command_line: *mut u16,
        process_attrs: *mut c_void,
        thread_attrs: *mut c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *mut c_void,
        current_dir: *const u16,
        startup_info: *mut StartupInfoExW,
        process_info: *mut ProcessInformation,
    ) -> i32;
}

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
const ERROR_BROKEN_PIPE: i32 = 109;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
const INFINITE: u32 = u32::MAX;

/// Poll step while waiting for pipe data (anonymous pipes cannot block
/// with a timeout).
const PEEK_STEP: Duration = Duration::from_millis(10);

pub struct Sys {
    hpc: Handle,
    /// We write keystrokes here; the console reads them.
    input_write: Handle,
    /// The console writes rendered output here; we read it.
    output_read: Handle,
    process: Handle,
    thread: Handle,
    exit_code: Option<i32>,
}

// Raw handles are owned exclusively by this struct.
unsafe impl Send for Sys {}

fn last_err() -> io::Error {
    io::Error::last_os_error()
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// Windows command-line quoting for one argument (the CommandLineToArgvW
/// convention): backslashes are literal except before a quote.
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| " \t\"".contains(c)) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        if c == '"' {
            // Backslashes before a quote must be doubled, plus one to
            // escape the quote itself.
            out.extend(std::iter::repeat('\\').take(backslashes * 2 + 1));
            backslashes = 0;
            out.push('"');
            continue;
        }
        out.extend(std::iter::repeat('\\').take(backslashes));
        backslashes = 0;
        out.push(c);
    }
    // Trailing backslashes precede the closing quote: double them.
    out.extend(std::iter::repeat('\\').take(backslashes * 2));
    out.push('"');
    out
}

pub fn build_command_line(cmd: &str, args: &[&str]) -> String {
    let mut line = quote_arg(cmd);
    for a in args {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    line
}

impl Sys {
    pub fn spawn(cmd: &str, args: &[&str], rows: u16, cols: u16) -> io::Result<Sys> {
        unsafe {
            // Pipes: (console input read, our input write) and
            // (our output read, console output write).
            let (mut in_read, mut in_write): (Handle, Handle) =
                (std::ptr::null_mut(), std::ptr::null_mut());
            let (mut out_read, mut out_write): (Handle, Handle) =
                (std::ptr::null_mut(), std::ptr::null_mut());
            if CreatePipe(&mut in_read, &mut in_write, std::ptr::null_mut(), 0) == 0 {
                return Err(last_err());
            }
            if CreatePipe(&mut out_read, &mut out_write, std::ptr::null_mut(), 0) == 0 {
                let e = last_err();
                CloseHandle(in_read);
                CloseHandle(in_write);
                return Err(e);
            }
            let size = Coord {
                x: cols as i16,
                y: rows as i16,
            };
            let mut hpc: Handle = std::ptr::null_mut();
            let hr = CreatePseudoConsole(size, in_read, out_write, 0, &mut hpc);
            // The console owns duplicates now; ours can go.
            CloseHandle(in_read);
            CloseHandle(out_write);
            if hr != 0 {
                CloseHandle(in_write);
                CloseHandle(out_read);
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("CreatePseudoConsole failed (HRESULT 0x{hr:08x})"),
                ));
            }

            match spawn_on_console(hpc, cmd, args) {
                Ok((process, thread)) => Ok(Sys {
                    hpc,
                    input_write: in_write,
                    output_read: out_read,
                    process,
                    thread,
                    exit_code: None,
                }),
                Err(e) => {
                    ClosePseudoConsole(hpc);
                    CloseHandle(in_write);
                    CloseHandle(out_read);
                    Err(e)
                }
            }
        }
    }

    pub fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut available: u32 = 0;
            let ok = unsafe {
                PeekNamedPipe(
                    self.output_read,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let e = last_err();
                return if e.raw_os_error() == Some(ERROR_BROKEN_PIPE) {
                    Ok(Some(0)) // console closed: end of stream
                } else {
                    Err(e)
                };
            }
            if available > 0 {
                let mut read: u32 = 0;
                let want = buf.len().min(available as usize) as u32;
                let ok = unsafe {
                    ReadFile(
                        self.output_read,
                        buf.as_mut_ptr(),
                        want,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    let e = last_err();
                    return if e.raw_os_error() == Some(ERROR_BROKEN_PIPE) {
                        Ok(Some(0))
                    } else {
                        Err(e)
                    };
                }
                return Ok(Some(read as usize));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(PEEK_STEP.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut written: u32 = 0;
        let ok = unsafe {
            WriteFile(
                self.input_write,
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_err());
        }
        Ok(written as usize)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        let hr = unsafe {
            ResizePseudoConsole(
                self.hpc,
                Coord {
                    x: cols as i16,
                    y: rows as i16,
                },
            )
        };
        if hr != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("ResizePseudoConsole failed (HRESULT 0x{hr:08x})"),
            ));
        }
        Ok(())
    }

    pub fn wait(&mut self) -> io::Result<i32> {
        if let Some(code) = self.exit_code {
            return Ok(code);
        }
        if unsafe { WaitForSingleObject(self.process, INFINITE) } != WAIT_OBJECT_0 {
            return Err(last_err());
        }
        self.fetch_exit_code()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        if let Some(code) = self.exit_code {
            return Ok(Some(code));
        }
        match unsafe { WaitForSingleObject(self.process, 0) } {
            WAIT_OBJECT_0 => self.fetch_exit_code().map(Some),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(last_err()),
        }
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if self.exit_code.is_some() {
            return Ok(());
        }
        if unsafe { TerminateProcess(self.process, 1) } == 0 {
            let e = last_err();
            // Already exited between checks is not an error.
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            return Err(e);
        }
        Ok(())
    }

    fn fetch_exit_code(&mut self) -> io::Result<i32> {
        let mut code: u32 = 0;
        if unsafe { GetExitCodeProcess(self.process, &mut code) } == 0 {
            return Err(last_err());
        }
        let code = code as i32;
        self.exit_code = Some(code);
        Ok(code)
    }
}

impl Drop for Sys {
    fn drop(&mut self) {
        unsafe {
            // Pipes first: ClosePseudoConsole blocks until conhost drains
            // its output, and conhost can be wedged mid-write to a full
            // pipe nobody is reading. Breaking the pipes unblocks it.
            CloseHandle(self.input_write);
            CloseHandle(self.output_read);
            ClosePseudoConsole(self.hpc);
            CloseHandle(self.thread);
            CloseHandle(self.process);
        }
    }
}

/// Build the attribute list binding the child to `hpc` and CreateProcess it.
unsafe fn spawn_on_console(hpc: Handle, cmd: &str, args: &[&str]) -> io::Result<(Handle, Handle)> {
    let mut attr_size: usize = 0;
    InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size);
    let e = last_err();
    if e.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER) || attr_size == 0 {
        return Err(e);
    }
    let mut attr_buf = vec![0u8; attr_size];
    let attr_list = attr_buf.as_mut_ptr() as *mut c_void;
    if InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) == 0 {
        return Err(last_err());
    }
    let result = (|| {
        if UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            hpc,
            std::mem::size_of::<Handle>(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(last_err());
        }
        let mut si: StartupInfoExW = std::mem::zeroed();
        si.startup_info.cb = std::mem::size_of::<StartupInfoExW>() as u32;
        // NULL std handles + USESTDHANDLES: without this the child inherits
        // the parent's (possibly redirected) std handles and writes past the
        // pseudoconsole. With it, the child falls back to its console — the
        // pty we just attached.
        si.startup_info.flags = STARTF_USESTDHANDLES;
        si.attribute_list = attr_list;
        let mut pi: ProcessInformation = std::mem::zeroed();
        let mut cmdline = wide(std::ffi::OsStr::new(&build_command_line(cmd, args)));
        if CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0, // no handle inheritance; the console attribute carries the pty
            EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut si,
            &mut pi,
        ) == 0
        {
            return Err(last_err());
        }
        Ok((pi.process, pi.thread))
    })();
    DeleteProcThreadAttributeList(attr_list);
    result
}

#[cfg(test)]
mod tests {
    use super::{build_command_line, quote_arg};

    #[test]
    fn quoting_follows_argv_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg("has space"), "\"has space\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        // No specials: backslashes are literal, no quoting needed.
        assert_eq!(quote_arg("trail\\"), "trail\\");
        // Quoted (space forces it): trailing backslashes double before the
        // closing quote; interior ones stay literal.
        assert_eq!(quote_arg("trail \\"), "\"trail \\\\\"");
        assert_eq!(quote_arg("a\\\\b c"), "\"a\\\\b c\"");
        assert_eq!(
            build_command_line("cmd", &["/C", "echo hello world"]),
            "cmd /C \"echo hello world\""
        );
    }
}
