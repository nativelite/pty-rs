# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- **Pane children no longer inherit terminals that are not their own** (unix).
  Two descriptors leaked into every spawned child:
  - The **master**. `spawn` set `FD_CLOEXEC` on it and checked the result, but
    the flag was never applied: `fcntl` is variadic in C and was declared here
    as a plain three-argument function. On Apple ARM64 a variadic argument is
    passed on the stack while a fixed one is passed in a register, so the flag
    the callee read was never the flag we sent, and `fcntl` returned 0, so the
    error check reported success on a call that did nothing. Now declared
    variadic, as `ioctl` beside it already was.
  - The **slave**. It was opened with no `FD_CLOEXEC` at all, so the spare
    descriptor `std` duplicates onto the child's 0/1/2 stayed open in the child
    as well. Now opened with `O_CLOEXEC`, which leaves no window in which it is
    inheritable; `std` clears the flag on 0/1/2 when it dup2s them, so the
    child keeps its stdio and loses only the spare.

  The effect was cumulative and, on macOS, unrecoverable without a reboot: a
  pane that outlived its parent pinned terminals nothing could reclaim, the pty
  pool drained one orphan at a time until the machine could not open a terminal
  at all, and the processes holding them could not be killed: they were
  already exiting, blocked revoking a controlling terminal another process
  still held open. Covered by
  `the_child_inherits_no_terminal_but_its_own`, which reads the child's own
  descriptor table and fails on any terminal above fd 2.

## [0.3.0] - 2026-08-29

### Added
- `Pty::spawn_full(cmd, args, rows, cols, env, cwd)`: the full spawn form,
  adding a per-child **working directory** on top of env injection. `cwd`
  is `Option<&str>`: `Some(dir)` starts the child in `dir` (the enabler for
  a spawned agent auto-loading the `CLAUDE.md` in its own tree); `None`
  inherits the parent's cwd (previous behavior). `spawn` and `spawn_with_env`
  are unchanged and now delegate to `spawn_full` (`spawn` → empty env +
  `None` cwd; `spawn_with_env` → env + `None` cwd).
  - Windows: the directory is passed (UTF-16, null-terminated) as
    `CreateProcessW`'s `lpCurrentDirectory`; `None` → NULL (inherit).
  - Unix: applied via `Command::current_dir` before exec, so the chdir and
    the env merge both take effect.
  - A nonexistent `cwd` is surfaced as an `io::Error` from `spawn_full` (no
    silent fallback to the parent's directory).
- Integration tests: a child spawned with `Some(dir)` reports that dir;
  `None` reports the parent's cwd; env + cwd together are both seen; a
  nonexistent cwd errors.

## [0.2.0] - 2026-08-29

### Added
- `Pty::spawn_with_env(cmd, args, rows, cols, env)`: spawn with per-child
  environment injection. The child's environment is the parent's environment
  **merged** with the caller's overrides (overrides win by key), so inherited
  variables like `PATH`/`SystemRoot` are preserved while credentials can be
  set for one child alone. The existing 4-arg `Pty::spawn` is unchanged and
  now delegates to `spawn_with_env` with no overrides.
  - Windows: builds a UTF-16, double-null-terminated environment block from
    the merged, case-insensitively-sorted map and passes it to
    `CreateProcessW` with `CREATE_UNICODE_ENVIRONMENT`. Keys merge
    case-insensitively (`Path` overrides an inherited `PATH`).
  - Unix: overrides are layered onto the inherited environment via
    `Command::envs` before exec.
- Integration tests: an injected variable reaches the child, an inherited
  `PATH` survives the merge, an override beats the inherited value (parent
  env untouched), and the 4-arg `spawn` still inherits unchanged.

## [0.1.0] - 2026-08-28

### Added
- `Pty::spawn(cmd, args, rows, cols)`: child on a fresh pseudo-terminal;
  `read_timeout` (None/`Some(0)`/`Some(n)` semantics), `write`, `resize`,
  `wait`/`try_wait`/`kill`. Dropping releases the terminal without killing
  the child (std convention).
- Windows backend: ConPTY via own kernel32 externs: `CreatePseudoConsole`
  + `PSEUDOCONSOLE` proc-thread attribute + `STARTF_USESTDHANDLES` with
  NULL handles (required when the parent's stdio is redirected, or child
  output bypasses the pty), `PeekNamedPipe` timeout polling, argv-correct
  command-line quoting (unit-tested).
- Unix backend: `posix_openpt`/`grantpt`/`unlockpt` master with
  `FD_CLOEXEC`, child via `std::process::Command` + `pre_exec`
  (`setsid` + `TIOCSCTTY`), `poll` timeouts, `TIOCSWINSZ` resize, signal
  exits reported as `128 + signo`. Linux CI-exercised; macOS
  compile-checked.
- Integration suite against real children on real PTYs, deadline-bounded:
  output/input round-trips through an interactive shell, resize + tty
  detection, exit codes, kill/reap transitions, post-exit drain, clean
  failure for missing programs.

Sixth crate of the nativelite **agent terminal** suite: the `atrium`
enabler (see `roadmap/agent-terminal-suite.md` in `nativelite/ops`).

[Unreleased]: https://github.com/nativelite/pty-rs/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/nativelite/pty-rs/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/nativelite/pty-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nativelite/pty-rs/releases/tag/v0.1.0
