//! `inherit_cursor` is process-wide, and a console started with it waits for
//! its cursor-position query to be answered, so it has a test binary (a
//! process) of its own: the other tests never answer that query.

#![cfg(windows)]

use std::time::{Duration, Instant};

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn an_inheriting_console_asks_where_the_cursor_is_and_starts_there() {
    pty::inherit_cursor(true);
    let mut p = pty::Pty::spawn("cmd", &["/C", "echo inherit-marker"], 24, 80).unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let mut answered = false;
    while Instant::now() < end && !contains(&out, b"inherit-marker") {
        if let Some(n) = p
            .read_timeout(&mut buf, Duration::from_millis(200))
            .unwrap()
        {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        // Answer the console's queries as a terminal would: the cursor is on
        // row 6 (five lines of a banner above it), and a plain VT100.
        if !answered && contains(&out, b"\x1b[6n") {
            p.write(b"\x1b[6;1R").unwrap();
            answered = true;
        }
        if contains(&out, b"\x1b[c") && !contains(&out, b"\x1b[?1;0c") {
            p.write(b"\x1b[?1;0c").unwrap();
            out.extend_from_slice(b"\x1b[?1;0c"); // answered; do not repeat
        }
    }
    pty::inherit_cursor(false);
    assert!(
        answered,
        "no cursor-position query: {:?}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        contains(&out, b"inherit-marker"),
        "no output after answering: {:?}",
        String::from_utf8_lossy(&out)
    );
    // The console never homes the cursor to row 1 over the banner.
    assert!(
        !contains(&out, b"\x1b[H") && !contains(&out, b"\x1b[1;1H"),
        "the console redrew from the top: {:?}",
        String::from_utf8_lossy(&out)
    );
    let _ = p.wait();
}
