//! Command-line options. Hand-rolled on `std::env::args_os` - there are
//! only a few flags, not worth a parser dependency. Arguments are taken as
//! `OsStr` so a `-c` path need not be valid UTF-8; any other argument that
//! is not is an unknown option.

use crate::configfile::parse_timeout;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

/// Usage text printed by `-h`/`--help` and on unknown options.
pub const USAGE: &str = "\
Usage: clip_gpt [OPTIONS]

Sends the clipboard text (or, if there is none, the clipboard image) to the
LLM configured in config.toml and replaces the clipboard with the response.

Options:
  -c, --config FILE      Read this config file instead of searching
                         ~/.config/clipgpt/config.toml and ./config.toml
  -p, --profile NAME[,NAME...]
                         Use these profiles of config.toml instead of
                         [default]. Each kind of clipboard content (text,
                         image) goes to the last listed profile whose input
                         accepts it
      --list-profiles    Print the names of all profiles and exit
      --show-profile     Print the settings of the selected profiles after
                         includes (and where each came from), then exit
  -s, --no-secret-check  Do not refuse clipboard text that looks like an API
                         key, token, private key or URL with a password
                         (password-manager and hidden-character checks
                         still apply)
      --timeout SECONDS  Wait at most SECONDS (up to 3600) for the LLM's
                         answer (0 = no limit); overrides request_timeout
                         in config.toml
  -V, --version          Print the version and exit
  -h, --help             Print this help and exit";

/// The `-V`/`--version` output. The version comes from `Cargo.toml` at
/// build time, so it always matches the package.
pub fn version_text() -> String {
    format!("clip_gpt version {}", env!("CARGO_PKG_VERSION"))
}

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
    /// `-p`/`--profile`: the profiles to use, in order; `None` means the
    /// default profile.
    pub profiles: Option<Vec<String>>,
    /// `-c`/`--config`: the config file to read; `None` means search the
    /// usual places.
    pub config: Option<PathBuf>,
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Normal run with these options.
    Run(Options),
    /// `--show-profile`: print the selected profiles' settings and exit.
    ShowProfile(Options),
    /// `--list-profiles`: print the profile names and exit (the options
    /// only matter for `-c`).
    ListProfiles(Options),
    /// Print [`USAGE`] and exit successfully.
    Help,
    /// Print [`version_text`] and exit successfully.
    Version,
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
    S: AsRef<OsStr>,
{
    let args: Vec<S> = args.into_iter().collect();
    if let [only] = args.as_slice() {
        if only.as_ref() == OWNER_HELPER_ARG {
            return Ok(Command::ClipboardOwner);
        }
    }
    let mut options = Options::default();
    let (mut list, mut show) = (false, false);
    let mut raw = args.iter().map(AsRef::as_ref);
    while let Some(raw_arg) = raw.next() {
        let Some(arg) = raw_arg.to_str() else {
            return Err(unknown(raw_arg));
        };
        if matches!(arg, "-c" | "--config") {
            let value = raw
                .next()
                .ok_or_else(|| format!("Option '{arg}' needs a file name\n\n{USAGE}"))?;
            set_config(&mut options, arg, value)?;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--config=") {
            set_config(&mut options, "--config", OsStr::new(value))?;
            continue;
        }
        let mut args = raw.by_ref().map(|a| a.to_str().ok_or_else(|| unknown(a)));
        match arg {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "-s" | "--no-secret-check" => options.no_secret_check = true,
            "--list-profiles" => list = true,
            "--show-profile" => show = true,
            "--timeout" => {
                let value = args.next().transpose()?.ok_or_else(|| {
                    format!("Option '{}' needs a number of seconds\n\n{}", arg, USAGE)
                })?;
                options.timeout = Some(timeout_value(arg, value)?);
            }
            "-p" | "--profile" => {
                let value = args
                    .next()
                    .transpose()?
                    .ok_or_else(|| format!("Option '{}' needs a profile name\n\n{}", arg, USAGE))?;
                set_profiles(&mut options, arg, value)?;
            }
            other => match other.strip_prefix("--timeout=") {
                Some(value) => options.timeout = Some(timeout_value("--timeout", value)?),
                None => match other.strip_prefix("--profile=") {
                    Some(value) => set_profiles(&mut options, "--profile", value)?,
                    None => return Err(unknown(OsStr::new(other))),
                },
            },
        }
    }
    Ok(match (list, show) {
        (true, true) => {
            return Err(format!(
                "--list-profiles and --show-profile cannot be combined\n\n{USAGE}"
            ))
        }
        (true, false) => Command::ListProfiles(options),
        (false, true) => Command::ShowProfile(options),
        (false, false) => Command::Run(options),
    })
}

/// The error for an unknown argument (shown lossily if not UTF-8).
fn unknown(arg: &OsStr) -> String {
    format!("Unknown option '{}'\n\n{USAGE}", arg.to_string_lossy())
}

/// Store the config file `value` of option `option`. Giving the option
/// twice or an empty file name is an error; whether the file exists is
/// checked when it is loaded.
fn set_config(options: &mut Options, option: &str, value: &OsStr) -> Result<(), String> {
    if options.config.is_some() {
        return Err(format!("Option '{option}' given more than once\n\n{USAGE}"));
    }
    if value.is_empty() {
        return Err(format!("Option '{option}' needs a file name\n\n{USAGE}"));
    }
    options.config = Some(PathBuf::from(OsString::from(value)));
    Ok(())
}

/// Store the comma-separated profile list `value` of option `option`.
/// Giving the option twice, an empty name or the same name twice is an
/// error; whether the names exist is checked against the config file.
fn set_profiles(options: &mut Options, option: &str, value: &str) -> Result<(), String> {
    if options.profiles.is_some() {
        return Err(format!(
            "Option '{option}' given more than once; list several profiles separated by commas \
             instead (-p a,b)\n\n{USAGE}"
        ));
    }
    let mut names: Vec<String> = Vec::new();
    for name in value.split(',') {
        if name.is_empty() {
            return Err(format!(
                "Option '{option}' has an empty profile name in '{value}'\n\n{USAGE}"
            ));
        }
        if names.iter().any(|n| n == name) {
            return Err(format!(
                "Option '{option}' lists profile '{name}' twice\n\n{USAGE}"
            ));
        }
        names.push(name.to_string());
    }
    options.profiles = Some(names);
    Ok(())
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
            ..Options::default()
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
    fn version_flags() {
        assert_eq!(parse(["-V"]), Ok(Command::Version));
        assert_eq!(parse(["--version"]), Ok(Command::Version));
        assert_eq!(parse(["-s", "--version"]), Ok(Command::Version));
        assert!(parse(["-v"]).is_err());
    }

    #[test]
    fn version_text_names_the_version() {
        let text = version_text();
        assert_eq!(
            text,
            format!("clip_gpt version {}", env!("CARGO_PKG_VERSION"))
        );
        let version = text.rsplit(' ').next().unwrap();
        assert_eq!(version.split('.').count(), 3, "{version}");
        assert!(!USAGE.contains(version));
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
        let err = parse(["--timeout", "3601"]).unwrap_err();
        assert!(
            err.contains("from 0 to 3600") && err.contains("Usage:"),
            "{err}"
        );
        assert!(parse(["--timeout=3600"]).is_ok());
        assert!(USAGE.contains("up to 3600"));
        assert!(parse(["--timeout="]).is_err());
        // The value is taken as-is, even if it looks like a flag.
        assert!(parse(["--timeout", "-s"]).is_err());
    }

    /// The profile names parsed from `args`.
    fn profiles(args: &[&str]) -> Option<Vec<String>> {
        run_options(args).profiles
    }

    #[test]
    fn profile_option_forms() {
        let ab = Some(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(profiles(&["-p", "a,b"]), ab);
        assert_eq!(profiles(&["--profile", "a,b"]), ab);
        assert_eq!(profiles(&["--profile=a,b"]), ab);
        assert_eq!(profiles(&["-p", "ocr"]), Some(vec!["ocr".to_string()]));
        assert_eq!(profiles(&[]), None);
        let opts = run_options(&["-s", "-p", "x", "--timeout", "5"]);
        assert_eq!(opts.profiles, Some(vec!["x".to_string()]));
        assert!(opts.no_secret_check);
    }

    #[test]
    fn profile_option_errors() {
        let err = parse(["-p"]).unwrap_err();
        assert!(err.contains("'-p' needs a profile name") && err.contains("Usage:"));
        for bad in ["a,,b", ",a", "a,", ""] {
            let err = parse(["-p", bad]).unwrap_err();
            assert!(err.contains("empty profile name"), "{bad}: {err}");
        }
        assert!(parse(["--profile=a,b,a"])
            .unwrap_err()
            .contains("'a' twice"));
        let err = parse(["-p", "a", "-p", "b"]).unwrap_err();
        assert!(
            err.contains("more than once") && err.contains("-p a,b"),
            "{err}"
        );
        assert!(parse(["-p", "a", "--profile=b"]).is_err());
    }

    #[test]
    fn list_and_show_profiles() {
        assert_eq!(
            parse(["--list-profiles"]),
            Ok(Command::ListProfiles(Options::default()))
        );
        assert_eq!(
            parse(["--show-profile"]),
            Ok(Command::ShowProfile(Options::default()))
        );
        assert_eq!(
            parse(["--show-profile", "-p", "a,b"]),
            Ok(Command::ShowProfile(Options {
                profiles: Some(vec!["a".into(), "b".into()]),
                ..Options::default()
            }))
        );
        assert!(parse(["--list-profiles", "--show-profile"])
            .unwrap_err()
            .contains("cannot be combined"));
        assert_eq!(parse(["--list-profiles", "--help"]), Ok(Command::Help));
    }

    /// The config path parsed from `args`.
    fn config(args: &[&str]) -> Option<PathBuf> {
        run_options(args).config
    }

    #[test]
    fn config_option_forms() {
        let want = Some(PathBuf::from("/x/work.toml"));
        assert_eq!(config(&["-c", "/x/work.toml"]), want);
        assert_eq!(config(&["--config", "/x/work.toml"]), want);
        assert_eq!(config(&["--config=/x/work.toml"]), want);
        assert_eq!(config(&["-c", "rel.toml"]), Some(PathBuf::from("rel.toml")));
        // A value that looks like an option is still the file name.
        assert_eq!(config(&["-c", "-p"]), Some(PathBuf::from("-p")));
        assert_eq!(config(&[]), None);
        let opts = run_options(&["-p", "ocr", "-c", "a.toml", "-s"]);
        assert_eq!(opts.profiles, Some(vec!["ocr".to_string()]));
        assert_eq!(opts.config, Some(PathBuf::from("a.toml")));
        assert!(opts.no_secret_check);
    }

    #[test]
    fn config_option_errors() {
        for args in [&["-c"][..], &["--config"], &["--config="], &["-c", ""]] {
            let err = parse(args).unwrap_err();
            assert!(
                err.contains("needs a file name") && err.contains("Usage:"),
                "{args:?}"
            );
        }
        let err = parse(["-c", "a", "--config=b"]).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn config_option_works_with_list_and_show() {
        let with_config = Options {
            config: Some(PathBuf::from("w.toml")),
            ..Options::default()
        };
        assert_eq!(
            parse(["--list-profiles", "-c", "w.toml"]),
            Ok(Command::ListProfiles(with_config))
        );
        assert_eq!(
            parse(["-c", "w.toml", "-p", "a", "--show-profile"]),
            Ok(Command::ShowProfile(Options {
                config: Some(PathBuf::from("w.toml")),
                profiles: Some(vec!["a".into()]),
                ..Options::default()
            }))
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments() {
        use std::os::unix::ffi::OsStrExt;
        let bad = OsStr::from_bytes(b"/tmp/caf\xe9.toml");
        let parsed = parse([OsStr::new("-c"), bad]).unwrap();
        assert_eq!(
            parsed,
            Command::Run(Options {
                config: Some(PathBuf::from(bad)),
                ..Options::default()
            })
        );
        let err = parse([OsStr::from_bytes(b"--x\xff")]).unwrap_err();
        assert!(err.starts_with("Unknown option '--x\u{FFFD}'"), "{err}");
        let err = parse([OsStr::new("-p"), OsStr::from_bytes(b"a\xff")]).unwrap_err();
        assert!(err.contains("Unknown option"), "{err}");
    }

    #[test]
    fn owner_helper_only_alone_and_hidden() {
        assert_eq!(parse([OWNER_HELPER_ARG]), Ok(Command::ClipboardOwner));
        assert!(parse(["-s", OWNER_HELPER_ARG]).is_err());
        assert!(parse([OWNER_HELPER_ARG, "-s"]).is_err());
        assert!(!USAGE.contains(OWNER_HELPER_ARG));
    }

    #[test]
    fn man_page_documents_every_option() {
        let man = include_str!("../man/clip_gpt.1").replace("\\-", "-");
        for flag in ALL_FLAGS {
            let macro_form = format!("Fl {}", &flag[1..]);
            assert!(man.contains(&macro_form), "{flag} missing from man page");
        }
        assert!(
            man.contains(OWNER_HELPER_ARG),
            "internal argument should be named"
        );
        assert!(!man.contains("Fl T"), "-T was removed");
    }

    /// Every documented option.
    const ALL_FLAGS: [&str; 13] = [
        "-c",
        "--config",
        "-p",
        "--profile",
        "--list-profiles",
        "--show-profile",
        "-s",
        "--no-secret-check",
        "--timeout",
        "-V",
        "--version",
        "-h",
        "--help",
    ];

    #[test]
    fn usage_mentions_all_flags() {
        for flag in ALL_FLAGS {
            assert!(USAGE.contains(flag), "{flag}");
        }
    }
}
