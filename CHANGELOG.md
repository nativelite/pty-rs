# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/nativelite/pty-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nativelite/pty-rs/releases/tag/v0.1.0
