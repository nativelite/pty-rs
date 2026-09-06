//! Integration tests for `pty` against **real child processes** on a real
//! pseudo-terminal — the only honest way to test this crate. Every test is
//! deadline-bounded so a regression hangs the suite for seconds, not
//! forever. Windows runs these locally; Linux runs them in CI.

use std::time::{Duration, Instant};

/// Platform shell one-liner: (command, args).
fn shell(script: &str) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        ("cmd", vec!["/C".into(), script.into()])
    } else {
        ("sh", vec!["-c".into(), script.into()])
    }
}

/// Read until `needle` appears in the accumulated output, EOF, or the
/// deadline; returns everything read.
fn read_until(pty: &mut pty::Pty, needle: &[u8], deadline: Duration) -> Vec<u8> {
    let end = Instant::now() + deadline;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while Instant::now() < end {
        match pty
            .read_timeout(&mut buf, Duration::from_millis(200))
            .unwrap()
        {
            Some(0) => break,
            Some(n) => {
                out.extend_from_slice(&buf[..n]);
                if windows_contains(&out, needle) {
                    break;
                }
            }
            None => {}
        }
    }
    out
}

fn windows_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len().max(1)).any(|w| w == needle)
}

#[test]
fn child_output_arrives_through_the_terminal() {
    let (cmd, args) = shell("echo pty-marker-1");
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argrefs, 24, 80).unwrap();
    let out = read_until(&mut p, b"pty-marker-1", Duration::from_secs(10));
    assert!(
        windows_contains(&out, b"pty-marker-1"),
        "output: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

#[test]
fn exit_codes_are_reported() {
    let (cmd, args) = shell("exit 3");
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argrefs, 24, 80).unwrap();
    assert_eq!(p.wait().unwrap(), 3);
    // wait() twice is stable
    assert_eq!(p.wait().unwrap(), 3);
}

#[test]
fn try_wait_transitions_from_running_to_exited() {
    let (cmd, args) = shell(if cfg!(windows) {
        "ping -n 30 127.0.0.1 > NUL"
    } else {
        "sleep 30"
    });
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argrefs, 24, 80).unwrap();
    assert_eq!(p.try_wait().unwrap(), None, "child should still be running");
    p.kill().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if p.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < end, "killed child never reaped");
        std::thread::sleep(Duration::from_millis(50));
    }
    p.kill().unwrap(); // killing an exited child is a no-op, not an error
}

#[test]
fn written_input_reaches_the_child() {
    // An interactive shell: write a command as if typed, expect its output.
    let (cmd, args) = if cfg!(windows) {
        ("cmd", vec!["/Q".to_string()])
    } else {
        ("sh", vec!["-i".to_string()])
    };
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argrefs, 24, 80).unwrap();
    p.write(b"echo pty-roundtrip-2\r\n").unwrap();
    let out = read_until(&mut p, b"pty-roundtrip-2", Duration::from_secs(10));
    assert!(
        windows_contains(&out, b"pty-roundtrip-2"),
        "output: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.write(b"exit\r\n").unwrap();
    p.wait().unwrap();
}

#[test]
fn resize_succeeds_and_child_sees_a_terminal() {
    let (cmd, args) = shell(if cfg!(windows) {
        // ConPTY: the child's console size is the pty size.
        "echo ok-3"
    } else {
        // On Unix, `test -t 1` proves stdout is a tty.
        "test -t 1 && echo is-a-tty-3"
    });
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argrefs, 30, 100).unwrap();
    p.resize(40, 120).unwrap();
    let needle: &[u8] = if cfg!(windows) {
        b"ok-3"
    } else {
        b"is-a-tty-3"
    };
    let out = read_until(&mut p, needle, Duration::from_secs(10));
    assert!(
        windows_contains(&out, needle),
        "output: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

#[test]
fn output_written_before_exit_is_drainable_afterwards() {
    // The terminal buffers: output written before the child exited must
    // still be readable after wait(). EOF timing is platform-dependent
    // (some Windows builds keep the console open until we drop it), so the
    // contract here is drain-then-quiet, not drain-then-EOF.
    let (cmd, args) = shell("echo done-4");
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argrefs, 24, 80).unwrap();
    p.wait().unwrap();
    let out = read_until(&mut p, b"done-4", Duration::from_secs(10));
    assert!(
        windows_contains(&out, b"done-4"),
        "output: {:?}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn spawn_of_missing_program_errors_cleanly() {
    assert!(pty::Pty::spawn("definitely-not-a-real-program-xyz", &[], 24, 80).is_err());
}

/// Shell one-liner that prints the value of environment variable `name`,
/// wrapped in unique markers so we can find it in the terminal's echo/prompt
/// noise. Emits `[MK[VALUE]MK]` — square brackets, deliberately NOT `<`/`>`
/// which cmd.exe would treat as redirection. Windows uses `echo`, Unix
/// `printf`.
fn echo_var(name: &str) -> String {
    if cfg!(windows) {
        format!("echo [MK[%{name}%]MK]")
    } else {
        format!("printf '[MK[%s]MK]' \"${name}\"")
    }
}

#[test]
fn injected_env_var_reaches_the_child() {
    let (cmd, args) = shell(&echo_var("PTY_INJECT_TEST"));
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env = vec![(
        "PTY_INJECT_TEST".to_string(),
        "injected-value-9".to_string(),
    )];
    let mut p = pty::Pty::spawn_with_env(cmd, &argrefs, 24, 80, &env).unwrap();
    let out = read_until(&mut p, b"]MK]", Duration::from_secs(10));
    assert!(
        windows_contains(&out, b"[MK[injected-value-9]MK]"),
        "expected injected value in output, got: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

#[test]
fn inherited_env_survives_injection() {
    // PATH is set in the parent and NOT among our overrides: the merge must
    // keep it (a from-scratch env would drop it). We inject an unrelated key
    // and assert PATH is still non-empty in the child.
    assert!(
        std::env::var_os("PATH").is_some(),
        "test harness expects PATH in the environment"
    );
    let (cmd, args) = shell(&echo_var("PATH"));
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env = vec![("PTY_UNRELATED_KEY".to_string(), "x".to_string())];
    let mut p = pty::Pty::spawn_with_env(cmd, &argrefs, 24, 80, &env).unwrap();
    let out = read_until(&mut p, b"]MK]", Duration::from_secs(10));
    // The child printed [MK[...PATH...]MK]; a preserved PATH is a non-empty
    // body between the markers.
    assert!(
        !windows_contains(&out, b"[MK[]MK]"),
        "PATH was empty in child (merge dropped it): {:?}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        windows_contains(&out, b"]MK]"),
        "never saw the closing marker: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

#[test]
fn override_wins_over_inherited_value() {
    // Set a var in the parent, then override it for the child only: the child
    // must see the override, and the parent's value is untouched.
    std::env::set_var("PTY_OVERRIDE_TEST", "parent-value");
    let (cmd, args) = shell(&echo_var("PTY_OVERRIDE_TEST"));
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env = vec![("PTY_OVERRIDE_TEST".to_string(), "child-value-7".to_string())];
    let mut p = pty::Pty::spawn_with_env(cmd, &argrefs, 24, 80, &env).unwrap();
    let out = read_until(&mut p, b"]MK]", Duration::from_secs(10));
    assert!(
        windows_contains(&out, b"[MK[child-value-7]MK]"),
        "override did not win, got: {:?}",
        String::from_utf8_lossy(&out)
    );
    // Parent's own environment is unchanged.
    assert_eq!(
        std::env::var("PTY_OVERRIDE_TEST").as_deref(),
        Ok("parent-value")
    );
    p.wait().unwrap();
}

/// Shell one-liner that prints the child's own working directory, wrapped in
/// the same `[MK[...]MK]` markers used for env values. Windows `cd` with no
/// argument prints the current directory; Unix `pwd`.
fn echo_cwd() -> String {
    if cfg!(windows) {
        "echo [MK[& cd & echo ]MK]".to_string()
    } else {
        "printf '[MK['; pwd | tr -d '\\n'; printf ']MK]'".to_string()
    }
}

/// A fresh temp directory unique to this process + a caller-supplied tag,
/// created on disk. Uses the OS temp dir via `std::env::temp_dir` so it works
/// on both platforms without extra deps.
fn make_temp_dir(tag: &str) -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("pty-cwd-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonicalize so comparisons survive symlinked temp dirs (e.g. macOS
    // /tmp -> /private/tmp, or 8.3 short paths on Windows).
    std::fs::canonicalize(&dir).unwrap()
}

/// Strip a Windows extended-length prefix (`\\?\`) that `canonicalize` adds,
/// so the string we look for matches what the child's shell prints.
fn strip_verbatim(p: &str) -> &str {
    p.strip_prefix(r"\\?\").unwrap_or(p)
}

#[test]
fn spawn_with_cwd_starts_child_there() {
    let dir = make_temp_dir("some");
    let (cmd, args) = shell(&echo_cwd());
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let dir_str = dir.to_string_lossy().into_owned();
    let mut p = pty::Pty::spawn_full(cmd, &argrefs, 24, 80, &[], Some(&dir_str)).unwrap();
    let out = read_until(&mut p, b"]MK]", Duration::from_secs(10));
    let want = strip_verbatim(&dir_str);
    assert!(
        windows_contains(&out, want.as_bytes()),
        "child cwd should be {want:?}, got: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

#[test]
fn spawn_with_none_cwd_inherits_parent() {
    // cwd = None must reproduce today's behavior: the child inherits the
    // parent's (this test process's) working directory.
    let parent = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let parent_str = parent.to_string_lossy().into_owned();
    let (cmd, args) = shell(&echo_cwd());
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn_full(cmd, &argrefs, 24, 80, &[], None).unwrap();
    let out = read_until(&mut p, b"]MK]", Duration::from_secs(10));
    let want = strip_verbatim(&parent_str);
    assert!(
        windows_contains(&out, want.as_bytes()),
        "child should inherit parent cwd {want:?}, got: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

#[test]
fn env_and_cwd_together() {
    // A child given both an injected env var and a cwd must see both.
    let dir = make_temp_dir("envcwd");
    let dir_str = dir.to_string_lossy().into_owned();
    // Print the injected var, then the cwd, so one child proves both.
    let script = if cfg!(windows) {
        "echo [MK[%PTY_BOTH_TEST%]MK] & echo [CW[& cd & echo ]CW]".to_string()
    } else {
        "printf '[MK[%s]MK]' \"$PTY_BOTH_TEST\"; printf '[CW['; pwd | tr -d '\\n'; printf ']CW]'"
            .to_string()
    };
    let (cmd, args) = shell(&script);
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env = vec![("PTY_BOTH_TEST".to_string(), "both-value-8".to_string())];
    let mut p = pty::Pty::spawn_full(cmd, &argrefs, 24, 80, &env, Some(&dir_str)).unwrap();
    let out = read_until(&mut p, b"]CW]", Duration::from_secs(10));
    assert!(
        windows_contains(&out, b"[MK[both-value-8]MK]"),
        "injected env var missing, got: {:?}",
        String::from_utf8_lossy(&out)
    );
    let want = strip_verbatim(&dir_str);
    assert!(
        windows_contains(&out, want.as_bytes()),
        "child cwd should be {want:?}, got: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

#[test]
fn nonexistent_cwd_errors() {
    let (cmd, args) = shell("echo unreachable");
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let missing = if cfg!(windows) {
        r"C:\pty-cwd-does-not-exist-xyz\nope"
    } else {
        "/pty-cwd-does-not-exist-xyz/nope"
    };
    let err = pty::Pty::spawn_full(cmd, &argrefs, 24, 80, &[], Some(missing));
    assert!(err.is_err(), "nonexistent cwd should error, not fall back");
}

#[test]
fn four_arg_spawn_still_inherits_env_unchanged() {
    // The pre-existing 4-arg spawn must behave exactly as before: the child
    // inherits a parent var with no overrides supplied.
    std::env::set_var("PTY_INHERIT_TEST", "inherited-value-5");
    let (cmd, args) = shell(&echo_var("PTY_INHERIT_TEST"));
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argrefs, 24, 80).unwrap();
    let out = read_until(&mut p, b"]MK]", Duration::from_secs(10));
    assert!(
        windows_contains(&out, b"[MK[inherited-value-5]MK]"),
        "4-arg spawn did not inherit env, got: {:?}",
        String::from_utf8_lossy(&out)
    );
    p.wait().unwrap();
}

/// **A pane child inherits no terminal but its own** (fd hygiene).
///
/// `spawn` opens the master, sets `FD_CLOEXEC` on it, and opens the slave with
/// no `FD_CLOEXEC` at all. The master's flag does not actually get set: `fcntl`
/// is variadic in C, this crate declared it as a plain three-argument function,
/// and on Apple ARM64 a variadic argument travels on the stack while a fixed one
/// travels in a register. The flag never reaches the kernel, and `fcntl` still
/// returns 0 — so the error check above it reports success on a call that did
/// nothing.
///
/// So every child inherits the master of its own terminal, plus the raw slave
/// descriptor `std` duplicated onto 0/1/2 and left open. A pane that outlives
/// its amux therefore pins terminals nothing can reclaim: the pty pool drains
/// one orphan at a time until the machine cannot open a terminal at all, and
/// the processes holding them cannot be killed — they are already exiting,
/// blocked revoking a controlling terminal another process still holds open.
/// Only a reboot clears it.
///
/// `ls -l /dev/fd` names every descriptor the child actually has. A character
/// device above fd 2 is a terminal that is not the child's own stdio, which is
/// the leak; the child's own terminal is legitimately on 0, 1 and 2.
///
/// Revert check: restore the three-argument `fcntl` declaration and this fails
/// naming a `0xf......` master on fd 3; drop the slave's `O_CLOEXEC` and it
/// fails naming a `0x10.....` slave. Every other test here spawns and reads
/// normally, and none of them observe either descriptor.
#[cfg(unix)]
#[test]
fn the_child_inherits_no_terminal_but_its_own() {
    let dir = make_temp_dir("fdleak");
    let out = dir.join("fds");
    let script = format!("ls -l /dev/fd > {} 2>&1", out.display());
    let (cmd, args) = shell(&script);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = pty::Pty::spawn(cmd, &argv, 24, 80).expect("spawn pane");
    let _ = read_until(&mut p, b"__never__", Duration::from_secs(5));
    let _ = p.wait();

    let listing = std::fs::read_to_string(&out).expect("child wrote its fd table");
    let mut leaks = Vec::new();
    for line in listing.lines() {
        // "crw-rw-rw-  1 root  tty  0xf00000f  Sep  5 20:21 3": mode first,
        // descriptor number last. Only character devices can be a terminal.
        if !line.starts_with('c') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(dev), Some(fd)) = (
            fields.get(4),
            fields.last().and_then(|f| f.parse::<u32>().ok()),
        ) else {
            continue;
        };
        if fd > 2 {
            leaks.push(format!("fd {fd} -> device {dev}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        leaks.is_empty(),
        "child inherited terminals that are not its stdio:\n  {}\nfull table:\n{listing}",
        leaks.join("\n  ")
    );
}
