# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-08-29

### Added
- `Pty::spawn_with_env(cmd, args, rows, cols, env)` — spawn with per-child
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
- `Pty::spawn(cmd, args, rows, cols)` — child on a fresh pseudo-terminal;
  `read_timeout` (None/`Some(0)`/`Some(n)` semantics), `write`, `resize`,
  `wait`/`try_wait`/`kill`. Dropping releases the terminal without killing
  the child (std convention).
- Windows backend: ConPTY via own kernel32 externs — `CreatePseudoConsole`
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

Sixth crate of the nativelite **agent terminal** suite — the `amux`
enabler (see `roadmap/agent-terminal-suite.md` in `nativelite/ops`).

[Unreleased]: https://github.com/nativelite/pty-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/nativelite/pty-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nativelite/pty-rs/releases/tag/v0.1.0
