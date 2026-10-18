//! Command-line options. Hand-rolled on `std::env::args` - there are only
//! a couple of flags, not worth a parser dependency.

use crate::configfile::parse_timeout;
use std::time::Duration;

/// Usage text printed by `-h`/`--help` and on unknown options.
pub const USAGE: &str = "\
Usage: clip_gpt [OPTIONS]

Sends the clipboard text to the LLM configured in config.ini and replaces
the clipboard with the response.

Options:
  -s, --no-secret-check  Do not refuse clipboard text that looks like an API
                         key, token, private key or URL with a password
                         (password-manager and hidden-character checks
                         still apply)
      --timeout SECONDS  Wait at most SECONDS for the LLM's answer (0 = no
                         limit); overrides request_timeout in config.ini
  -h, --help             Print this help and exit";

/// Hidden argument that runs clipGPT as the background X11 clipboard owner
/// (see `clipboard_backend`'s `x11` module). Deliberately not in [`USAGE`]:
/// it is only meant to be passed by clipGPT itself.
pub const OWNER_HELPER_ARG: &str = "--x11-clipboard-owner";

/// Parsed command-line options.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Skip secret-pattern detection for this run.
    pub no_secret_check: bool,
    /// `--timeout`: `None` if not given (use the config value),
    /// otherwise the timeout for this run (`Some(None)` = no limit).
    pub timeout: Option<Option<Duration>>,
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Normal run with these options.
    Run(Options),
    /// Print [`USAGE`] and exit successfully.
    Help,
    /// Serve the X11 clipboard ([`OWNER_HELPER_ARG`], must be the only
    /// argument).
    ClipboardOwner,
}

/// Parse the arguments after the program name. Unknown arguments are an
/// error naming the argument, so a mistyped flag never silently does
/// nothing. [`OWNER_HELPER_ARG`] is only recognised on its own.
pub fn parse<I, S>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<S> = args.into_iter().collect();
    if let [only] = args.as_slice() {
        if only.as_ref() == OWNER_HELPER_ARG {
            return Ok(Command::ClipboardOwner);
        }
    }
    let mut options = Options::default();
    let mut args = args.iter().map(AsRef::as_ref);
    while let Some(arg) = args.next() {
        match arg {
            "-h" | "--help" => return Ok(Command::Help),
            "-s" | "--no-secret-check" => options.no_secret_check = true,
            "--timeout" => {
                let value = args.next().ok_or_else(|| {
                    format!("Option '{}' needs a number of seconds\n\n{}", arg, USAGE)
                })?;
                options.timeout = Some(timeout_value(arg, value)?);
            }
            other => match other.strip_prefix("--timeout=") {
                Some(value) => options.timeout = Some(timeout_value("--timeout", value)?),
                None => return Err(format!("Unknown option '{}'\n\n{}", other, USAGE)),
            },
        }
    }
    Ok(Command::Run(options))
}

/// Parse the value of the timeout option `option` (see
/// [`parse_timeout`]), adding the usage text to an error.
fn timeout_value(option: &str, value: &str) -> Result<Option<Duration>, String> {
    parse_timeout(option, value).map_err(|e| format!("{}\n\n{}", e, USAGE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_runs_with_defaults() {
        assert_eq!(
            parse(Vec::<String>::new()),
            Ok(Command::Run(Options::default()))
        );
    }

    #[test]
    fn secret_check_flags() {
        let expected = Ok(Command::Run(Options {
            no_secret_check: true,
            timeout: None,
        }));
        assert_eq!(parse(["-s"]), expected);
        assert_eq!(parse(["--no-secret-check"]), expected);
        assert_eq!(parse(["-s", "-s"]), expected);
    }

    #[test]
    fn help_flags() {
        assert_eq!(parse(["-h"]), Ok(Command::Help));
        assert_eq!(parse(["--help"]), Ok(Command::Help));
        assert_eq!(parse(["-s", "--help"]), Ok(Command::Help));
    }

    #[test]
    fn unknown_option_is_error_with_usage() {
        let err = parse(["--no-secret-chek"]).unwrap_err();
        assert!(err.contains("'--no-secret-chek'"));
        assert!(err.contains("Usage:"));
        assert!(parse(["-x"]).is_err());
        assert!(parse(["extra"]).is_err());
    }

    /// The options parsed from `args`, which must be a normal run.
    fn run_options(args: &[&str]) -> Options {
        match parse(args) {
            Ok(Command::Run(options)) => options,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    #[test]
    fn timeout_option_forms() {
        let two_min = Some(Some(Duration::from_secs(120)));
        assert_eq!(run_options(&["--timeout", "120"]).timeout, two_min);
        assert_eq!(run_options(&["--timeout=120"]).timeout, two_min);
        assert_eq!(run_options(&["--timeout", "0"]).timeout, Some(None));
        assert_eq!(run_options(&[]).timeout, None);
        // Last one wins; combines with other flags.
        let opts = run_options(&["--timeout", "5", "-s", "--timeout=7"]);
        assert_eq!(opts.timeout, Some(Some(Duration::from_secs(7))));
        assert!(opts.no_secret_check);
    }

    #[test]
    fn timeout_option_errors() {
        let err = parse(["--timeout"]).unwrap_err();
        assert!(err.contains("'--timeout' needs a number of seconds") && err.contains("Usage:"));
        // There is no short form.
        assert!(parse(["-T", "5"])
            .unwrap_err()
            .contains("Unknown option '-T'"));
        let err = parse(["--timeout", "soon"]).unwrap_err();
        assert!(
            err.contains("'soon'") && err.contains("'--timeout'"),
            "{err}"
        );
        assert!(err.contains("Usage:"));
        assert!(parse(["--timeout=-1"]).is_err());
        assert!(parse(["--timeout="]).is_err());
        // The value is taken as-is, even if it looks like a flag.
        assert!(parse(["--timeout", "-s"]).is_err());
    }

    #[test]
    fn owner_helper_only_alone_and_hidden() {
        assert_eq!(parse([OWNER_HELPER_ARG]), Ok(Command::ClipboardOwner));
        assert!(parse(["-s", OWNER_HELPER_ARG]).is_err());
        assert!(parse([OWNER_HELPER_ARG, "-s"]).is_err());
        assert!(!USAGE.contains(OWNER_HELPER_ARG));
    }

    #[test]
    fn usage_mentions_all_flags() {
        for flag in ["-s", "--no-secret-check", "--timeout", "-h", "--help"] {
            assert!(USAGE.contains(flag), "{flag}");
        }
    }
}
