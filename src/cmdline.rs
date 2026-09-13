//! Building a `cmd.exe` command line that passes arguments through intact.
//!
//! A `.cmd`/`.bat` file cannot be launched by `CreateProcessW` directly; it runs
//! under `cmd.exe /c`, and **cmd.exe parses that line itself** before the batch
//! file sees it, then the batch file's own `%*` expansion parses it again. Plain
//! argv quoting (the `CommandLineToArgvW` convention) is not enough: an unquoted
//! `&`, `|`, `<`, `>` or `^` splits or redirects the command — every argument
//! after it is silently dropped — and `%NAME%` is expanded even inside quotes.
//! npm-installed CLIs (Claude Code included) are exactly such shims, so a prompt
//! or branch name containing `A & B` used to kill the launch.
//!
//! The rules here are the ones the Rust standard library adopted for batch files
//! after CVE-2024-24576 (`make_bat_command_line` / `append_bat_arg`): quote every
//! argument that isn't made of known-safe characters, escape `"` by doubling it,
//! double backslashes only where they precede a quote, and defuse `%` with the
//! zero-length `%cd:~,%` substring so no `%NAME%` can form. CR, LF and NUL cannot
//! be carried through cmd.exe at all and are rejected. The encoding round-trips
//! through the CRT argv parser that node, Python and Rust programs use.
//!
//! Pure string functions, available on every platform so the rules are tested
//! everywhere; only Windows spawns through them.

use std::io;

/// Is `program` a batch file that must run under `cmd.exe`? (`.bat` / `.cmd`,
/// any case.)
pub fn is_batch(program: &str) -> bool {
    let lower = program.to_ascii_lowercase();
    lower.ends_with(".bat") || lower.ends_with(".cmd")
}

/// Encode one argument for a `cmd.exe /c` line (see the module docs). Errors
/// with `InvalidInput` on CR, LF or NUL, which cmd.exe would truncate at.
pub fn quote_batch_arg(arg: &str) -> io::Result<String> {
    if arg.contains(&['\r', '\n', '\0'][..]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a batch-file argument cannot contain CR, LF or NUL",
        ));
    }
    // Quote unless every character is known-safe; a trailing `\` or an empty
    // argument also needs quotes.
    const UNQUOTED: &str = r"#$*+-./:?@\_";
    let quote = arg.is_empty()
        || arg.ends_with('\\')
        || arg.chars().any(|c| {
            (c.is_ascii() && !(c.is_ascii_alphanumeric() || UNQUOTED.contains(c))) || c.is_control()
        });
    let mut out = String::with_capacity(arg.len() + 2);
    if quote {
        out.push('"');
    }
    let mut backslashes = 0usize;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            if c == '"' {
                // n backslashes before a quote become 2n; the doubled quote is
                // the escape.
                out.extend(std::iter::repeat('\\').take(backslashes));
                out.push('"');
            } else if c == '%' {
                // `%%cd:~,%` — a literal `%` then an empty substring expansion,
                // which stops cmd.exe pairing this `%` into a `%NAME%`.
                out.push_str("%%cd:~,");
            }
            backslashes = 0;
        }
        out.push(c);
    }
    if quote {
        out.extend(std::iter::repeat('\\').take(backslashes));
        out.push('"');
    }
    Ok(out)
}

/// The full command line that runs batch file `script` with `args` under
/// `cmd_exe`: `<cmd_exe> /e:ON /v:OFF /d /c ""<script>" <args…>"`.
///
/// `/e:ON` enables the extensions the `%` defusing relies on, `/v:OFF` keeps
/// `!` literal, `/d` skips AutoRun. The outer quote pair is the one `/c` strips.
/// `cmd_exe` is emitted as given (the caller quotes a path with spaces). Errors
/// if `script` contains `"` or ends in `\` (not a valid file name), or any
/// argument is rejected by [`quote_batch_arg`].
pub fn batch_command_line(cmd_exe: &str, script: &str, args: &[&str]) -> io::Result<String> {
    if script.contains('"') || script.ends_with('\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a batch file name cannot contain `\"` or end with `\\`",
        ));
    }
    let mut line = format!("{cmd_exe} /e:ON /v:OFF /d /c \"\"{script}\"");
    for a in args {
        line.push(' ');
        line.push_str(&quote_batch_arg(a)?);
    }
    line.push('"');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> String {
        quote_batch_arg(s).unwrap()
    }

    #[test]
    fn batch_detection_is_by_extension_any_case() {
        assert!(is_batch(r"C:\npm\claude.cmd"));
        assert!(is_batch("RUN.BAT"));
        assert!(!is_batch("node.exe"));
        assert!(!is_batch("cmd"));
        assert!(!is_batch("notes.cmd.txt"));
    }

    #[test]
    fn safe_arguments_pass_bare() {
        assert_eq!(q("--permission-mode"), "--permission-mode");
        assert_eq!(q(r"C:\dir\file.txt"), r"C:\dir\file.txt");
        assert_eq!(q("a@b#c$d*e+f?g"), "a@b#c$d*e+f?g");
    }

    #[test]
    fn cmd_metacharacters_force_quotes() {
        for a in [
            "a&b", "x|y", "p>q", "<in", "c^d", "(p)", "s;c", "e=v", "b!", "t~", "k`",
        ] {
            assert_eq!(q(a), format!("\"{a}\""), "{a}");
        }
        assert_eq!(q("has space"), "\"has space\"");
        assert_eq!(q(""), "\"\"");
    }

    #[test]
    fn percent_is_defused() {
        assert_eq!(q("%PATH%"), "\"%%cd:~,%PATH%%cd:~,%\"");
        assert_eq!(q("100%"), "\"100%%cd:~,%\"");
    }

    #[test]
    fn quotes_double_and_backslashes_double_only_before_a_quote() {
        assert_eq!(q(r#"a"b"#), r#""a""b""#);
        assert_eq!(q(r#"q\"x"#), r#""q\\""x""#);
        assert_eq!(q(r"trail\"), r#""trail\\""#);
        assert_eq!(q(r"back\\slash"), r"back\\slash");
    }

    #[test]
    fn line_breaks_and_nul_are_rejected() {
        for a in ["a\nb", "a\rb", "a\0b"] {
            let e = quote_batch_arg(a).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn batch_line_wraps_script_and_args_in_the_outer_quotes() {
        let line = batch_command_line("cmd.exe", r"C:\x\claude.cmd", &["-p", "a & b", "--x"]);
        assert_eq!(
            line.unwrap(),
            r#"cmd.exe /e:ON /v:OFF /d /c ""C:\x\claude.cmd" -p "a & b" --x""#
        );
        assert!(batch_command_line("cmd.exe", r#"bad"name.cmd"#, &[]).is_err());
        assert!(batch_command_line("cmd.exe", "x.cmd", &["ok", "no\nway"]).is_err());
    }
}
