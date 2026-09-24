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
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
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
    fn LoadLibraryExW(name: *const u16, file: Handle, flags: u32) -> Handle;
    fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
    fn CloseHandle(handle: Handle) -> i32;
    /// Resolve a process id from its handle. The caller needs the id (not the
    /// handle) to reopen the process, e.g. to assign it to a Job Object, which
    /// is how a whole process tree is bounded on Windows.
    fn GetProcessId(process: Handle) -> u32;
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
    fn ResumeThread(thread: Handle) -> u32;
    fn GetCurrentProcess() -> Handle;
    fn DuplicateHandle(
        source_process: Handle,
        source: Handle,
        target_process: Handle,
        target: *mut Handle,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
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
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const CREATE_SUSPENDED: u32 = 0x0000_0004;
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
    /// The pseudo-console came from the library set with
    /// `use_conpty_library` (false: the system's `conhost.exe`). Resize and
    /// close must go to the same one.
    library: bool,
}

// Raw handles are owned exclusively by this struct.
unsafe impl Send for Sys {}

const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

/// A blocking reader over a duplicate of the output pipe's read end.
pub struct Reader {
    handle: Handle,
}

// The duplicated handle is owned exclusively by this struct.
unsafe impl Send for Reader {}

impl Reader {
    /// Block until output arrives. `Ok(0)` is end of stream: the pseudoconsole
    /// closed its end (`ERROR_BROKEN_PIPE`).
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let mut read: u32 = 0;
            let want = buf.len().min(u32::MAX as usize) as u32;
            let ok = unsafe {
                ReadFile(
                    self.handle,
                    buf.as_mut_ptr(),
                    want,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let e = last_err();
                return if e.raw_os_error() == Some(ERROR_BROKEN_PIPE) {
                    Ok(0)
                } else {
                    Err(e)
                };
            }
            // A successful zero-byte read is a zero-length write, not the end
            // of the stream: keep waiting for real bytes.
            if read > 0 {
                return Ok(read as usize);
            }
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

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

/// Build a UTF-16, double-null-terminated environment block for
/// `CreateProcessW` (used with `CREATE_UNICODE_ENVIRONMENT`) from the parent
/// environment merged with `overrides`.
///
/// Semantics: start from `std::env::vars_os` (the parent set), then apply
/// each override; later wins, and keys are matched **case-insensitively**
/// because Windows environment variables are (so `Path` overrides an
/// inherited `PATH` rather than duplicating it). The final block is sorted
/// case-insensitively by key, which `CreateProcessW` requires. Returns
/// `None` when the merged environment is empty, so the caller can pass a
/// NULL block (there is no valid zero-entry Unicode block; it would be just
/// the terminator, which is the "empty environment" sentinel we never want).
fn build_env_block(overrides: &[(String, String)]) -> Option<Vec<u16>> {
    use std::collections::BTreeMap;

    // Key: uppercased (for case-insensitive identity + ordering). Value:
    // (original-cased key, value) so we emit the real name.
    let mut merged: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        let v = v.to_string_lossy().into_owned();
        merged.insert(k.to_uppercase(), (k, v));
    }
    for (k, v) in overrides {
        merged.insert(k.to_uppercase(), (k.clone(), v.clone()));
    }
    if merged.is_empty() {
        return None;
    }

    let mut block: Vec<u16> = Vec::new();
    for (_, (k, v)) in merged {
        // A truly empty key can't form a valid entry; skip it. (Windows also
        // exposes per-drive cwd markers whose key begins with '=', e.g.
        // "=C:"; those are real inherited state and we pass them through.)
        if k.is_empty() {
            continue;
        }
        block.extend(k.encode_utf16());
        block.push(u16::from(b'='));
        block.extend(v.encode_utf16());
        block.push(0);
    }
    // Terminating null for the block (on top of the last entry's null).
    block.push(0);
    Some(block)
}

pub fn build_command_line(cmd: &str, args: &[&str]) -> String {
    let mut line = quote_arg(cmd);
    for a in args {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    line
}

/// The command line for spawning `cmd` with `args`. A `.cmd`/`.bat` target runs
/// under the system `cmd.exe` with batch-safe argument encoding
/// ([`crate::cmdline`]) — argv quoting alone lets cmd.exe split the line at a
/// bare `&`/`|` and expand `%VAR%`. Anything else uses argv quoting.
fn spawn_command_line(cmd: &str, args: &[&str]) -> io::Result<String> {
    if !crate::cmdline::is_batch(cmd) {
        return Ok(build_command_line(cmd, args));
    }
    // The system cmd.exe by full path, never a `cmd.exe` found first in the
    // child's working directory.
    let cmd_exe = match std::env::var_os("SystemRoot") {
        Some(root) => {
            let path = std::path::Path::new(&root).join("System32").join("cmd.exe");
            quote_arg(&path.to_string_lossy())
        }
        None => "cmd.exe".to_string(),
    };
    crate::cmdline::batch_command_line(&cmd_exe, cmd, args)
}

impl Sys {
    pub fn spawn_full(
        cmd: &str,
        args: &[&str],
        rows: u16,
        cols: u16,
        env: &[(String, String)],
        cwd: Option<&str>,
        suspended: bool,
    ) -> io::Result<Sys> {
        // Fail fast on a nonexistent cwd: CreateProcessW reports this as
        // ERROR_DIRECTORY, but checking up front lets us surface a clear
        // error before allocating pipes/console, and matches the contract
        // (no silent fallback to the parent's directory).
        if let Some(dir) = cwd {
            let meta = std::fs::metadata(dir)?;
            if !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("cwd is not a directory: {dir}"),
                ));
            }
        }
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
            // Prefer the chosen library; if it cannot create the console (its
            // OpenConsole.exe fails to start), this pane falls back to the
            // system's conhost rather than failing: slower, never broken.
            let mut library = PROVIDER.get().is_some();
            let mut hr = create_pseudo_console(size, in_read, out_write, &mut hpc, library);
            if hr != 0 && library {
                library = false;
                hpc = std::ptr::null_mut();
                hr = create_pseudo_console(size, in_read, out_write, &mut hpc, false);
            }
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

            match spawn_on_console(hpc, cmd, args, env, cwd, suspended) {
                Ok((process, thread)) => Ok(Sys {
                    hpc,
                    input_write: in_write,
                    output_read: out_read,
                    process,
                    thread,
                    exit_code: None,
                    library,
                }),
                Err(e) => {
                    close_pseudo_console(hpc, library);
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

    /// A blocking reader over the output, for a thread of its own.
    pub fn reader(&self) -> io::Result<Reader> {
        let mut handle: Handle = std::ptr::null_mut();
        let ok = unsafe {
            let me = GetCurrentProcess();
            DuplicateHandle(
                me,
                self.output_read,
                me,
                &mut handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(last_err());
        }
        Ok(Reader { handle })
    }

    /// Start a child created with `CREATE_SUSPENDED`. `ResumeThread` returns the
    /// previous suspend count, 0 for a thread that was already running.
    pub fn resume(&mut self) -> io::Result<()> {
        if unsafe { ResumeThread(self.thread) } == u32::MAX {
            return Err(last_err());
        }
        Ok(())
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
            resize_pseudo_console(
                self.library,
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

    /// The child's process id.
    ///
    /// Windows has no process groups in the POSIX sense; the equivalent bound on
    /// a process tree is a Job Object, and a caller assigns one by reopening the
    /// process from its id. Returns 0 if the handle can no longer be resolved
    /// (the process has exited), which callers treat as "nothing to do".
    pub fn uses_conpty_library(&self) -> bool {
        self.library
    }

    pub fn pid(&self) -> u32 {
        unsafe { GetProcessId(self.process) }
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
            close_pseudo_console(self.hpc, self.library);
            CloseHandle(self.thread);
            CloseHandle(self.process);
        }
    }
}

/// Build the attribute list binding the child to `hpc` and CreateProcess it.
unsafe fn spawn_on_console(
    hpc: Handle,
    cmd: &str,
    args: &[&str],
    env: &[(String, String)],
    cwd: Option<&str>,
    suspended: bool,
) -> io::Result<(Handle, Handle)> {
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
        // pseudoconsole. With it, the child falls back to its console: the
        // pty we just attached.
        si.startup_info.flags = STARTF_USESTDHANDLES;
        si.attribute_list = attr_list;
        let mut pi: ProcessInformation = std::mem::zeroed();
        let mut cmdline = wide(std::ffi::OsStr::new(&spawn_command_line(cmd, args)?));
        // Merged (parent + overrides) Unicode environment block. `env_block`
        // must outlive the call; a `None` merged env means pass NULL (inherit
        // verbatim), matching the old behavior when there are no overrides and
        // the parent env is somehow empty.
        let mut env_block = build_env_block(env);
        let (env_ptr, creation_flags) = match env_block.as_mut() {
            Some(b) => (
                b.as_mut_ptr() as *mut c_void,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            ),
            None => (std::ptr::null_mut(), EXTENDED_STARTUPINFO_PRESENT),
        };
        let creation_flags = if suspended {
            creation_flags | CREATE_SUSPENDED
        } else {
            creation_flags
        };
        // lpCurrentDirectory: a UTF-16, null-terminated directory when the
        // caller gave a cwd, else NULL (inherit the parent's). `dir_wide`
        // must outlive the call, hence the binding here.
        let dir_wide = cwd.map(|d| wide(std::ffi::OsStr::new(d)));
        let dir_ptr = match dir_wide.as_ref() {
            Some(w) => w.as_ptr(),
            None => std::ptr::null(),
        };
        if CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0, // no handle inheritance; the console attribute carries the pty
            creation_flags,
            env_ptr,
            dir_ptr,
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

// --- Alternative ConPTY implementations --------------------------------------
//
// `CreatePseudoConsole` in kernel32 always starts the system's `conhost.exe`.
// Windows Terminal ships its own console host, OpenConsole (MIT), launched by
// a `conpty.dll` that exports drop-in `Conpty*` versions of the same three
// calls and starts the `OpenConsole.exe` beside it. Loading that DLL swaps the
// console host for every pty this process creates afterwards.

type FnCreate = unsafe extern "system" fn(Coord, Handle, Handle, u32, *mut Handle) -> i32;
type FnResize = unsafe extern "system" fn(Handle, Coord) -> i32;
type FnClose = unsafe extern "system" fn(Handle);

struct Provider {
    path: PathBuf,
    create: FnCreate,
    resize: FnResize,
    close: FnClose,
}

static PROVIDER: OnceLock<Provider> = OnceLock::new();

/// `PSEUDOCONSOLE_INHERIT_CURSOR`: start at the terminal's cursor.
const PSEUDOCONSOLE_INHERIT_CURSOR: u32 = 0x1;
static INHERIT_CURSOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_inherit_cursor(on: bool) {
    INHERIT_CURSOR.store(on, std::sync::atomic::Ordering::Release);
}

fn console_flags() -> u32 {
    if INHERIT_CURSOR.load(std::sync::atomic::Ordering::Acquire) {
        PSEUDOCONSOLE_INHERIT_CURSOR
    } else {
        0
    }
}

/// Resolve dependencies of the loaded DLL from its own directory.
const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;

pub fn use_conpty_library(dll: &Path) -> io::Result<()> {
    if let Some(p) = PROVIDER.get() {
        return if p.path == dll {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("a ConPTY library is already in use: {}", p.path.display()),
            ))
        };
    }
    let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call;
    // the reserved handle is null as documented. Failure returns null. The
    // module is never freed: pseudo-consoles created through it may outlive
    // any scope we could tie it to.
    let module = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
    };
    if module.is_null() {
        return Err(last_err());
    }
    let find = |name: &'static str| -> io::Result<*mut c_void> {
        // SAFETY: `module` is a live module handle and `name` is NUL-terminated.
        let p = unsafe { GetProcAddress(module, name.as_ptr()) };
        if p.is_null() {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} does not export {}",
                    dll.display(),
                    name.trim_end_matches('\0')
                ),
            ))
        } else {
            Ok(p)
        }
    };
    let (create, resize, close) = (
        find("ConptyCreatePseudoConsole\0")?,
        find("ConptyResizePseudoConsole\0")?,
        find("ConptyClosePseudoConsole\0")?,
    );
    // SAFETY: each pointer is a non-null export whose documented prototype
    // (Windows Terminal's conpty.h) matches kernel32's function of the same
    // name without the prefix, which is the type it is cast to.
    let provider = unsafe {
        Provider {
            path: dll.to_path_buf(),
            create: std::mem::transmute::<*mut c_void, FnCreate>(create),
            resize: std::mem::transmute::<*mut c_void, FnResize>(resize),
            close: std::mem::transmute::<*mut c_void, FnClose>(close),
        }
    };
    let _ = PROVIDER.set(provider);
    Ok(())
}

pub fn conpty_library() -> Option<PathBuf> {
    PROVIDER.get().map(|p| p.path.clone())
}

/// The library's functions when `library` is set and one is loaded, else
/// kernel32's (the system's conhost).
unsafe fn create_pseudo_console(
    size: Coord,
    input: Handle,
    output: Handle,
    hpc: *mut Handle,
    library: bool,
) -> i32 {
    match PROVIDER.get().filter(|_| library) {
        // SAFETY: forwarded unchanged; the caller upholds CreatePseudoConsole's contract.
        Some(p) => unsafe { (p.create)(size, input, output, console_flags(), hpc) },
        None => unsafe { CreatePseudoConsole(size, input, output, console_flags(), hpc) },
    }
}

unsafe fn resize_pseudo_console(library: bool, hpc: Handle, size: Coord) -> i32 {
    match PROVIDER.get().filter(|_| library) {
        // SAFETY: `hpc` came from the same provider's create call.
        Some(p) => unsafe { (p.resize)(hpc, size) },
        None => unsafe { ResizePseudoConsole(hpc, size) },
    }
}

unsafe fn close_pseudo_console(hpc: Handle, library: bool) {
    match PROVIDER.get().filter(|_| library) {
        // SAFETY: `hpc` came from the same provider's create call.
        Some(p) => unsafe { (p.close)(hpc) },
        None => unsafe { ClosePseudoConsole(hpc) },
    }
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

    #[test]
    fn a_missing_conpty_library_is_an_error() {
        use super::use_conpty_library;
        let err = use_conpty_library(std::path::Path::new("Z:/definitely/not/here/conpty.dll"))
            .expect_err("no such file");
        assert_ne!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_library_without_the_conpty_exports_is_rejected() {
        // kernel32 exports CreatePseudoConsole but not the Conpty* names, so it
        // must be refused rather than half-loaded.
        use super::{conpty_library, use_conpty_library};
        let err = use_conpty_library(std::path::Path::new("kernel32.dll"))
            .expect_err("no Conpty exports");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(
            conpty_library().is_none(),
            "a failed load must not install a provider"
        );
    }
}
