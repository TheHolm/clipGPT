//! Command-line options. Hand-rolled on `std::env::args` - there are only
//! a couple of flags, not worth a parser dependency.

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
  -h, --help             Print this help and exit";

/// Parsed command-line options.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Skip secret-pattern detection for this run.
    pub no_secret_check: bool,
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Normal run with these options.
    Run(Options),
    /// Print [`USAGE`] and exit successfully.
    Help,
}

/// Parse the arguments after the program name. Unknown arguments are an
/// error naming the argument, so a mistyped flag never silently does
/// nothing.
pub fn parse<I, S>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut options = Options::default();
    for arg in args {
        match arg.as_ref() {
            "-h" | "--help" => return Ok(Command::Help),
            "-s" | "--no-secret-check" => options.no_secret_check = true,
            other => return Err(format!("Unknown option '{}'\n\n{}", other, USAGE)),
        }
    }
    Ok(Command::Run(options))
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

    #[test]
    fn usage_mentions_all_flags() {
        for flag in ["-s", "--no-secret-check", "-h", "--help"] {
            assert!(USAGE.contains(flag), "{flag}");
        }
    }
}
