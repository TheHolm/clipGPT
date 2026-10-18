use pretty_ini::{ini, ini_file};

use std::env;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const APP_NAME: &str = "clipgpt";

const SECTION: &str = "openai";

const DEFAULT_API_TYPE: &str = "OpenAI";
const DEFAULT_API_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_PROMPT_TEXT: &str =
    "Check spelling and gramma and rewrite following text on pirate speak. Be mean and pushy \n\n";
const DEFAULT_PROMPT_IMAGE: &str = "OCR attached image and return text formatted as markdown";
const DEFAULT_BEEP_TONE: u32 = 1046;
/// Error tone, also used when the config itself could not be loaded.
pub const DEFAULT_ERROR_TONE: u32 = 131;
/// Beep duration, also used when the config itself could not be loaded.
pub const DEFAULT_BEEP_DURATION: u32 = 250;
const DEFAULT_MAX_INPUT_BYTES: usize = 64 * 1024;
const DEFAULT_ALLOW_REDIRECTS: bool = false;
const DEFAULT_SECRET_CHECK: bool = true;
/// Default for `request_timeout`, in seconds (the same as reqwest's own
/// default, which applied before the option existed).
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiType {
    OpenAI,
}

impl ApiType {
    /// Parse the `api_type` config value (case-insensitive).
    fn parse(value: &str) -> Result<Self, String> {
        if value.eq_ignore_ascii_case("openai") {
            Ok(ApiType::OpenAI)
        } else {
            Err(format!(
                "Unsupported api_type '{}': only 'OpenAI' is supported for now",
                value
            ))
        }
    }
}

// api_type and prompt_image are not used by main.rs yet (there is only one
// API type, and image prompts aren't implemented) - kept here as reserved
// config surface for when those land.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Config {
    pub api_type: ApiType,
    pub api_url: String,
    /// `None` when the key is missing, empty or whitespace-only (e.g. a
    /// local Ollama server needs no key); no Authorization header is sent
    /// then.
    pub api_key: Option<String>,
    pub model: String,
    pub prompt_text: String,
    pub prompt_image: String,
    pub beep_tone: u32,
    pub error_tone: u32,
    pub beep_duration: u32,
    /// Maximum clipboard text size in bytes (UTF-8) that will be sent to
    /// the LLM. Larger clipboards are refused.
    pub max_input_bytes: usize,
    /// Whether HTTP redirects from `api_url` are followed.
    pub allow_redirects: bool,
    /// Whether clipboard text that looks like a secret (API key, token,
    /// private key, URL with password) is refused. The `-s` command line
    /// option turns it off for one run regardless of this value.
    pub secret_check: bool,
    /// Longest wait for the whole LLM request, from sending it to the end
    /// of the response. `None` means no limit (`request_timeout=0`). The
    /// `--timeout` command line option overrides it for one run.
    pub request_timeout: Option<Duration>,
}

/// Default per-OS location of config.ini:
/// * Linux   "~/.config/clipgpt/config.ini"
/// * MacOS   "~/Library/Application Support/clipgpt/config.ini"
/// * Windows "%APPDATA%\clipgpt\config.ini"
fn ini_path() -> Result<String, String> {
    if cfg!(target_os = "windows") {
        if let Some(app_dir) = env::var_os("APPDATA") {
            let app_dir = app_dir.to_string_lossy();
            Ok(format!("{}\\{}\\config.ini", app_dir, APP_NAME))
        } else {
            Err("APPDATA environment variable is not set. And we are under Windows".to_string())
        }
    } else if let Some(home_dir) = env::var_os("HOME") {
        let home_dir = home_dir.to_string_lossy();
        if cfg!(target_os = "macos") {
            Ok(format!(
                "{}/Library/Application Support/{}/config.ini",
                home_dir, APP_NAME
            ))
        } else {
            // treating all other OSes as linux
            Ok(format!("{}/.config/{}/config.ini", home_dir, APP_NAME))
        }
    } else {
        Err("HOME environment variable is not set. And we are not under Windows".to_string())
    }
}

/// Resolve which config.ini to actually load: the per-OS default location,
/// falling back to ./config.ini in the current directory.
fn resolve_config_path() -> Result<String, String> {
    if let Ok(preferred) = ini_path() {
        if Path::new(&preferred).exists() {
            return Ok(preferred);
        }
    }

    if Path::new("config.ini").exists() {
        return Ok("config.ini".to_string());
    }

    let preferred = ini_path().unwrap_or_else(|_| "<unknown>".to_string());
    Err(format!(
        "No config.ini found. Checked '{}' and './config.ini'. Copy config.ini.example to one of those locations and fill in model (and api_key if your server needs one).",
        preferred
    ))
}

/// Return the raw value of `key` in `table`, if present.
fn get_value(ini: &ini::Ini, table: &str, key: &str) -> Option<String> {
    ini.get(table, key).ok().map(|variable| variable.value)
}

/// Return `key` parsed as u32, falling back to `default` when it is
/// missing or not a valid number.
fn get_u32(ini: &ini::Ini, table: &str, key: &str, default: u32) -> u32 {
    get_value(ini, table, key)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

/// Parse a boolean config value. Accepts true/false, yes/no, 1/0 and
/// on/off, case-insensitive. Anything else is an error, so a typo can never
/// silently enable a security-relevant option.
fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        _ => Err(format!(
            "Invalid value '{}' for '{}': expected true/false, yes/no, 1/0 or on/off",
            value, key
        )),
    }
}

/// Return `key` as a boolean; a missing or empty value means `default`,
/// an unrecognised value is an error (see [`parse_bool`]).
fn get_bool(ini: &ini::Ini, table: &str, key: &str, default: bool) -> Result<bool, String> {
    match get_value(ini, table, key) {
        Some(value) if !value.trim().is_empty() => parse_bool(key, &value),
        _ => Ok(default),
    }
}

/// Parse a size limit: a positive whole number of bytes. Zero and
/// non-numbers are errors rather than silently disabling the limit.
fn parse_positive_usize(key: &str, value: &str) -> Result<usize, String> {
    match value.trim().parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "Invalid value '{}' for '{}': expected a positive number of bytes",
            value, key
        )),
    }
}

/// Return `key` as a positive size; a missing or empty value means
/// `default`.
fn get_size(ini: &ini::Ini, table: &str, key: &str, default: usize) -> Result<usize, String> {
    match get_value(ini, table, key) {
        Some(value) if !value.trim().is_empty() => parse_positive_usize(key, &value),
        _ => Ok(default),
    }
}

/// Parse a timeout given as a whole number of seconds; `0` means no limit
/// (`None`). Anything else (negative, fractional, text) is an error naming
/// `key`. Shared by the config key and the command line option.
pub fn parse_timeout(key: &str, value: &str) -> Result<Option<Duration>, String> {
    match value.trim().parse::<u64>() {
        Ok(0) => Ok(None),
        Ok(secs) => Ok(Some(Duration::from_secs(secs))),
        Err(_) => Err(format!(
            "Invalid value '{}' for '{}': expected a whole number of seconds (0 = no limit)",
            value, key
        )),
    }
}

/// Return `key` as a timeout (see [`parse_timeout`]); a missing or empty
/// value means `default_secs`.
fn get_timeout(
    ini: &ini::Ini,
    table: &str,
    key: &str,
    default_secs: u64,
) -> Result<Option<Duration>, String> {
    match get_value(ini, table, key) {
        Some(value) if !value.trim().is_empty() => parse_timeout(key, &value),
        _ => Ok(Some(Duration::from_secs(default_secs))),
    }
}

/// Treat a missing, empty or whitespace-only api_key as "no key".
fn normalize_api_key(value: Option<String>) -> Option<String> {
    value
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

/// Refuse a config file holding an API key when its Unix permission bits
/// give group or others any access (anything wider than 0600).
#[cfg_attr(not(any(unix, test)), allow(dead_code))]
fn check_key_file_mode(mode: u32, path: &str) -> Result<(), String> {
    if mode & 0o077 != 0 {
        Err(format!(
            "{} contains an api_key but is accessible by other users (mode {:o}). Run: chmod 600 '{}'",
            path,
            mode & 0o777,
            path
        ))
    } else {
        Ok(())
    }
}

/// pretty_ini does not interpret escape sequences and trims whitespace off
/// values, so a literal "\n" typed in config.ini stays as a two-character
/// backslash-n instead of becoming a real newline. Turn those into real
/// newlines so prompt text written in the ini file behaves like the
/// Rust-literal defaults (which already contain real newlines).
fn unescape_newlines(value: String) -> String {
    value.replace("\\n", "\n")
}

/// Locate and load config.ini (see [`resolve_config_path`]).
pub fn read_ini() -> Result<Config, String> {
    let path = resolve_config_path()?;
    load_from_path(&path)
}

/// Read and parse the config file at `path`.
///
/// The file is read here rather than by pretty_ini because pretty_ini
/// panics on unreadable or non-UTF-8 files. On Unix the permission check
/// uses the metadata of the same open file handle that is read.
fn load_from_path(path: &str) -> Result<Config, String> {
    let mut handle =
        File::open(path).map_err(|e| format!("Cannot open config file {}: {}", path, e))?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        handle
            .metadata()
            .map_err(|e| format!("Cannot read metadata of {}: {}", path, e))?
            .permissions()
            .mode()
    };
    let mut contents = String::new();
    handle
        .read_to_string(&mut contents)
        .map_err(|e| format!("Cannot read config file {}: {}", path, e))?;

    let mut file = ini_file::IniFile::default();
    file.set_path(path);
    file.set_buffer(contents.lines().map(str::to_string).collect());

    let mut ini = ini::Ini::default();
    ini.load(&mut file)?;

    let api_key = normalize_api_key(get_value(&ini, SECTION, "api_key"));

    #[cfg(unix)]
    if api_key.is_some() {
        check_key_file_mode(mode, path)?;
    }

    let model = get_value(&ini, SECTION, "model")
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| format!("'model' is required in [{}] section of {}", SECTION, path))?;

    let api_type_str =
        get_value(&ini, SECTION, "api_type").unwrap_or_else(|| DEFAULT_API_TYPE.to_string());
    let api_type = ApiType::parse(&api_type_str)?;

    let api_url =
        get_value(&ini, SECTION, "api_url").unwrap_or_else(|| DEFAULT_API_URL.to_string());

    let prompt_text = unescape_newlines(
        get_value(&ini, SECTION, "prompt_text").unwrap_or_else(|| DEFAULT_PROMPT_TEXT.to_string()),
    );

    let prompt_image = unescape_newlines(
        get_value(&ini, SECTION, "prompt_image")
            .unwrap_or_else(|| DEFAULT_PROMPT_IMAGE.to_string()),
    );

    let beep_tone = get_u32(&ini, SECTION, "beep_tone", DEFAULT_BEEP_TONE);
    let error_tone = get_u32(&ini, SECTION, "error_tone", DEFAULT_ERROR_TONE);
    let beep_duration = get_u32(&ini, SECTION, "beep_duration", DEFAULT_BEEP_DURATION);
    let max_input_bytes = get_size(&ini, SECTION, "max_input_bytes", DEFAULT_MAX_INPUT_BYTES)?;
    let allow_redirects = get_bool(&ini, SECTION, "allow_redirects", DEFAULT_ALLOW_REDIRECTS)?;
    let secret_check = get_bool(&ini, SECTION, "secret_check", DEFAULT_SECRET_CHECK)?;
    let request_timeout = get_timeout(
        &ini,
        SECTION,
        "request_timeout",
        DEFAULT_REQUEST_TIMEOUT_SECS,
    )?;

    Ok(Config {
        api_type,
        api_url,
        api_key,
        model,
        prompt_text,
        prompt_image,
        beep_tone,
        error_tone,
        beep_duration,
        max_input_bytes,
        allow_redirects,
        secret_check,
        request_timeout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `contents` to a fresh file in the temp dir with `mode` (Unix
    /// only) and return its path.
    fn temp_config(name: &str, contents: &str, mode: u32) -> String {
        let path =
            env::temp_dir().join(format!("clipgpt-test-{}-{}.ini", std::process::id(), name));
        let mut file = File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn parse_bool_accepts_known_spellings() {
        for v in ["true", "TRUE", "yes", "1", "On", " true "] {
            assert_eq!(parse_bool("k", v), Ok(true), "{v}");
        }
        for v in ["false", "No", "0", "off"] {
            assert_eq!(parse_bool("k", v), Ok(false), "{v}");
        }
    }

    #[test]
    fn parse_bool_rejects_typos() {
        let err = parse_bool("allow_redirects", "ture").unwrap_err();
        assert!(err.contains("allow_redirects"));
        assert!(parse_bool("k", "2").is_err());
    }

    #[test]
    fn parse_positive_usize_rejects_zero_and_garbage() {
        assert_eq!(parse_positive_usize("k", "4096"), Ok(4096));
        assert!(parse_positive_usize("k", "0").is_err());
        assert!(parse_positive_usize("k", "-1").is_err());
        assert!(parse_positive_usize("k", "64k").is_err());
    }

    #[test]
    fn normalize_api_key_treats_blank_as_none() {
        assert_eq!(normalize_api_key(None), None);
        assert_eq!(normalize_api_key(Some(String::new())), None);
        assert_eq!(normalize_api_key(Some("   ".into())), None);
        assert_eq!(
            normalize_api_key(Some(" sk-x ".into())),
            Some("sk-x".into())
        );
    }

    #[test]
    fn key_file_mode_check() {
        assert!(check_key_file_mode(0o100600, "f").is_ok());
        assert!(check_key_file_mode(0o100400, "f").is_ok());
        assert!(check_key_file_mode(0o100640, "f").is_err());
        assert!(check_key_file_mode(0o100604, "f").is_err());
        assert!(check_key_file_mode(0o100660, "f").is_err());
        let err = check_key_file_mode(0o100644, "/x/config.ini").unwrap_err();
        assert!(err.contains("chmod 600 '/x/config.ini'"));
        assert!(err.contains("644"));
    }

    #[test]
    fn loads_defaults_without_key() {
        let path = temp_config("nokey", "[openai]\nmodel=llama3\n", 0o644);
        let cfg = load_from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.model, "llama3");
        assert_eq!(cfg.api_url, DEFAULT_API_URL);
        assert_eq!(cfg.max_input_bytes, DEFAULT_MAX_INPUT_BYTES);
        assert!(!cfg.allow_redirects);
        assert!(cfg.secret_check);
        assert_eq!(cfg.error_tone, DEFAULT_ERROR_TONE);
        assert_eq!(cfg.request_timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_timeout_values() {
        assert_eq!(parse_timeout("k", "0"), Ok(None));
        assert_eq!(
            parse_timeout("k", " 120 "),
            Ok(Some(Duration::from_secs(120)))
        );
        for bad in ["-1", "1.5", "ten", "", "30s"] {
            let err = parse_timeout("request_timeout", bad).unwrap_err();
            assert!(
                err.contains("request_timeout") && err.contains("0 = no limit"),
                "{err}"
            );
        }
    }

    #[test]
    fn request_timeout_from_file() {
        let read = |name: &str, line: &str| {
            let path = temp_config(name, &format!("[openai]\nmodel=m\n{line}\n"), 0o600);
            let cfg = load_from_path(&path);
            std::fs::remove_file(&path).unwrap();
            cfg.map(|c| c.request_timeout)
        };
        assert_eq!(
            read("to300", "request_timeout=300"),
            Ok(Some(Duration::from_secs(300)))
        );
        assert_eq!(read("to0", "request_timeout=0"), Ok(None));
        assert_eq!(
            read("toempty", "request_timeout="),
            Ok(Some(Duration::from_secs(30)))
        );
        assert!(read("tobad", "request_timeout=forever")
            .unwrap_err()
            .contains("request_timeout"));
    }

    #[test]
    fn empty_key_is_no_key_and_skips_mode_check() {
        let path = temp_config("emptykey", "[openai]\napi_key=\nmodel=m\n", 0o644);
        let cfg = load_from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cfg.api_key, None);
    }

    #[cfg(unix)]
    #[test]
    fn key_in_world_readable_file_is_refused() {
        let path = temp_config("openkey", "[openai]\napi_key=sk-abc\nmodel=m\n", 0o644);
        let err = load_from_path(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(err.contains("chmod 600"));
        assert!(!err.contains("sk-abc"));
    }

    #[test]
    fn key_in_private_file_is_loaded() {
        let path = temp_config("privkey", "[openai]\napi_key=sk-abc\nmodel=m\n", 0o600);
        let cfg = load_from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some("sk-abc"));
    }

    #[test]
    fn new_keys_are_parsed() {
        let path = temp_config(
            "newkeys",
            "[openai]\nmodel=m\nallow_redirects=yes\nmax_input_bytes=1000\nsecret_check=off\n",
            0o600,
        );
        let cfg = load_from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(cfg.allow_redirects);
        assert_eq!(cfg.max_input_bytes, 1000);
        assert!(!cfg.secret_check);
    }

    #[test]
    fn invalid_secret_check_is_an_error() {
        let path = temp_config("badsecret", "[openai]\nmodel=m\nsecret_check=nah\n", 0o600);
        let err = load_from_path(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(err.contains("secret_check"));
    }

    #[test]
    fn invalid_allow_redirects_is_an_error() {
        let path = temp_config(
            "badbool",
            "[openai]\nmodel=m\nallow_redirects=maybe\n",
            0o600,
        );
        let err = load_from_path(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(err.contains("allow_redirects"));
    }

    #[test]
    fn missing_model_is_an_error() {
        let path = temp_config("nomodel", "[openai]\napi_url=http://127.0.0.1/\n", 0o600);
        let err = load_from_path(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(err.contains("model"));
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        assert!(load_from_path("/nonexistent/clipgpt/config.ini").is_err());
    }

    #[test]
    fn prompt_newlines_are_unescaped() {
        assert_eq!(unescape_newlines("a\\nb".into()), "a\nb");
    }
}
