//! Shared protocol for the hidden clipboard-owner helper processes
//! (`--clipboard-owner=x11` / `--clipboard-owner=wayland`; see the `x11`
//! submodule and `clipboard_backend`'s Wayland code).
//!
//! Neither the X11 nor the Wayland clipboard holds data itself, only the
//! name of the client that set it, which must keep running to answer
//! paste requests. [`hand_over`] therefore starts a copy of clipGPT with
//! the hidden argument and passes the text to set on stdin; that copy
//! ([`run_owner`]) takes ownership, reports back on stdout (`ok`,
//! `unavailable: <message>` or `error: <message>`), and then serves
//! requests until another client takes the clipboard over.
//!
//! Re-executing avoids `fork()`: by the time the clipboard is set,
//! clipGPT has already made the HTTP request, so other threads (reqwest's
//! runtime, any resolver threads) are running, and forking a
//! multi-threaded process risks a child that deadlocks on a lock another
//! thread held at the moment of the fork.

use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long [`hand_over`] waits for the owner helper to report back.
const HELPER_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Largest text [`run_owner`] reads from its parent before refusing:
/// twice [`crate::net::MAX_RESPONSE_BYTES`] (the largest text an LLM
/// reply or [`hand_over`] can ever send; the built-in placeholders are far
/// smaller), so real input is never refused, while piping something
/// unbounded into the hidden mode by hand (deliberately, or by mistake)
/// cannot exhaust memory.
const MAX_OWNER_TEXT_BYTES: usize = 2 * crate::net::MAX_RESPONSE_BYTES;

/// How the owner helper's `take` step (see [`run_owner`]) can fail.
#[derive(Debug, PartialEq, Eq)]
pub enum TakeFailure {
    /// This backend is not available at all (e.g. no Wayland data-control
    /// protocol on this compositor); the parent should try a different
    /// one.
    Unavailable(String),
    /// The backend was reached but taking the clipboard failed; shown
    /// as-is, final.
    Failed(String),
}

/// The outcome [`hand_over`] reports back to its caller.
#[derive(Debug, PartialEq, Eq)]
pub enum HandOverResult {
    /// The helper took the clipboard and is now serving it.
    Ok,
    /// The backend itself is not available ([`TakeFailure::Unavailable`]).
    Unavailable(String),
    /// Any other failure: spawning the helper, its stdio, a timeout, an
    /// unexpected exit, or [`TakeFailure::Failed`].
    Failed(String),
}

/// The command that starts `exe` as the owner helper for `backend_arg`
/// (e.g. `--clipboard-owner=x11`): piped stdin/stdout, stderr discarded,
/// cwd `/` (so it holds no directory busy), in its own process group (so
/// a Ctrl-C in the terminal that started clipGPT does not take the
/// clipboard down with it).
pub fn helper_command(exe: std::path::PathBuf, backend_arg: &str) -> Command {
    let mut command = Command::new(exe);
    command
        .arg(backend_arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .current_dir("/")
        .process_group(0);
    command
}

/// Start the owner helper `command`, feed it `data` and wait (at most
/// [`HELPER_READY_TIMEOUT`]) for its status line on stdout. The helper is
/// killed if it does not answer in time; on [`HandOverResult::Ok`] it is
/// left running.
pub fn hand_over(mut command: Command, data: &[u8]) -> HandOverResult {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return HandOverResult::Failed(e.to_string()),
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return HandOverResult::Failed("helper has no stdin/stdout".to_string());
    };

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    // A helper that dies early makes this fail with a broken pipe; its
    // status line (if any) says more, so that is preferred below.
    let written = stdin.write_all(data);
    drop(stdin);

    let status = match rx.recv_timeout(HELPER_READY_TIMEOUT) {
        Ok(Ok(line)) => line,
        // A real io::Error from a pipe read (as opposed to Ok(0)/EOF when
        // the writer closes) is exceptional; not reproduced in tests.
        Ok(Err(e)) => format!("error: cannot read helper status: {e}"),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return HandOverResult::Failed("helper did not respond in time".to_string());
        }
    };
    match parse_status(&status) {
        HandOverResult::Ok => match written {
            Ok(()) => HandOverResult::Ok,
            Err(e) => HandOverResult::Failed(format!("cannot pass text to helper: {e}")),
        },
        other => {
            let _ = child.wait();
            other
        }
    }
}

/// Interpret the helper's status line: `ok` is success, `unavailable:
/// <msg>` and `error: <msg>` the matching failures, anything else
/// (including nothing, if the helper died) an unexpected exit.
fn parse_status(line: &str) -> HandOverResult {
    let line = line.trim_end();
    if line == "ok" {
        HandOverResult::Ok
    } else if let Some(message) = line.strip_prefix("unavailable: ") {
        HandOverResult::Unavailable(message.to_string())
    } else if let Some(message) = line.strip_prefix("error: ") {
        HandOverResult::Failed(message.to_string())
    } else {
        HandOverResult::Failed("helper exited unexpectedly".to_string())
    }
}

/// The owner helper protocol, with the I/O and the backend-specific parts
/// (`take` the clipboard with the text, then `serve` it) passed in. The
/// status line goes to `output` (one line, so newlines in an error are
/// replaced); `serve` only runs once `ok` was written. Text over
/// [`MAX_OWNER_TEXT_BYTES`] is refused rather than read in full, so an
/// `input` with no end (e.g. `/dev/zero`) cannot exhaust memory. Returns
/// the process exit status: 0 after serving, 1 on any failure.
pub fn run_owner<O>(
    input: impl Read,
    mut output: impl Write,
    take: impl FnOnce(Vec<u8>) -> Result<O, TakeFailure>,
    serve: impl FnOnce(O) -> Result<(), String>,
) -> i32 {
    let mut data = Vec::new();
    let owner = input
        .take(MAX_OWNER_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| TakeFailure::Failed(format!("cannot read text: {e}")))
        .and_then(|_| {
            if data.len() > MAX_OWNER_TEXT_BYTES {
                Err(TakeFailure::Failed(format!(
                    "text from the parent process is over {MAX_OWNER_TEXT_BYTES} bytes; refusing"
                )))
            } else {
                take(data)
            }
        });
    let (prefix, message) = match owner {
        Ok(owner) => {
            if writeln!(output, "ok").and_then(|_| output.flush()).is_err() {
                return 1;
            }
            return match serve(owner) {
                Ok(()) => 0,
                Err(_) => 1,
            };
        }
        Err(TakeFailure::Unavailable(m)) => ("unavailable", m),
        Err(TakeFailure::Failed(m)) => ("error", m),
    };
    let _ = writeln!(output, "{prefix}: {}", message.replace('\n', " "));
    let _ = output.flush();
    1
}

/// [`run_owner`] on the process's real stdin/stdout, refusing to start if
/// stdin is a terminal: this hidden mode is only ever meant to be piped
/// into by [`hand_over`], and a person who runs it by accident typing
/// text in would otherwise see nothing happen (it reads until EOF, i.e.
/// Ctrl-D), which looks like a hang rather than the internal option it
/// is.
pub fn run_owner_on_stdio<O>(
    take: impl FnOnce(Vec<u8>) -> Result<O, TakeFailure>,
    serve: impl FnOnce(O) -> Result<(), String>,
) -> i32 {
    run_owner_on_stdio_if(std::io::stdin().is_terminal(), take, serve)
}

/// [`run_owner_on_stdio`] with the terminal check's result passed in, so
/// both outcomes can be tested without a real terminal.
fn run_owner_on_stdio_if<O>(
    is_terminal: bool,
    take: impl FnOnce(Vec<u8>) -> Result<O, TakeFailure>,
    serve: impl FnOnce(O) -> Result<(), String>,
) -> i32 {
    if is_terminal {
        eprintln!(
            "This is an internal option that clipGPT runs on itself; it is not meant to be run \
             directly."
        );
        return 1;
    }
    run_owner(std::io::stdin(), std::io::stdout(), take, serve)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_command_runs_hidden_mode_from_root() {
        let command = helper_command("/opt/clip_gpt".into(), "--clipboard-owner=x11");
        assert_eq!(command.get_program(), "/opt/clip_gpt");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--clipboard-owner=x11"]);
        assert_eq!(command.get_current_dir(), Some(std::path::Path::new("/")));
    }

    /// Run [`run_owner`] on `input`; returns exit status, output, and what
    /// `take` received (if it ran) and whether `serve` ran.
    fn owner_run(
        input: &[u8],
        take: Result<(), TakeFailure>,
        serve: Result<(), String>,
    ) -> (i32, String, Option<Vec<u8>>, bool) {
        let mut out = Vec::new();
        let mut taken = None;
        let mut served = false;
        let status = run_owner(
            input,
            &mut out,
            |data| {
                taken = Some(data);
                take
            },
            |()| {
                served = true;
                serve
            },
        );
        (status, String::from_utf8(out).unwrap(), taken, served)
    }

    #[test]
    fn run_owner_reports_ok_then_serves() {
        let (status, out, taken, served) = owner_run(b"text", Ok(()), Ok(()));
        assert_eq!((status, out.as_str(), served), (0, "ok\n", true));
        assert_eq!(taken.as_deref(), Some(&b"text"[..]));
        assert_eq!(parse_status(&out), HandOverResult::Ok);
        let (status, _, _, served) = owner_run(b"t", Ok(()), Err("lost".into()));
        assert_eq!((status, served), (1, true));
    }

    #[test]
    fn run_owner_reports_take_failure_on_one_line() {
        let fail = Err(TakeFailure::Failed("no\ndisplay".into()));
        let (status, out, _, served) = owner_run(b"t", fail, Ok(()));
        assert_eq!(
            (status, out.as_str(), served),
            (1, "error: no display\n", false)
        );
        assert_eq!(
            parse_status(&out),
            HandOverResult::Failed("no display".into())
        );
    }

    #[test]
    fn run_owner_reports_unavailable_on_one_line() {
        let fail = Err(TakeFailure::Unavailable("no\nprotocol".into()));
        let (status, out, _, served) = owner_run(b"t", fail, Ok(()));
        assert_eq!(
            (status, out.as_str(), served),
            (1, "unavailable: no protocol\n", false)
        );
        assert_eq!(
            parse_status(&out),
            HandOverResult::Unavailable("no protocol".into())
        );
    }

    #[test]
    fn run_owner_input_and_output_errors() {
        /// Reads and writes that always fail.
        struct Broken;
        impl Read for Broken {
            /// Always fails.
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed"))
            }
        }
        impl Write for Broken {
            /// Always fails.
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed"))
            }
            /// Nothing to flush.
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut out = Vec::new();
        let status = run_owner(
            Broken,
            &mut out,
            |_| -> Result<(), _> { panic!() },
            |_| panic!(),
        );
        assert_eq!(status, 1);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "error: cannot read text: closed\n"
        );
        let status = run_owner(
            &b"t"[..],
            Broken,
            |_| Ok(()),
            |_| panic!("not after failed ok"),
        );
        assert_eq!(status, 1);
    }

    #[test]
    fn run_owner_caps_how_much_text_it_reads() {
        // Right at the limit: read and passed to take whole.
        let ok = vec![b'x'; MAX_OWNER_TEXT_BYTES];
        let mut out = Vec::new();
        let mut taken = None;
        let status = run_owner(
            &ok[..],
            &mut out,
            |data| {
                taken = Some(data.len());
                Ok(())
            },
            |()| Ok(()),
        );
        assert_eq!((status, taken), (0, Some(MAX_OWNER_TEXT_BYTES)));

        // One byte over: refused, never reaches take, and does not have
        // to read a limitless input in full to notice (an endless `Read`
        // one byte over the limit still terminates this call).
        let over =
            std::iter::repeat_n(b'x', MAX_OWNER_TEXT_BYTES + 1).chain(std::iter::repeat(b'x'));
        let mut out = Vec::new();
        let status = run_owner(
            IterReader(over),
            &mut out,
            |_| -> Result<(), TakeFailure> { panic!("must not be called") },
            |()| panic!("must not be called"),
        );
        assert_eq!(status, 1);
        assert!(String::from_utf8(out)
            .unwrap()
            .starts_with("error: text from the parent process"),);
    }

    /// An endless [`Read`] yielding the bytes of `I`, one at a time.
    struct IterReader<I>(I);

    impl<I: Iterator<Item = u8>> Read for IterReader<I> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            match self.0.next() {
                Some(b) => {
                    buf[0] = b;
                    Ok(1)
                }
                None => Ok(0),
            }
        }
    }

    #[test]
    fn run_owner_on_stdio_refuses_a_terminal() {
        // The terminal check itself is a one-line call to a standard
        // library trait with no fake terminal to point it at here, so
        // run_owner_on_stdio_if takes the result in, which lets both
        // outcomes be tested without one.
        let status = run_owner_on_stdio_if(
            true,
            |_| -> Result<(), TakeFailure> { panic!("must not be called") },
            |()| panic!("must not be called"),
        );
        assert_eq!(status, 1);
    }

    #[test]
    fn run_owner_on_stdio_runs_the_protocol_when_not_a_terminal() {
        // Exercises the real entry point end to end: whatever this
        // process's stdin is (not a terminal, since the test above would
        // otherwise fail), reading it must not hang, and take/serve must
        // run on the real std::io::stdin()/stdout().
        assert!(!std::io::stdin().is_terminal());
        let mut taken = false;
        let mut served = false;
        let status = run_owner_on_stdio_if(
            false,
            |data| {
                taken = true;
                assert!(data.is_empty(), "nothing is piped into the test binary");
                Ok(())
            },
            |()| {
                served = true;
                Ok(())
            },
        );
        assert_eq!((status, taken, served), (0, true, true));
        // The real public entry point, which does its own IsTerminal
        // check instead of taking one in: same outcome, since this
        // process's stdin genuinely is not a terminal here.
        let status = run_owner_on_stdio(Ok, |_| Ok(()));
        assert_eq!(status, 0);
    }

    #[test]
    fn status_line_parsing() {
        assert_eq!(parse_status("ok\n"), HandOverResult::Ok);
        assert_eq!(
            parse_status("error: no display\n"),
            HandOverResult::Failed("no display".into())
        );
        assert_eq!(
            parse_status("unavailable: no protocol\n"),
            HandOverResult::Unavailable("no protocol".into())
        );
        assert_eq!(
            parse_status(""),
            HandOverResult::Failed("helper exited unexpectedly".into())
        );
        assert_eq!(
            parse_status("garbage"),
            HandOverResult::Failed("helper exited unexpectedly".into())
        );
    }

    /// A shell command standing in for the owner helper, for testing
    /// [`hand_over`] without a display of any kind.
    fn fake_helper(script: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn hand_over_passes_data_and_accepts_ok() {
        // The helper checks it got exactly the text before saying ok.
        let script = r#"[ "$(cat)" = "hi there" ] && echo ok || echo "error: wrong data""#;
        assert_eq!(
            hand_over(fake_helper(script), b"hi there"),
            HandOverResult::Ok
        );
        assert!(matches!(
            hand_over(fake_helper(script), b"other"),
            HandOverResult::Failed(m) if m.contains("wrong data")
        ));
    }

    #[test]
    fn hand_over_reports_a_program_that_cannot_be_spawned() {
        let result = hand_over(Command::new("/no/such/program-xyz"), b"x");
        assert!(matches!(result, HandOverResult::Failed(_)));
    }

    #[test]
    fn hand_over_reports_missing_stdio() {
        // No .stdin(Stdio::piped())/.stdout(Stdio::piped()): hand_over
        // cannot take() them back from the child.
        let result = hand_over(Command::new("true"), b"x");
        assert!(matches!(result, HandOverResult::Failed(m) if m.contains("stdin/stdout")));
    }

    #[test]
    fn hand_over_reports_a_write_that_fails() {
        // Closes its own stdin right away, then waits: large enough data
        // that hand_over cannot write it all in one go before that
        // happens, so the write itself fails (broken pipe) rather than
        // just never being read.
        let result = hand_over(fake_helper("exec 0<&-; echo ok"), &vec![b'x'; 4 << 20]);
        assert!(
            matches!(&result, HandOverResult::Failed(m) if m.contains("cannot pass text")),
            "{result:?}"
        );
    }

    #[test]
    fn hand_over_reports_unavailable() {
        let result = hand_over(fake_helper("echo 'unavailable: no protocol'"), b"x");
        assert_eq!(result, HandOverResult::Unavailable("no protocol".into()));
    }

    #[test]
    fn hand_over_reports_helper_death() {
        let result = hand_over(fake_helper("exit 3"), b"x");
        assert!(matches!(result, HandOverResult::Failed(m) if m.contains("unexpectedly")));
    }

    #[test]
    fn hand_over_times_out_silent_helper() {
        let start = std::time::Instant::now();
        let result = hand_over(fake_helper("cat >/dev/null; sleep 30"), b"x");
        assert!(matches!(result, HandOverResult::Failed(m) if m.contains("in time")));
        assert!(start.elapsed() < Duration::from_secs(20));
    }
}
