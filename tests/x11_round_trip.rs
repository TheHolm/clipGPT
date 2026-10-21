//! A genuine end-to-end check of the real `clip_gpt` binary on X11: seeds
//! the clipboard with `xclip`, runs the compiled binary against a small
//! local HTTP server standing in for the LLM, and reads the clipboard
//! back with `xclip`. This is the only way to exercise the real
//! clipboard-owner re-exec (`--clipboard-owner=x11`, see
//! `clipboard_backend/owner.rs`): that mechanism relies on
//! `std::env::current_exe()`, which inside a `cargo test` *unit* test
//! resolves to the test harness binary, not `clip_gpt` - so this part of
//! `main.rs`/`clipboard_backend.rs` can only be covered from a true
//! external subprocess, which integration tests under `tests/` are.
//!
//! Ignored by default (needs a real or virtual X server and `xclip`);
//! see README.md "Testing" for how to run it.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Serialises the tests in this file, which share one real X clipboard.
static X11_CLIPBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn x11_lock() -> std::sync::MutexGuard<'static, ()> {
    X11_CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `program` with `args`, feeding nothing on stdin, and return its
/// (status, stdout, stderr) as strings. Panics if it cannot even start.
fn run(program: &str, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Replace the X11 `CLIPBOARD` selection with `text` using `xclip`, and
/// wait for it to actually own the selection (so the read below does not
/// race it).
fn xclip_copy(text: &str) {
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("cannot run xclip - install it to run this test");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "xclip -selection clipboard"
    );
    std::thread::sleep(Duration::from_millis(200));
}

/// Like [`xclip_copy`], but offered under the given X11 target (MIME
/// type) instead of the usual text ones, with raw `data` (binary for an
/// image, or a password marker's irrelevant placeholder value).
fn xclip_copy_as(target: &str, data: &[u8]) {
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard", "-target", target])
        .stdin(Stdio::piped())
        .spawn()
        .expect("cannot run xclip - install it to run this test");
    child.stdin.take().unwrap().write_all(data).unwrap();
    assert!(child.wait().unwrap().success(), "xclip -target {target}");
    std::thread::sleep(Duration::from_millis(200));
}

/// The X11 `CLIPBOARD` selection's current text, via `xclip`.
fn xclip_paste() -> String {
    let (status, out, err) = run("xclip", &["-selection", "clipboard", "-o"]);
    assert_eq!(status, 0, "xclip -o: {err}");
    out
}

/// A minimal HTTP server standing in for an OpenAI-compatible endpoint:
/// answers the first request on a background thread with one fixed 200 OK
/// JSON body holding `reply` as the assistant message (request parsing
/// mirrors net.rs's own `serve()` test helper: read headers, then the
/// body by Content-Length). Returns the `http://127.0.0.1:<port>/` base
/// URL.
fn fake_llm_server(reply: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let reply = reply.to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap();
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let len = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if raw.len() >= end + 4 + len || n == 0 {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let body =
            format!(r#"{{"choices":[{{"message":{{"role":"assistant","content":"{reply}"}}}}]}}"#);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
    });
    format!("http://127.0.0.1:{port}/v1/chat/completions")
}

/// A config file with one `[default]` profile pointing at `api_url`,
/// written to a fresh temp file unique to this call. Returns its path
/// (the caller is responsible for deleting it).
fn temp_config(api_url: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "clipgpt-x11-integration-{}-{n}.toml",
        std::process::id(),
    ));
    std::fs::write(
        &path,
        format!("[default]\napi_url = \"{api_url}\"\nmodel = \"m\"\nbeep_duration = 0\n"),
    )
    .unwrap();
    path
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn response_replaces_clipboard_via_the_real_binary() {
    let _guard = x11_lock();
    let base = fake_llm_server("INTEGRATION-REPLY");
    let config = temp_config(&base);
    xclip_copy("original clipboard text");

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", config.to_str().unwrap()]);
    std::fs::remove_file(&config).unwrap();

    assert_eq!(status, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert!(
        stdout.contains("Response copied to clipboard!"),
        "{stdout:?}"
    );
    assert_eq!(xclip_paste(), "INTEGRATION-REPLY");
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn a_secret_is_refused_and_the_clipboard_is_left_alone() {
    let _guard = x11_lock();
    let base = fake_llm_server("should never be requested");
    let config = temp_config(&base);
    let secret = "-----BEGIN OPENSSH PRIVATE KEY-----\nabcdefgh\n-----END OPENSSH PRIVATE KEY-----";
    xclip_copy(secret);

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", config.to_str().unwrap()]);
    std::fs::remove_file(&config).unwrap();

    assert_eq!(status, 1, "stdout={stdout:?} stderr={stderr:?}");
    assert!(stderr.contains("looks like"), "{stderr:?}");
    // Refused before anything was sent: the original secret is still
    // there, not replaced by anything (not even a placeholder).
    assert_eq!(xclip_paste(), secret);
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn hidden_characters_are_refused_and_the_clipboard_is_replaced() {
    let _guard = x11_lock();
    let base = fake_llm_server("should never be requested");
    let config = temp_config(&base);
    // A Unicode tag character pair: invisible, a known prompt-injection
    // channel.
    xclip_copy("hello\u{E0001}\u{E0069}");

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", config.to_str().unwrap()]);
    std::fs::remove_file(&config).unwrap();

    assert_eq!(status, 1, "stdout={stdout:?} stderr={stderr:?}");
    assert!(stderr.contains("hidden characters"), "{stderr:?}");
    // Unlike a secret, the clipboard is replaced by a notice - the
    // hidden payload must not be left pasteable either.
    assert!(xclip_paste().contains("hidden characters"));
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn an_unused_profile_warns_on_stderr() {
    let _guard = x11_lock();
    let base = fake_llm_server("REPLY");
    let path = std::env::temp_dir().join(format!(
        "clipgpt-x11-integration-unused-{}.toml",
        std::process::id()
    ));
    std::fs::write(
        &path,
        format!(
            "[nomodel]\ninput = [\"text\"]\n\
             [default]\napi_url = \"{base}\"\nmodel = \"m\"\n"
        ),
    )
    .unwrap();
    xclip_copy("some text");

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(
        exe,
        &["-c", path.to_str().unwrap(), "-p", "nomodel,default"],
    );
    std::fs::remove_file(&path).unwrap();

    assert_eq!(status, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert!(
        stderr.contains("[nomodel] is selected but handles nothing (overridden by [default])"),
        "{stderr:?}"
    );
    assert_eq!(xclip_paste(), "REPLY");
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn a_password_manager_marker_is_refused_and_the_clipboard_is_replaced() {
    let _guard = x11_lock();
    let base = fake_llm_server("should never be requested");
    let config = temp_config(&base);
    xclip_copy_as("x-kde-passwordManagerHint", b"irrelevant-value");

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", config.to_str().unwrap()]);
    std::fs::remove_file(&config).unwrap();

    assert_eq!(status, 1, "stdout={stdout:?} stderr={stderr:?}");
    assert!(stderr.contains("password"), "{stderr:?}");
    assert!(xclip_paste().contains("password"));
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn text_offered_only_to_an_image_profile_is_unhandled() {
    let _guard = x11_lock();
    let base = fake_llm_server("should never be requested");
    let path = std::env::temp_dir().join(format!(
        "clipgpt-x11-integration-unhandled-{}.toml",
        std::process::id()
    ));
    std::fs::write(
        &path,
        format!("[default]\ninput = [\"image\"]\napi_url = \"{base}\"\nmodel = \"m\"\n"),
    )
    .unwrap();
    xclip_copy("plain text, no image profile selected");

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", path.to_str().unwrap()]);
    std::fs::remove_file(&path).unwrap();

    assert_eq!(status, 1, "stdout={stdout:?} stderr={stderr:?}");
    assert!(stderr.contains("no selected profile accepts"), "{stderr:?}");
    // Left unchanged, not even a placeholder.
    assert_eq!(xclip_paste(), "plain text, no image profile selected");
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn timeout_override_is_accepted_and_still_works() {
    let _guard = x11_lock();
    let base = fake_llm_server("TIMEOUT-OVERRIDE-REPLY");
    let config = temp_config(&base);
    xclip_copy("original text");

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", config.to_str().unwrap(), "--timeout", "5"]);
    std::fs::remove_file(&config).unwrap();

    assert_eq!(status, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert!(
        stdout.contains("Response copied to clipboard!"),
        "{stdout:?}"
    );
    assert_eq!(xclip_paste(), "TIMEOUT-OVERRIDE-REPLY");
}

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn an_empty_llm_reply_is_an_error_and_the_clipboard_is_left_alone() {
    let _guard = x11_lock();
    let base = fake_llm_server(""); // empty assistant message content
    let config = temp_config(&base);
    xclip_copy("original text, should survive");

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", config.to_str().unwrap()]);
    std::fs::remove_file(&config).unwrap();

    assert_eq!(status, 1, "stdout={stdout:?} stderr={stderr:?}");
    assert!(stderr.contains("LLM response was empty"), "{stderr:?}");
    assert_eq!(xclip_paste(), "original text, should survive");
}

/// A 2x2 red PNG, the smallest valid image small enough to embed here.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0xFD, 0xD4, 0x9A,
    0x73, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x44, 0x0C, 0x10, 0x0A, 0x00, 0x1F, 0xEE, 0x03, 0xFD, 0x8B, 0x5F, 0x14, 0xD4, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

#[test]
#[ignore = "needs a real or virtual X server (DISPLAY) and xclip, e.g. xvfb-run"]
fn an_image_is_sent_and_replaced_by_the_reply() {
    let _guard = x11_lock();
    let base = fake_llm_server("IMAGE-REPLY");
    let config = temp_config(&base);
    xclip_copy_as("image/png", TINY_PNG);

    let exe = env!("CARGO_BIN_EXE_clip_gpt");
    let (status, stdout, stderr) = run(exe, &["-c", config.to_str().unwrap()]);
    std::fs::remove_file(&config).unwrap();

    assert_eq!(status, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert!(
        stdout.contains("Response copied to clipboard!"),
        "{stdout:?}"
    );
    assert_eq!(xclip_paste(), "IMAGE-REPLY");
}
