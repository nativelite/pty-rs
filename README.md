# pty-rs
**Spawn a child process on a real pseudo-terminal**, built entirely on the
Rust standard library. **Zero dependencies** — no `libc`/`windows` crates:
the OS boundary is this crate's own `extern` blocks. **ConPTY**
(`CreatePseudoConsole`, Windows 10 1809+) on Windows; the classic POSIX pty
(`posix_openpt`/`grantpt`/`unlockpt` + `setsid`/`TIOCSCTTY`) on Unix.

A process on a PTY believes it is talking to a real terminal: it enables
colors, draws progress UIs, and emits the VT byte stream a terminal would
receive. That stream is exactly what this crate hands you — parse it with
the [`ansi`](https://github.com/nativelite/ansi-rs) crate, or pipe it on.
This is the enabler for hosting interactive CLIs (a coding agent, a shell)
inside another program.

## Usage

```rust,no_run
use std::time::Duration;

let mut pty = pty::Pty::spawn("cmd", &["/C", "echo hello"], 24, 80)?;
let mut buf = [0u8; 4096];
while let Some(n) = pty.read_timeout(&mut buf, Duration::from_secs(2))? {
    if n == 0 { break; } // the terminal closed
    handle_vt_bytes(&buf[..n]);
}
pty.write(b"like keystrokes\r\n")?;   // to the child's terminal input
pty.resize(40, 120)?;                 // the child sees the new size
let code = pty.wait()?;               // also: try_wait(), kill()
# fn handle_vt_bytes(_: &[u8]) {}
# std::io::Result::Ok(())
```

Semantics, stated plainly:

- `read_timeout` → `None` timeout / `Some(0)` end-of-stream / `Some(n)`
  data. End-of-stream timing is platform-dependent (some Windows builds
  keep the console open until drop) — output written before exit is always
  drainable after `wait()`; drain with short timeouts.
- Exit codes: a Unix child killed by a signal reports `128 + signo`.
- Dropping a `Pty` releases the terminal but — like `std::process::Child` —
  does **not** kill the child.
- `spawn_with_env(cmd, args, rows, cols, env)` injects per-child environment
  variables: the child sees the **parent environment merged with `env`**
  (overrides win by key), so `PATH`/`SystemRoot` survive while a credential
  can be set for one child alone. `spawn` is `spawn_with_env` with no
  overrides.

## Platform notes (the hard-won bits)

- **Windows:** the child is bound to the pseudoconsole via the
  `PSEUDOCONSOLE` proc-thread attribute, and — crucially —
  `STARTF_USESTDHANDLES` with NULL handles, without which a child whose
  parent has redirected stdio (a test harness, a service) writes past the
  pty entirely. Timeouts poll `PeekNamedPipe` (anonymous pipes can't do
  overlapped I/O); `ERROR_BROKEN_PIPE` is end-of-stream. Command lines are
  built with proper argv quoting (unit-tested against the
  `CommandLineToArgvW` rules).
- **Unix:** the child is spawned with `std::process::Command` over the
  slave fd, made session leader + controlling tty in `pre_exec`; the
  master carries `FD_CLOEXEC` so it never leaks into children. Timeouts
  use `poll`; `EIO` from a closed slave is end-of-stream. Linux is
  CI-exercised; macOS constants are in place and compile-checked, not yet
  runtime-verified.

## What's deliberately out of scope

Multiplexing, layout, scrollback, and session persistence — the `amux`
product's concerns. Per-child **env injection** is supported (see
`spawn_with_env` above); **cwd** customization is not (children inherit the
parent's working directory). One child, one PTY, bytes in, bytes out.

## Correctness

The integration tests run **real children on real PTYs**, deadline-bounded:
output arrives through the terminal, written input reaches an interactive
shell and its response comes back, resize succeeds and the child observes a
tty, exit codes propagate (twice-`wait` stable), `try_wait` transitions
after `kill` (and killing an exited child is a no-op), pre-exit output is
drainable post-exit, and a missing program errors cleanly without leaking
handles. Windows runs these locally; Linux runs them in CI.

## Development

```bash
python dev.py check   # zero-dependency guard + cargo test (what CI runs)
```
