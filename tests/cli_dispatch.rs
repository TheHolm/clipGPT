//! Checks of `clip_gpt`'s command-line dispatch that need no clipboard or
//! network access, so they run everywhere (not `#[ignore]`d): `--help`,
//! `--version`, `--list-profiles`, `--show-profile` and an unknown
//! argument. These exercise `main()`'s own dispatch, which a unit test
//! cannot reach directly (it reads the process's real argv).

use std::process::{Command, Stdio};

/// Run the real `clip_gpt` binary with `args` and no stdin, and return
/// its (status, stdout, stderr) as strings.
fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_clip_gpt"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn help_and_version() {
    let (status, out, _) = run(&["--help"]);
    assert_eq!(status, 0);
    assert!(out.starts_with("Usage: clip_gpt"), "{out:?}");
    let (status, out, _) = run(&["-h"]);
    assert_eq!(status, 0);
    assert!(out.starts_with("Usage: clip_gpt"), "{out:?}");

    let (status, out, _) = run(&["--version"]);
    assert_eq!(status, 0);
    assert!(out.trim() == format!("clip_gpt version {}", env!("CARGO_PKG_VERSION")));
    let (status, out, _) = run(&["-V"]);
    assert_eq!(status, 0);
    assert_eq!(
        out.trim(),
        format!("clip_gpt version {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn unknown_argument_is_an_error_with_usage() {
    let (status, out, err) = run(&["--no-such-option"]);
    assert_eq!(status, 1, "stdout={out:?} stderr={err:?}");
    assert!(err.contains("Unknown option '--no-such-option'"), "{err:?}");
    assert!(err.contains("Usage: clip_gpt"), "{err:?}");
    assert!(out.is_empty(), "{out:?}");
}

/// A temp config file with one profile per kind, so `--list-profiles` and
/// `--show-profile` have more than the implicit default to report.
fn temp_config() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "clipgpt-cli-dispatch-{}-{n}.toml",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "[default]\napi_url = \"https://llm.example/v1\"\nmodel = \"m\"\n\
         [ocr]\ninput = [\"image\"]\napi_url = \"https://llm.example/v1\"\nmodel = \"v\"\n",
    )
    .unwrap();
    path
}

#[test]
fn list_profiles() {
    let config = temp_config();
    let (status, out, err) = run(&["-c", config.to_str().unwrap(), "--list-profiles"]);
    std::fs::remove_file(&config).unwrap();
    assert_eq!(status, 0, "stdout={out:?} stderr={err:?}");
    assert_eq!(out, "default\nocr\n");
}

#[test]
fn show_profile() {
    let config = temp_config();
    let (status, out, err) = run(&[
        "-c",
        config.to_str().unwrap(),
        "-p",
        "default,ocr",
        "--show-profile",
    ]);
    std::fs::remove_file(&config).unwrap();
    assert_eq!(status, 0, "stdout={out:?} stderr={err:?}");
    assert!(
        out.contains("[default]") && out.contains("[ocr]"),
        "{out:?}"
    );
    assert!(out.contains("model") && out.contains("\"v\""), "{out:?}");
    assert_eq!(err, "", "no api_url warnings expected: {err:?}");
}

#[test]
fn show_profile_reports_an_invalid_api_url() {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "clipgpt-cli-dispatch-bad-{}-{n}.toml",
        std::process::id()
    ));
    std::fs::write(&path, "[default]\napi_url = \"not a url\"\nmodel = \"m\"\n").unwrap();
    let (status, out, err) = run(&["-c", path.to_str().unwrap(), "--show-profile"]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(status, 1, "stdout={out:?} stderr={err:?}");
    assert!(
        err.contains("Profile [default]: Invalid api_url"),
        "{err:?}"
    );
}
