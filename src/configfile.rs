//! Loading `config.toml`: named profiles, `include`/`clear`, the `input`
//! kinds each profile handles, and choosing which selected profile handles
//! which kind of clipboard content.
//!
//! Every `[name]` table is a profile. A profile's settings are, in
//! increasing precedence: the built-in defaults, the profiles listed in its
//! `include` (in order, later ones win), then its own keys. `clear` drops
//! keys that came from includes, so the built-in default applies again.
//! The whole file is checked on every run - unknown keys, wrong types,
//! unknown or cyclic includes are hard errors even in profiles not used.
//! Error messages never contain setting values (an API key could be one).

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use toml::{Table, Value};

const APP_NAME: &str = "clipgpt";

/// Name of the config file in each searched directory.
const CONFIG_FILE: &str = "config.toml";

/// Profile used when no `-p` is given.
pub const DEFAULT_PROFILE: &str = "default";

const DEFAULT_API_TYPE: &str = "OpenAI";
const DEFAULT_API_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_PROMPT_TEXT: &str =
    "Check spelling and gramma and rewrite following text on pirate speak. Be mean and pushy \n\n";
/// Default prompt for clipboard images: make a vision model act as OCR.
const DEFAULT_PROMPT_IMAGE: &str = "You are an OCR engine. Transcribe all text visible in the \
     image exactly as written, in its original language. Do not translate, summarise, correct, or \
     explain it. Keep the reading order and reproduce the layout as Markdown: headings as #, lists \
     as - or 1., tables as Markdown tables, code or monospaced text in ``` blocks. Mark unreadable \
     parts as [illegible]. Ignore pictures, icons and other non-text content. If the image \
     contains no text, reply exactly: [no text found]. Output only the transcription, with no \
     introduction or comments.";
const DEFAULT_BEEP_TONE: u32 = 1046;
/// Error tone, also used when the config itself could not be loaded.
pub const DEFAULT_ERROR_TONE: u32 = 131;
/// Beep duration, also used when the config itself could not be loaded.
pub const DEFAULT_BEEP_DURATION: u32 = 250;
const DEFAULT_MAX_INPUT_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_ALLOW_REDIRECTS: bool = false;
const DEFAULT_SECRET_CHECK: bool = true;
/// Default for `request_timeout`, in seconds (the same as reqwest's own
/// default, which applied before the option existed).
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// The `input` value marking a profile that handles nothing (one meant
/// only to be included by others).
const INPUT_NONE: &str = "none";

/// Every key a profile may contain, in the order `--show-profile` prints
/// them.
pub const KEYS: [&str; 17] = [
    "include",
    "clear",
    "input",
    "api_type",
    "api_url",
    "api_key",
    "model",
    "prompt_text",
    "prompt_image",
    "beep_tone",
    "error_tone",
    "beep_duration",
    "max_input_bytes",
    "max_image_bytes",
    "allow_redirects",
    "secret_check",
    "request_timeout",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiType {
    OpenAI,
}

impl ApiType {
    /// Parse the `api_type` config value (case-insensitive).
    fn parse(value: &str) -> Option<Self> {
        value
            .eq_ignore_ascii_case("openai")
            .then_some(ApiType::OpenAI)
    }
}

/// A kind of clipboard content a profile can handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputKind {
    Text,
    Image,
}

impl InputKind {
    /// Every kind, in the order they are listed and routed. A profile
    /// without `input` handles all of them, including kinds added later.
    pub const ALL: [InputKind; 2] = [InputKind::Text, InputKind::Image];

    /// The name used in `input = [...]` and in messages.
    pub fn name(self) -> &'static str {
        match self {
            InputKind::Text => "text",
            InputKind::Image => "image",
        }
    }

    /// The kind called `name`, if any.
    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// The settings of one profile, after includes, ready to run with.
// api_type is not used by main.rs yet (there is only one API type) - kept
// here as reserved config surface for when more land.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Config {
    pub api_type: ApiType,
    pub api_url: String,
    /// `None` when the key is missing, empty or whitespace-only (e.g. a
    /// local Ollama server needs no key); no Authorization header is sent
    /// then.
    pub api_key: Option<String>,
    pub model: String,
    /// System prompt for clipboard text.
    pub prompt_text: String,
    /// System prompt for clipboard images.
    pub prompt_image: String,
    pub beep_tone: u32,
    pub error_tone: u32,
    pub beep_duration: u32,
    /// Maximum clipboard text size in bytes (UTF-8) that will be sent to
    /// the LLM. Larger clipboards are refused.
    pub max_input_bytes: usize,
    /// Maximum clipboard image size in bytes, both as read and after any
    /// conversion to PNG. Larger images are refused.
    pub max_image_bytes: usize,
    /// Whether HTTP redirects from `api_url` are followed.
    pub allow_redirects: bool,
    /// Whether clipboard text that looks like a secret (API key, token,
    /// private key, URL with password) is refused. The `-s` command line
    /// option turns it off for one run regardless of this value.
    pub secret_check: bool,
    /// Longest wait for the whole LLM request, from sending it to the end
    /// of the response. `None` means no limit (`request_timeout = 0`). The
    /// `--timeout` command line option overrides it for one run.
    pub request_timeout: Option<Duration>,
}

/// The type a key's value must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyType {
    /// Any string.
    Str,
    /// A string naming a supported API type.
    ApiType,
    /// An integer from 0 to `u32::MAX`.
    U32,
    /// A positive integer (a size in bytes).
    Size,
    /// `true` or `false`.
    Bool,
    /// A whole number of seconds, 0 = no limit.
    Timeout,
    /// The list of input kinds (`input`).
    Input,
    /// A list of profile names (`include`).
    Profiles,
    /// A list of key names (`clear`).
    Keys,
}

/// The type of `key`'s value, or `None` for an unknown key.
fn key_type(key: &str) -> Option<KeyType> {
    Some(match key {
        "include" => KeyType::Profiles,
        "clear" => KeyType::Keys,
        "input" => KeyType::Input,
        "api_type" => KeyType::ApiType,
        "api_url" | "api_key" | "model" | "prompt_text" | "prompt_image" => KeyType::Str,
        "beep_tone" | "error_tone" | "beep_duration" => KeyType::U32,
        "max_input_bytes" | "max_image_bytes" => KeyType::Size,
        "allow_redirects" | "secret_check" => KeyType::Bool,
        "request_timeout" => KeyType::Timeout,
        _ => return None,
    })
}

/// The built-in default of a setting, as a TOML value. `None` for keys
/// without one (`api_key`, `model`) and for `input`, whose default is
/// "every kind" (see [`InputKind::ALL`]).
fn default_value(key: &str) -> Option<Value> {
    Some(match key {
        "api_type" => Value::from(DEFAULT_API_TYPE),
        "api_url" => Value::from(DEFAULT_API_URL),
        "prompt_text" => Value::from(DEFAULT_PROMPT_TEXT),
        "prompt_image" => Value::from(DEFAULT_PROMPT_IMAGE),
        "beep_tone" => Value::from(i64::from(DEFAULT_BEEP_TONE)),
        "error_tone" => Value::from(i64::from(DEFAULT_ERROR_TONE)),
        "beep_duration" => Value::from(i64::from(DEFAULT_BEEP_DURATION)),
        "max_input_bytes" => Value::from(DEFAULT_MAX_INPUT_BYTES as i64),
        "max_image_bytes" => Value::from(DEFAULT_MAX_IMAGE_BYTES as i64),
        "allow_redirects" => Value::from(DEFAULT_ALLOW_REDIRECTS),
        "secret_check" => Value::from(DEFAULT_SECRET_CHECK),
        "request_timeout" => Value::from(DEFAULT_REQUEST_TIMEOUT_SECS as i64),
        _ => return None,
    })
}

/// Per-OS directory holding config.toml:
/// * Linux   "~/.config/clipgpt"
/// * MacOS   "~/Library/Application Support/clipgpt"
/// * Windows "%APPDATA%\clipgpt"
fn config_dir() -> Result<String, String> {
    if cfg!(target_os = "windows") {
        if let Some(app_dir) = env::var_os("APPDATA") {
            Ok(format!("{}\\{}", app_dir.to_string_lossy(), APP_NAME))
        } else {
            Err("APPDATA environment variable is not set. And we are under Windows".to_string())
        }
    } else if let Some(home_dir) = env::var_os("HOME") {
        let home_dir = home_dir.to_string_lossy();
        if cfg!(target_os = "macos") {
            Ok(format!(
                "{}/Library/Application Support/{}",
                home_dir, APP_NAME
            ))
        } else {
            // treating all other OSes as linux
            Ok(format!("{}/.config/{}", home_dir, APP_NAME))
        }
    } else {
        Err("HOME environment variable is not set. And we are not under Windows".to_string())
    }
}

/// `file` inside `dir`, with the OS's path separator.
fn join(dir: &str, file: &str) -> String {
    let separator = if cfg!(target_os = "windows") {
        '\\'
    } else {
        '/'
    };
    format!("{dir}{separator}{file}")
}

/// Resolve which config.toml to actually load: the one in the per-OS
/// directory, falling back to ./config.toml in the current directory.
fn resolve_config_path() -> Result<String, String> {
    let dir = config_dir();
    if let Ok(dir) = &dir {
        let preferred = join(dir, CONFIG_FILE);
        if Path::new(&preferred).exists() {
            return Ok(preferred);
        }
    }
    if Path::new(CONFIG_FILE).exists() {
        return Ok(CONFIG_FILE.to_string());
    }
    let dir = dir.unwrap_or_else(|_| "<unknown>".to_string());
    let old_ini = Path::new(&join(&dir, "config.ini")).exists() || Path::new("config.ini").exists();
    Err(not_found_message(&join(&dir, CONFIG_FILE), old_ini))
}

/// The error for a missing config.toml; `old_ini` adds a note that the
/// config.ini of clipGPT 0.5 and older is no longer read.
fn not_found_message(preferred: &str, old_ini: bool) -> String {
    let mut message = format!(
        "No {CONFIG_FILE} found. Checked '{preferred}' and './{CONFIG_FILE}'. Copy \
         config.toml.example to one of those locations and fill in model (and api_key if your \
         server needs one)."
    );
    if old_ini {
        message.push_str(
            " A config.ini was found: it is no longer read since clipGPT 0.6 - convert it to \
             config.toml (see the release notes).",
        );
    }
    message
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

/// Parse a timeout given as a whole number of seconds; `0` means no limit
/// (`None`). Anything else (negative, fractional, text) is an error naming
/// `key`. Used by the `--timeout` command line option.
pub fn parse_timeout(key: &str, value: &str) -> Result<Option<Duration>, String> {
    match value.trim().parse::<u64>() {
        Ok(secs) => Ok(timeout_from_secs(secs)),
        Err(_) => Err(format!(
            "Invalid value '{}' for '{}': expected a whole number of seconds (0 = no limit)",
            value, key
        )),
    }
}

/// A timeout of `secs` seconds, `0` meaning no limit.
fn timeout_from_secs(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// One profile as written in the file, its values already type-checked.
#[derive(Debug)]
struct Profile {
    name: String,
    /// Profiles to include, in order (later ones win).
    include: Vec<String>,
    /// Keys whose included values are dropped.
    clear: Vec<String>,
    /// The profile's own settings (everything but `include`/`clear`).
    settings: Vec<(String, Value)>,
}

/// A setting after includes: its value and the profile it came from.
#[derive(Debug, Clone, PartialEq)]
struct Setting {
    value: Value,
    source: String,
}

/// Settings of a profile after includes, by key. Keys not present take
/// their built-in default.
type Resolved = HashMap<String, Setting>;

/// What a profile's `input` says it handles.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Accepts {
    /// These kinds (every kind when `input` is not set).
    Kinds(Vec<InputKind>),
    /// `input = ["none"]`: nothing.
    Nothing,
}

/// All profiles of a config file, checked, in the order of the file.
#[derive(Debug)]
pub struct ConfigFile {
    path: String,
    profiles: Vec<Profile>,
}

/// Locate and load config.toml (see [`resolve_config_path`]).
pub fn load() -> Result<ConfigFile, String> {
    let path = resolve_config_path()?;
    load_from_path(&path)
}

/// Read, parse and check the config file at `path`.
///
/// On Unix the permission check uses the metadata of the same open file
/// handle that is read. It applies when any profile, used or not, has a
/// non-empty `api_key`.
fn load_from_path(path: &str) -> Result<ConfigFile, String> {
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

    let file = parse_file(path, &contents)?;
    #[cfg(unix)]
    if file.has_api_key() {
        check_key_file_mode(mode, path)?;
    }
    Ok(file)
}

/// The 1-based line of byte offset `offset` in `contents`.
fn line_of(contents: &str, offset: usize) -> usize {
    let end = offset.min(contents.len());
    contents.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Whether `name` may be a profile name: TOML bare-key characters only
/// (letters, digits, `_`, `-`), so it can always be given to `-p` in a
/// comma-separated list.
fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Parse and check the contents of a config file. `path` is only used in
/// messages.
fn parse_file(path: &str, contents: &str) -> Result<ConfigFile, String> {
    let table: Table = contents.parse().map_err(|e: toml::de::Error| {
        // Only the message and position: toml's full error text quotes the
        // offending line, which may hold an API key.
        match e.span() {
            Some(span) => format!(
                "{path}: invalid TOML at line {}: {}",
                line_of(contents, span.start),
                e.message()
            ),
            None => format!("{path}: invalid TOML: {}", e.message()),
        }
    })?;
    let mut profiles = Vec::new();
    for (name, value) in table {
        let Value::Table(table) = value else {
            return Err(format!(
                "{path}: '{name}' is outside any [profile] table; every setting must be inside one"
            ));
        };
        if !valid_profile_name(&name) {
            return Err(format!(
                "{path}: invalid profile name [{name}]: use only letters, digits, '_' and '-'"
            ));
        }
        profiles.push(parse_profile(path, name, table)?);
    }
    if profiles.is_empty() {
        return Err(format!("{path} defines no profiles"));
    }
    let file = ConfigFile {
        path: path.to_string(),
        profiles,
    };
    // Every include of every profile must exist and be free of cycles.
    for profile in &file.profiles {
        file.resolve(&profile.name)?;
    }
    Ok(file)
}

/// Check one profile's keys and value types.
fn parse_profile(path: &str, name: String, table: Table) -> Result<Profile, String> {
    let at = |key: &str| format!("{path}: profile [{name}], '{key}'");
    let mut include = Vec::new();
    let mut clear = Vec::new();
    let mut settings = Vec::new();
    for (key, value) in table {
        let Some(kind) = key_type(&key) else {
            return Err(format!(
                "{path}: unknown key '{key}' in profile [{name}] (keys are case-sensitive; valid \
                 keys: {})",
                KEYS.join(", ")
            ));
        };
        check_value(kind, &value).map_err(|why| format!("{} {why}", at(&key)))?;
        match kind {
            KeyType::Profiles => include = string_list(&value),
            KeyType::Keys => clear = string_list(&value),
            _ => settings.push((key, value)),
        }
    }
    if let Some(key) = clear.iter().find(|k| settings.iter().any(|(s, _)| s == *k)) {
        return Err(format!(
            "{}: profile [{name}] both sets and clears '{key}'",
            path
        ));
    }
    Ok(Profile {
        name,
        include,
        clear,
        settings,
    })
}

/// The strings of an array value already checked by [`check_value`].
fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Check that `value` fits `kind`. The error describes what is expected
/// and never quotes the value.
fn check_value(kind: KeyType, value: &Value) -> Result<(), String> {
    let integer = value.as_integer();
    match kind {
        KeyType::Str => value.as_str().map(drop).ok_or("must be a string".into()),
        KeyType::ApiType => match value.as_str() {
            Some(s) if ApiType::parse(s).is_some() => Ok(()),
            _ => Err("must be \"OpenAI\" (the only API type supported for now)".into()),
        },
        KeyType::U32 => match integer {
            Some(n) if u32::try_from(n).is_ok() => Ok(()),
            _ => Err(format!("must be a whole number from 0 to {}", u32::MAX)),
        },
        KeyType::Size => match integer {
            Some(n) if n > 0 && usize::try_from(n).is_ok() => Ok(()),
            _ => Err("must be a positive whole number of bytes".into()),
        },
        KeyType::Bool => value
            .as_bool()
            .map(drop)
            .ok_or("must be true or false (without quotes)".into()),
        KeyType::Timeout => match integer {
            Some(n) if n >= 0 => Ok(()),
            _ => Err("must be a whole number of seconds (0 = no limit)".into()),
        },
        KeyType::Input => check_input(value),
        KeyType::Profiles => {
            let names = unique_strings(value, "profile names")?;
            match names.iter().find(|n| !valid_profile_name(n)) {
                Some(_) => Err("lists a name that is not a valid profile name".into()),
                None => Ok(()),
            }
        }
        KeyType::Keys => {
            for key in unique_strings(value, "key names")? {
                match key {
                    "include" | "clear" => return Err(format!("cannot list '{key}'")),
                    _ if key_type(key).is_none() => {
                        return Err(format!("lists unknown key '{key}'"))
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }
}

/// The strings of an array of strings without repeats; `what` names them
/// in the error.
fn unique_strings<'a>(value: &'a Value, what: &str) -> Result<Vec<&'a str>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("must be a list of {what}, e.g. [\"a\", \"b\"]"))?;
    let mut strings: Vec<&str> = Vec::new();
    for item in items {
        let s = item
            .as_str()
            .ok_or_else(|| format!("must be a list of {what} (strings)"))?;
        if strings.contains(&s) {
            return Err(format!("lists '{s}' twice"));
        }
        strings.push(s);
    }
    Ok(strings)
}

/// Check an `input` value: a non-empty list of known kinds, or `["none"]`.
fn check_input(value: &Value) -> Result<(), String> {
    let kinds = unique_strings(value, "input kinds")?;
    let known: Vec<&str> = InputKind::ALL.iter().map(|k| k.name()).collect();
    if kinds.is_empty() {
        return Err(format!(
            "must list at least one of {}, or be [\"{INPUT_NONE}\"]",
            known.join(", ")
        ));
    }
    if kinds.contains(&INPUT_NONE) && kinds.len() > 1 {
        return Err(format!(
            "\"{INPUT_NONE}\" cannot be combined with other kinds"
        ));
    }
    match kinds
        .iter()
        .find(|k| **k != INPUT_NONE && InputKind::parse(k).is_none())
    {
        Some(kind) => Err(format!(
            "lists unknown kind '{kind}' (known: {}, {INPUT_NONE})",
            known.join(", ")
        )),
        None => Ok(()),
    }
}

/// What the resolved `input` of a profile accepts.
fn accepts(resolved: &Resolved) -> Accepts {
    match resolved.get("input") {
        None => Accepts::Kinds(InputKind::ALL.to_vec()),
        Some(setting) => {
            let kinds = string_list(&setting.value);
            if kinds.iter().any(|k| k == INPUT_NONE) {
                Accepts::Nothing
            } else {
                Accepts::Kinds(kinds.iter().filter_map(|k| InputKind::parse(k)).collect())
            }
        }
    }
}

/// One profile picked with `-p` (or the default one).
#[derive(Debug)]
pub struct Selected {
    pub name: String,
    /// The kinds this profile handles in this run (empty if later
    /// profiles took all of them).
    pub handles: Vec<InputKind>,
    /// The settings to run with; only built for profiles that handle
    /// something, since only those need a `model`.
    pub config: Option<Config>,
    resolved: Resolved,
}

/// The profiles picked for a run and which one handles which kind.
#[derive(Debug)]
pub struct Selection {
    pub profiles: Vec<Selected>,
}

impl Selection {
    /// The profile handling `kind`, with its config, if any selected
    /// profile accepts it.
    pub fn handler(&self, kind: InputKind) -> Option<&Config> {
        self.profiles
            .iter()
            .find(|p| p.handles.contains(&kind))
            .and_then(|p| p.config.as_ref())
    }

    /// Every config that handles something.
    pub fn handlers(&self) -> impl Iterator<Item = &Config> {
        self.profiles.iter().filter_map(|p| p.config.as_ref())
    }

    /// Mutable access to every config that handles something (to apply
    /// command line overrides).
    pub fn handlers_mut(&mut self) -> impl Iterator<Item = &mut Config> {
        self.profiles.iter_mut().filter_map(|p| p.config.as_mut())
    }

    /// The config used before the kind of input is known (beep tones for
    /// early errors): the last selected profile's. The last profile
    /// always handles something, as no later one can take its kinds.
    pub fn fallback(&self) -> Option<&Config> {
        self.profiles.last().and_then(|p| p.config.as_ref())
    }

    /// The names of the selected profiles, comma-separated.
    pub fn names(&self) -> String {
        self.profiles
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// One warning per selected profile that handles nothing because
    /// profiles after it took every kind it accepts.
    pub fn warnings(&self) -> Vec<String> {
        self.profiles
            .iter()
            .filter(|p| p.handles.is_empty())
            .map(|p| {
                format!(
                    "Warning: profile [{}] is selected but handles nothing (overridden by {})",
                    p.name,
                    self.overriders(p)
                )
            })
            .collect()
    }

    /// The profiles that took the kinds `unused` accepts, as `[a], [b]`.
    fn overriders(&self, unused: &Selected) -> String {
        let mut names: Vec<&str> = Vec::new();
        if let Accepts::Kinds(kinds) = accepts(&unused.resolved) {
            for kind in kinds {
                if let Some(p) = self.profiles.iter().find(|p| p.handles.contains(&kind)) {
                    if !names.contains(&p.name.as_str()) {
                        names.push(&p.name);
                    }
                }
            }
        }
        names
            .iter()
            .map(|n| format!("[{n}]"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl ConfigFile {
    /// The profile called `name`, if any.
    fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Names of all profiles, in file order.
    pub fn profile_names(&self) -> Vec<&str> {
        self.profiles.iter().map(|p| p.name.as_str()).collect()
    }

    /// Whether any profile has a non-empty `api_key`.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn has_api_key(&self) -> bool {
        self.profiles.iter().any(|p| {
            p.settings
                .iter()
                .any(|(k, v)| k == "api_key" && v.as_str().is_some_and(|s| !s.trim().is_empty()))
        })
    }

    /// The available profiles, for error messages.
    fn available(&self) -> String {
        format!(
            "profiles in {}: {}",
            self.path,
            self.profile_names().join(", ")
        )
    }

    /// The settings of `name` after includes and `clear`.
    fn resolve(&self, name: &str) -> Result<Resolved, String> {
        self.resolve_chain(name, &mut Vec::new())
    }

    /// [`Self::resolve`], with `chain` the profiles currently being
    /// resolved (to detect include cycles and name the includer).
    fn resolve_chain(&self, name: &str, chain: &mut Vec<String>) -> Result<Resolved, String> {
        if let Some(start) = chain.iter().position(|n| n == name) {
            let cycle: Vec<String> = chain[start..]
                .iter()
                .chain(std::iter::once(&name.to_string()))
                .map(|n| format!("[{n}]"))
                .collect();
            return Err(format!(
                "{}: include cycle: {}",
                self.path,
                cycle.join(" -> ")
            ));
        }
        let profile = self.profile(name).ok_or_else(|| match chain.last() {
            Some(includer) => format!(
                "Profile [{includer}] includes unknown profile [{name}] ({})",
                self.available()
            ),
            None => format!("Unknown profile [{name}] ({})", self.available()),
        })?;
        chain.push(name.to_string());
        let mut resolved = Resolved::new();
        for included in &profile.include {
            resolved.extend(self.resolve_chain(included, chain)?);
        }
        chain.pop();
        for key in &profile.clear {
            resolved.remove(key);
        }
        for (key, value) in &profile.settings {
            resolved.insert(
                key.clone(),
                Setting {
                    value: value.clone(),
                    source: name.to_string(),
                },
            );
        }
        Ok(resolved)
    }

    /// Pick the profiles for a run: `names` from `-p`, or
    /// [`DEFAULT_PROFILE`]. Each kind of input goes to the last profile in
    /// the list that accepts it. Unknown profiles and profiles with
    /// `input = ["none"]` are errors; a profile handling something must
    /// have a `model`.
    pub fn select(&self, names: Option<&[String]>) -> Result<Selection, String> {
        let default = [DEFAULT_PROFILE.to_string()];
        let implicit = names.is_none();
        let names = names.unwrap_or(&default);
        let mut picked = Vec::new();
        for name in names {
            if implicit && self.profile(name).is_none() {
                return Err(format!(
                    "No [{DEFAULT_PROFILE}] profile, so a profile must be picked with -p ({})",
                    self.available()
                ));
            }
            let resolved = self.resolve(name)?;
            match accepts(&resolved) {
                Accepts::Nothing => return Err(self.none_selected(name, &resolved)),
                Accepts::Kinds(kinds) => picked.push((name.clone(), kinds, resolved)),
            }
        }
        let mut profiles = Vec::new();
        for (index, (name, _, resolved)) in picked.iter().enumerate() {
            let handles: Vec<InputKind> = InputKind::ALL
                .into_iter()
                .filter(|kind| {
                    picked
                        .iter()
                        .rposition(|(_, kinds, _)| kinds.contains(kind))
                        == Some(index)
                })
                .collect();
            let config = if handles.is_empty() {
                None
            } else {
                Some(self.build_config(name, &handles, resolved)?)
            };
            profiles.push(Selected {
                name: name.clone(),
                handles,
                config,
                resolved: resolved.clone(),
            });
        }
        Ok(Selection { profiles })
    }

    /// The error for selecting a profile that handles nothing.
    fn none_selected(&self, name: &str, resolved: &Resolved) -> String {
        let mut message = format!(
            "Profile [{name}] has input = [\"{INPUT_NONE}\"] (it handles nothing) and cannot be \
             selected"
        );
        if let Some(source) = resolved.get("input").map(|s| &s.source) {
            if source != name {
                message.push_str(&format!(
                    "; it got that input from [{source}] - set input in [{name}], or add \
                     \"input\" to its clear list"
                ));
            }
        }
        message
    }

    /// Build the run settings of profile `name` from its `resolved`
    /// settings; `handles` is only used in the missing-model message.
    fn build_config(
        &self,
        name: &str,
        handles: &[InputKind],
        resolved: &Resolved,
    ) -> Result<Config, String> {
        let get = |key: &str| {
            resolved
                .get(key)
                .map(|s| s.value.clone())
                .or_else(|| default_value(key))
        };
        let string = |key: &str| get(key).and_then(|v| v.as_str().map(str::to_string));
        let int = |key: &str| get(key).and_then(|v| v.as_integer()).unwrap_or(0);
        let boolean = |key: &str| get(key).and_then(|v| v.as_bool()).unwrap_or(false);

        let model = string("model")
            .filter(|m| !m.trim().is_empty())
            .ok_or_else(|| {
                let kinds: Vec<&str> = handles.iter().map(|k| k.name()).collect();
                format!(
                    "Profile [{name}] handles {} but has no model: set model in [{name}] or in a \
                     profile it includes ({})",
                    kinds.join(" and "),
                    self.path
                )
            })?;
        Ok(Config {
            api_type: ApiType::OpenAI,
            api_url: string("api_url").unwrap_or_default(),
            api_key: normalize_api_key(string("api_key")),
            model,
            prompt_text: string("prompt_text").unwrap_or_default(),
            prompt_image: string("prompt_image").unwrap_or_default(),
            beep_tone: u32::try_from(int("beep_tone")).unwrap_or(DEFAULT_BEEP_TONE),
            error_tone: u32::try_from(int("error_tone")).unwrap_or(DEFAULT_ERROR_TONE),
            beep_duration: u32::try_from(int("beep_duration")).unwrap_or(DEFAULT_BEEP_DURATION),
            max_input_bytes: usize::try_from(int("max_input_bytes"))
                .unwrap_or(DEFAULT_MAX_INPUT_BYTES),
            max_image_bytes: usize::try_from(int("max_image_bytes"))
                .unwrap_or(DEFAULT_MAX_IMAGE_BYTES),
            allow_redirects: boolean("allow_redirects"),
            secret_check: boolean("secret_check"),
            request_timeout: timeout_from_secs(
                u64::try_from(int("request_timeout")).unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
            ),
        })
    }

    /// The `--show-profile` report: each selected profile's settings after
    /// includes, with the profile each value came from, then which profile
    /// handles which kind. The API key is never shown, only whether one is
    /// set.
    pub fn show(&self, selection: &Selection) -> String {
        let mut out = String::new();
        for selected in &selection.profiles {
            let profile = self.profile(&selected.name);
            out.push_str(&format!("[{}]\n", selected.name));
            if let Some(profile) = profile.filter(|p| !p.include.is_empty()) {
                out.push_str(&format!("  # includes: {}\n", profile.include.join(", ")));
            }
            if let Some(profile) = profile.filter(|p| !p.clear.is_empty()) {
                out.push_str(&format!("  # clears:   {}\n", profile.clear.join(", ")));
            }
            let handles = if selected.handles.is_empty() {
                format!("nothing (overridden by {})", selection.overriders(selected))
            } else {
                kind_names(&selected.handles)
            };
            out.push_str(&format!("  # handles:  {handles}\n"));
            for key in KEYS.iter().filter(|k| !matches!(**k, "include" | "clear")) {
                let setting = selected.resolved.get(*key);
                let value = match (*key, setting) {
                    ("api_key", Some(s))
                        if s.value.as_str().is_some_and(|k| !k.trim().is_empty()) =>
                    {
                        "(set, hidden)".to_string()
                    }
                    ("api_key", Some(_)) => "\"\" (no key sent)".to_string(),
                    (_, Some(s)) => display_value(&s.value),
                    ("input", None) => format!("(every kind: {})", kind_names(&InputKind::ALL)),
                    (_, None) => default_value(key)
                        .map(|v| display_value(&v))
                        .unwrap_or_else(|| "(not set)".to_string()),
                };
                let source = match setting {
                    Some(s) => format!("[{}]", s.source),
                    None => "built-in default".to_string(),
                };
                out.push_str(&format!("  {key:<16} = {value}  # {source}\n"));
            }
            out.push('\n');
        }
        out.push_str("# routing:\n");
        for kind in InputKind::ALL {
            let handler = selection
                .profiles
                .iter()
                .find(|p| p.handles.contains(&kind))
                .map(|p| format!("[{}]", p.name))
                .unwrap_or_else(|| "(no selected profile - refused)".to_string());
            out.push_str(&format!("#   {:<6} -> {handler}\n", kind.name()));
        }
        out
    }
}

/// `kinds` as `text, image`.
fn kind_names(kinds: &[InputKind]) -> String {
    kinds
        .iter()
        .map(|k| k.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A value on one line, in TOML syntax (strings as basic strings with
/// escapes, so multi-line prompts stay on one line).
fn display_value(value: &Value) -> String {
    match value {
        Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(display_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Integer(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        // Other types never pass check_value.
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `contents` to a fresh file in the temp dir with `mode` (Unix
    /// only) and return its path.
    fn temp_config(name: &str, contents: &str, mode: u32) -> String {
        let path =
            env::temp_dir().join(format!("clipgpt-test-{}-{}.toml", std::process::id(), name));
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

    /// Parse `contents` as a config file named `t.toml`.
    fn parse(contents: &str) -> Result<ConfigFile, String> {
        parse_file("t.toml", contents)
    }

    /// The error from parsing `contents`, which must fail.
    fn parse_err(contents: &str) -> String {
        parse(contents).expect_err("config should be refused")
    }

    /// Select `names` (comma-separated, `None` = default) in `contents`.
    fn select(contents: &str, names: Option<&str>) -> Result<Selection, String> {
        let names: Option<Vec<String>> = names.map(|n| n.split(',').map(String::from).collect());
        parse(contents)?.select(names.as_deref())
    }

    /// The config of the single default profile in `contents`.
    fn default_config(contents: &str) -> Config {
        let selection = select(contents, None).unwrap();
        selection.handler(InputKind::Text).unwrap().clone()
    }

    #[test]
    fn key_file_mode_check() {
        assert!(check_key_file_mode(0o100600, "f").is_ok());
        assert!(check_key_file_mode(0o100400, "f").is_ok());
        assert!(check_key_file_mode(0o100640, "f").is_err());
        assert!(check_key_file_mode(0o100604, "f").is_err());
        assert!(check_key_file_mode(0o100660, "f").is_err());
        let err = check_key_file_mode(0o100644, "/x/config.toml").unwrap_err();
        assert!(err.contains("chmod 600 '/x/config.toml'"));
        assert!(err.contains("644"));
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
    fn defaults_apply_to_unset_keys() {
        let cfg = default_config("[default]\nmodel = \"llama3\"\n");
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.model, "llama3");
        assert_eq!(cfg.api_url, DEFAULT_API_URL);
        assert_eq!(cfg.prompt_text, DEFAULT_PROMPT_TEXT);
        assert_eq!(cfg.prompt_image, DEFAULT_PROMPT_IMAGE);
        assert_eq!(cfg.beep_tone, DEFAULT_BEEP_TONE);
        assert_eq!(cfg.error_tone, DEFAULT_ERROR_TONE);
        assert_eq!(cfg.beep_duration, DEFAULT_BEEP_DURATION);
        assert_eq!(cfg.max_input_bytes, DEFAULT_MAX_INPUT_BYTES);
        assert_eq!(cfg.max_image_bytes, 10_485_760);
        assert!(!cfg.allow_redirects);
        assert!(cfg.secret_check);
        assert_eq!(cfg.request_timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn all_keys_are_parsed() {
        let cfg = default_config(
            "[default]\nmodel = \"m\"\napi_type = \"openai\"\napi_url = \"http://127.0.0.1/\"\n\
             api_key = \" sk-x \"\nprompt_text = \"a\\nb\"\nprompt_image = 'c\\nd'\n\
             beep_tone = 1\nerror_tone = 2\nbeep_duration = 0\nmax_input_bytes = 1000\n\
             max_image_bytes = 2000\nallow_redirects = true\nsecret_check = false\n\
             request_timeout = 0\ninput = [\"image\", \"text\"]\n",
        );
        assert_eq!(cfg.api_url, "http://127.0.0.1/");
        assert_eq!(cfg.api_key.as_deref(), Some("sk-x"));
        // Basic strings take escapes, literal strings do not.
        assert_eq!(cfg.prompt_text, "a\nb");
        assert_eq!(cfg.prompt_image, "c\\nd");
        assert_eq!(
            (cfg.beep_tone, cfg.error_tone, cfg.beep_duration),
            (1, 2, 0)
        );
        assert_eq!((cfg.max_input_bytes, cfg.max_image_bytes), (1000, 2000));
        assert!(cfg.allow_redirects);
        assert!(!cfg.secret_check);
        assert_eq!(cfg.request_timeout, None);
    }

    #[test]
    fn empty_strings_are_real_values() {
        let cfg = default_config("[default]\nmodel = \"m\"\nprompt_text = \"\"\napi_key = \"\"\n");
        assert_eq!(cfg.prompt_text, "");
        assert_eq!(cfg.api_key, None);
    }

    #[test]
    fn wrong_types_are_errors_without_values() {
        for (line, expect) in [
            ("beep_tone = \"abc\"", "whole number"),
            ("beep_tone = -1", "whole number"),
            ("beep_tone = 4294967296", "whole number"),
            ("beep_duration = 1.5", "whole number"),
            ("max_input_bytes = 0", "positive"),
            ("max_image_bytes = \"64k\"", "positive"),
            ("allow_redirects = \"yes\"", "true or false"),
            ("secret_check = 1", "true or false"),
            ("request_timeout = -1", "0 = no limit"),
            ("request_timeout = \"30s\"", "0 = no limit"),
            ("api_url = 5", "must be a string"),
            ("api_key = 12345678", "must be a string"),
            ("api_type = \"Claude\"", "OpenAI"),
            ("model = true", "must be a string"),
        ] {
            let err = parse_err(&format!("[default]\nmodel = \"m\"\n[other]\n{line}\n"));
            let key = line.split(' ').next().unwrap();
            assert!(
                err.contains(&format!("[other], '{key}'")) && err.contains(expect),
                "{line}: {err}"
            );
            let value = line.split(" = ").nth(1).unwrap().trim_matches('"');
            assert!(!err.contains(value), "{line} leaks value: {err}");
        }
    }

    #[test]
    fn unknown_and_wrong_case_keys_are_errors() {
        let err = parse_err("[default]\nmodle = \"m\"\n");
        assert!(
            err.contains("unknown key 'modle' in profile [default]"),
            "{err}"
        );
        assert!(err.contains("valid keys: include, clear, input"), "{err}");
        assert!(parse_err("[default]\nModel = \"m\"\n").contains("'Model'"));
        // A sub-table is an unknown key too.
        assert!(parse_err("[default.sub]\nmodel = \"m\"\n").contains("unknown key 'sub'"));
    }

    #[test]
    fn toml_syntax_errors_name_the_line_not_the_content() {
        let err = parse_err("[default]\nmodel = \"m\"\napi_key = sk-secret-value\n");
        assert!(err.contains("t.toml: invalid TOML at line 3"), "{err}");
        assert!(!err.contains("sk-secret"), "{err}");
        let err = parse_err("[default]\nmodel = \"m\"\nmodel = \"n\"\n");
        assert!(
            err.contains("line 3") && err.contains("duplicate key"),
            "{err}"
        );
        assert!(parse_err("[a]\nmodel = \"m\"\n[a]\n").contains("line 3"));
        assert!(parse_err("[a]\nmodel \"m\"\n").contains("line 2"));
    }

    #[test]
    fn blank_lines_comments_crlf_and_indentation_are_fine() {
        let cfg = default_config(
            "# comment\r\n \t \r\n  [default]  # trailing\r\n\tmodel = \"m\" \t\r\n",
        );
        assert_eq!(cfg.model, "m");
    }

    #[test]
    fn settings_outside_profiles_and_bad_names_are_errors() {
        let err = parse_err("model = \"m\"\n[default]\n");
        assert!(err.contains("'model' is outside any [profile]"), "{err}");
        assert!(parse_err("[\"a,b\"]\nmodel = \"m\"\n").contains("invalid profile name"));
        assert!(parse_err("[\"\"]\nmodel = \"m\"\n").contains("invalid profile name"));
        assert!(parse_err("[[default]]\nmodel = \"m\"\n").contains("outside any"));
        assert!(parse_err("# nothing\n").contains("defines no profiles"));
    }

    #[test]
    fn includes_apply_in_order_and_own_keys_win() {
        let file = "[a]\ninput = [\"none\"]\nbeep_tone = 1\nerror_tone = 1\nmodel = \"a\"\n\
                    [b]\ninput = [\"none\"]\nbeep_tone = 2\nbeep_duration = 2\n\
                    [default]\ninclude = [\"a\", \"b\"]\nclear = [\"input\"]\nbeep_duration = 3\n";
        let cfg = default_config(file);
        assert_eq!(cfg.beep_tone, 2, "later include wins");
        assert_eq!(cfg.error_tone, 1, "earlier include still applies");
        assert_eq!(cfg.beep_duration, 3, "own key wins");
        assert_eq!(cfg.model, "a");
    }

    #[test]
    fn nested_and_diamond_includes() {
        let file = "[base]\nmodel = \"base\"\napi_url = \"http://base/\"\n\
                    [mid]\ninclude = [\"base\"]\nmodel = \"mid\"\n\
                    [left]\ninclude = [\"base\"]\n\
                    [default]\ninclude = [\"mid\", \"left\"]\n";
        let cfg = default_config(file);
        // [left] re-applies [base] after [mid].
        assert_eq!(cfg.model, "base");
        assert_eq!(cfg.api_url, "http://base/");
    }

    #[test]
    fn clear_restores_the_built_in_default() {
        let file = "[base]\nmodel = \"m\"\napi_key = \"sk-x\"\nbeep_tone = 5\nprompt_text = \"p\"\n\
                    [default]\ninclude = [\"base\"]\nclear = [\"api_key\", \"beep_tone\", \"prompt_text\"]\n";
        let cfg = default_config(file);
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.beep_tone, DEFAULT_BEEP_TONE);
        assert_eq!(cfg.prompt_text, DEFAULT_PROMPT_TEXT);
        assert_eq!(cfg.model, "m");
        // Clearing model leaves the profile without one.
        let err = select(
            "[base]\nmodel = \"m\"\n[default]\ninclude = [\"base\"]\nclear = [\"model\"]\n",
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("[default] handles text and image but has no model"),
            "{err}"
        );
    }

    #[test]
    fn clear_errors() {
        for (clear, expect) in [
            ("[\"modle\"]", "unknown key 'modle'"),
            ("[\"include\"]", "cannot list 'include'"),
            ("[\"clear\"]", "cannot list 'clear'"),
            ("[\"model\", \"model\"]", "twice"),
            ("\"model\"", "must be a list"),
            ("[1]", "strings"),
        ] {
            let err = parse_err(&format!("[default]\nclear = {clear}\n"));
            assert!(err.contains(expect), "{clear}: {err}");
        }
        let err = parse_err("[default]\nmodel = \"m\"\nclear = [\"model\"]\n");
        assert!(err.contains("both sets and clears 'model'"), "{err}");
    }

    #[test]
    fn include_errors() {
        let err = parse_err("[default]\ninclude = [\"nope\"]\nmodel = \"m\"\n");
        assert!(
            err.contains("[default] includes unknown profile [nope]")
                && err.contains("profiles in t.toml: default"),
            "{err}"
        );
        let err =
            parse_err("[a]\ninclude = [\"b\"]\n[b]\ninclude = [\"c\"]\n[c]\ninclude = [\"a\"]\n");
        assert!(
            err.contains("include cycle: [a] -> [b] -> [c] -> [a]"),
            "{err}"
        );
        assert!(parse_err("[a]\ninclude = [\"a\"]\n").contains("[a] -> [a]"));
        assert!(parse_err("[a]\ninclude = [\"x\", \"x\"]\n").contains("twice"));
        assert!(parse_err("[a]\ninclude = \"x\"\n").contains("must be a list"));
        // Checked in profiles that are never selected too.
        assert!(
            parse_err("[default]\nmodel = \"m\"\n[unused]\ninclude = [\"gone\"]\n")
                .contains("unknown profile [gone]")
        );
    }

    #[test]
    fn input_values() {
        for (input, expect) in [
            ("[]", "at least one"),
            ("[\"none\", \"text\"]", "cannot be combined"),
            ("[\"video\"]", "unknown kind 'video'"),
            ("[\"text\", \"text\"]", "twice"),
            ("[\"Text\"]", "unknown kind 'Text'"),
            ("\"text\"", "must be a list"),
        ] {
            let err = parse_err(&format!("[default]\nmodel = \"m\"\ninput = {input}\n"));
            assert!(
                err.contains("'input'") && err.contains(expect),
                "{input}: {err}"
            );
        }
    }

    #[test]
    fn default_profile_is_used_without_p_and_required() {
        let sel = select("[default]\nmodel = \"m\"\n[x]\nmodel = \"x\"\n", None).unwrap();
        assert_eq!(sel.names(), "default");
        let err = select("[openai]\nmodel = \"m\"\n[x]\nmodel = \"x\"\n", None).unwrap_err();
        assert!(
            err.contains("No [default] profile") && err.contains("-p") && err.contains("openai, x"),
            "{err}"
        );
    }

    #[test]
    fn unknown_selected_profile_lists_available() {
        let err = select(
            "[default]\nmodel = \"m\"\n[ocr]\nmodel = \"o\"\n",
            Some("ocrr"),
        )
        .unwrap_err();
        assert!(
            err.contains("Unknown profile [ocrr]") && err.contains("default, ocr"),
            "{err}"
        );
    }

    /// A file with profiles `both` (every kind), `text` and `image` (one
    /// kind each) and `beeps` (none).
    const ROUTING: &str = "[beeps]\ninput = [\"none\"]\nbeep_tone = 880\n\
                           [both]\ninclude = [\"beeps\"]\nclear = [\"input\"]\nmodel = \"both\"\n\
                           [text]\ninput = [\"text\"]\nmodel = \"text\"\n\
                           [image]\ninput = [\"image\"]\nmodel = \"image\"\n";

    /// The model handling each kind (text, image) for `-p names`.
    fn routes(names: &str) -> (Option<String>, Option<String>) {
        let sel = select(ROUTING, Some(names)).unwrap();
        let model = |kind| sel.handler(kind).map(|c| c.model.clone());
        (model(InputKind::Text), model(InputKind::Image))
    }

    #[test]
    fn last_profile_accepting_a_kind_handles_it() {
        let s = |m: &str| Some(m.to_string());
        assert_eq!(routes("both"), (s("both"), s("both")));
        assert_eq!(routes("both,text"), (s("text"), s("both")));
        assert_eq!(routes("image,both"), (s("both"), s("both")));
        assert_eq!(routes("text,image"), (s("text"), s("image")));
        assert_eq!(routes("image"), (None, s("image")));
        assert_eq!(routes("text"), (s("text"), None));
    }

    #[test]
    fn unused_profiles_warn_and_need_no_model() {
        let file = format!("{ROUTING}[nomodel]\ninput = [\"image\"]\n");
        let sel = select(&file, Some("nomodel,image,both")).unwrap();
        let warnings = sel.warnings();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0]
            .contains("[nomodel] is selected but handles nothing (overridden by [both])"));
        assert!(sel.profiles[0].config.is_none());
        assert_eq!(sel.fallback().unwrap().model, "both");
        assert_eq!(sel.handlers().count(), 1);
        // But a profile that handles something needs one.
        let err = select(&file, Some("text,nomodel")).unwrap_err();
        assert!(
            err.contains("[nomodel] handles image but has no model"),
            "{err}"
        );
    }

    #[test]
    fn none_profiles_cannot_be_selected() {
        let err = select(ROUTING, Some("beeps")).unwrap_err();
        assert!(err.contains("[beeps] has input = [\"none\"]"), "{err}");
        assert!(select(ROUTING, Some("text,beeps")).is_err());
        // A profile that inherits "none" gets a hint.
        let file = "[beeps]\ninput = [\"none\"]\n[default]\ninclude = [\"beeps\"]\nmodel = \"m\"\n";
        let err = select(file, None).unwrap_err();
        assert!(
            err.contains("got that input from [beeps]") && err.contains("clear list"),
            "{err}"
        );
        // Values of none profiles are still checked.
        assert!(
            parse_err("[beeps]\ninput = [\"none\"]\nbeep_tone = \"x\"\n").contains("beep_tone")
        );
    }

    #[test]
    fn last_profile_always_handles_something() {
        for names in [
            "both",
            "text,both",
            "both,text",
            "image,text",
            "text,image,both",
        ] {
            let sel = select(ROUTING, Some(names)).unwrap();
            assert!(!sel.profiles.last().unwrap().handles.is_empty(), "{names}");
            assert!(sel.fallback().is_some(), "{names}");
        }
    }

    #[test]
    fn show_lists_values_sources_and_routing_but_never_the_key() {
        let file = "[base]\ninput = [\"none\"]\napi_key = \"sk-very-secret\"\nmodel = \"gpt\"\n\
                    prompt_text = \"line1\\nline2\"\n\
                    [default]\ninclude = [\"base\"]\nclear = [\"input\"]\nbeep_tone = 1\n\
                    [ocr]\ninclude = [\"base\"]\ninput = [\"image\"]\n\
                    [nokey]\ninput = [\"text\"]\napi_key = \"\"\nmodel = \"m\"\n";
        let parsed = parse(file).unwrap();
        let sel = parsed
            .select(Some(&["default".into(), "ocr".into()]))
            .unwrap();
        let out = parsed.show(&sel);
        assert!(!out.contains("sk-very"), "{out}");
        assert!(
            out.contains("api_key          = (set, hidden)  # [base]"),
            "{out}"
        );
        assert!(
            out.contains("model            = \"gpt\"  # [base]"),
            "{out}"
        );
        assert!(
            out.contains("prompt_text      = \"line1\\nline2\"  # [base]"),
            "{out}"
        );
        assert!(out.contains("beep_tone        = 1  # [default]"), "{out}");
        assert!(
            out.contains("error_tone       = 131  # built-in default"),
            "{out}"
        );
        assert!(out.contains("input            = (every kind: text, image)  # built-in default"));
        assert!(
            out.contains("input            = [\"image\"]  # [ocr]"),
            "{out}"
        );
        assert!(out.contains("# includes: base") && out.contains("# clears:   input"));
        assert!(out.contains("#   text   -> [default]") && out.contains("#   image  -> [ocr]"));

        let sel = parsed.select(Some(&["nokey".into()])).unwrap();
        let out = parsed.show(&sel);
        assert!(
            out.contains("api_key          = \"\" (no key sent)  # [nokey]"),
            "{out}"
        );
        assert!(
            out.contains("#   image  -> (no selected profile - refused)"),
            "{out}"
        );

        let sel = parsed
            .select(Some(&["ocr".into(), "default".into()]))
            .unwrap();
        let out = parsed.show(&sel);
        assert!(
            out.contains("# handles:  nothing (overridden by [default])"),
            "{out}"
        );
    }

    #[test]
    fn display_value_is_one_line_toml() {
        assert_eq!(
            display_value(&Value::from("a\"b\\c\nd\te\u{1}")),
            "\"a\\\"b\\\\c\\nd\\te\\u0001\""
        );
        assert_eq!(
            display_value(&Value::Array(vec!["x".into(), "y".into()])),
            "[\"x\", \"y\"]"
        );
        assert_eq!(display_value(&Value::from(true)), "true");
        assert_eq!(display_value(&Value::from(7)), "7");
    }

    #[test]
    fn profile_names_in_file_order() {
        let file = parse("[zeta]\nmodel = \"m\"\n[alpha]\nmodel = \"m\"\n[mid-1]\n").unwrap();
        assert_eq!(file.profile_names(), ["zeta", "alpha", "mid-1"]);
    }

    #[test]
    fn parse_timeout_values() {
        assert_eq!(parse_timeout("k", "0"), Ok(None));
        assert_eq!(
            parse_timeout("k", " 120 "),
            Ok(Some(Duration::from_secs(120)))
        );
        for bad in ["-1", "1.5", "ten", "", "30s"] {
            let err = parse_timeout("--timeout", bad).unwrap_err();
            assert!(
                err.contains("--timeout") && err.contains("0 = no limit"),
                "{err}"
            );
        }
    }

    #[test]
    fn not_found_message_mentions_old_ini_only_if_present() {
        let msg = not_found_message("/h/.config/clipgpt/config.toml", false);
        assert!(msg.contains("config.toml.example") && !msg.contains("config.ini"));
        assert!(not_found_message("x", true).contains("config.ini was found"));
    }

    #[cfg(unix)]
    #[test]
    fn key_in_any_profile_needs_a_private_file() {
        let contents = "[default]\nmodel = \"m\"\n[unused]\napi_key = \"sk-abc\"\n";
        let path = temp_config("openkey", contents, 0o644);
        let err = load_from_path(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(err.contains("chmod 600"));
        assert!(!err.contains("sk-abc"));
    }

    #[test]
    fn key_in_private_file_is_loaded() {
        let path = temp_config(
            "privkey",
            "[default]\napi_key = \"sk-abc\"\nmodel = \"m\"\n",
            0o600,
        );
        let file = load_from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let sel = file.select(None).unwrap();
        assert_eq!(
            sel.handler(InputKind::Text).unwrap().api_key.as_deref(),
            Some("sk-abc")
        );
    }

    #[test]
    fn empty_key_skips_mode_check() {
        let path = temp_config(
            "emptykey",
            "[default]\napi_key = \" \"\nmodel = \"m\"\n",
            0o644,
        );
        let file = load_from_path(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(file.is_ok());
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        assert!(load_from_path("/nonexistent/clipgpt/config.toml").is_err());
    }

    #[test]
    fn non_utf8_file_is_an_error() {
        let path = env::temp_dir().join(format!("clipgpt-test-{}-latin1.toml", std::process::id()));
        std::fs::write(&path, b"[default]\nmodel = \"\xe9\"\n").unwrap();
        let err = load_from_path(&path.to_string_lossy());
        std::fs::remove_file(&path).unwrap();
        assert!(err.unwrap_err().contains("Cannot read config file"));
    }

    /// The short example config (one profile).
    const EXAMPLE_SHORT: &str = include_str!("../config.toml.example");
    /// The example config with several profiles and includes.
    const EXAMPLE_EXPANDED: &str = include_str!("../config.expanded.toml.example");
    /// The example config listing every key.
    const EXAMPLE_FULL: &str = include_str!("../config.full.toml.example");

    /// Key names set or commented out (`#key = ...`) in `example`.
    fn example_keys(example: &str) -> Vec<&str> {
        let mut keys: Vec<&str> = example
            .lines()
            .map(|l| l.trim_start_matches('#').trim())
            .filter_map(|l| l.split_once(" = ").map(|(k, _)| k))
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    #[test]
    fn every_example_is_valid_and_its_profiles_selectable() {
        for (name, example) in [
            ("short", EXAMPLE_SHORT),
            ("expanded", EXAMPLE_EXPANDED),
            ("full", EXAMPLE_FULL),
        ] {
            let file = parse(example).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(file.select(None).is_ok(), "{name}: [default]");
            // Every profile can be selected on its own except the
            // include-only ones.
            for profile in file.profile_names() {
                let one = [profile.to_string()];
                let resolved = file.resolve(profile).unwrap();
                assert_eq!(
                    file.select(Some(&one)).is_ok(),
                    accepts(&resolved) != Accepts::Nothing,
                    "{name}: [{profile}]"
                );
            }
        }
    }

    #[test]
    fn short_example_is_one_plain_profile_with_the_default_prompt() {
        let file = parse(EXAMPLE_SHORT).unwrap();
        assert_eq!(file.profile_names(), [DEFAULT_PROFILE]);
        let profile = &file.profiles[0];
        assert!(profile.include.is_empty() && profile.clear.is_empty());
        let cfg = default_config(EXAMPLE_SHORT);
        assert_eq!(cfg.prompt_text, DEFAULT_PROMPT_TEXT);
        assert_eq!(cfg.prompt_image, DEFAULT_PROMPT_IMAGE);
    }

    #[test]
    fn expanded_example_routes_text_and_images_apart() {
        let file = parse(EXAMPLE_EXPANDED).unwrap();
        assert!(file.profiles.iter().any(|p| !p.include.is_empty()));
        let sel = file
            .select(Some(&["default".into(), "ocr".into()]))
            .unwrap();
        assert!(sel.warnings().is_empty());
        let text = sel.handler(InputKind::Text).unwrap();
        let image = sel.handler(InputKind::Image).unwrap();
        assert_eq!(text.model, "gpt-4o-mini");
        assert!(text.api_key.is_some());
        assert_eq!(image.model, "qwen2.5vl");
        assert_eq!(image.api_key, None, "api_key = \"\" in [local]");
        assert!(image.api_url.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn full_example_covers_every_key() {
        let mut all = KEYS.to_vec();
        all.sort_unstable();
        assert_eq!(example_keys(EXAMPLE_FULL), all);
        let cfg = default_config(EXAMPLE_FULL);
        assert_eq!(cfg.prompt_image, DEFAULT_PROMPT_IMAGE);
        assert_eq!(cfg.max_image_bytes, DEFAULT_MAX_IMAGE_BYTES);
    }

    #[test]
    fn full_example_prompt_image_matches_default() {
        let line = EXAMPLE_FULL
            .lines()
            .find_map(|l| l.strip_prefix("prompt_image = "))
            .expect("prompt_image in config.full.toml.example");
        assert_eq!(line, display_value(&Value::from(DEFAULT_PROMPT_IMAGE)));
    }

    #[test]
    fn examples_point_to_the_full_documentation() {
        for example in [EXAMPLE_SHORT, EXAMPLE_EXPANDED, EXAMPLE_FULL] {
            assert!(example.contains("man clip_gpt"));
            assert!(example.contains("README.md at\n# https://github.com/TheHolm/clipGPT"));
        }
    }

    /// The man page with roff hyphen escapes (`\-`) undone.
    fn man_page() -> String {
        include_str!("../man/clip_gpt.1").replace("\\-", "-")
    }

    #[test]
    fn man_page_documents_every_config_key() {
        let man = man_page();
        for key in KEYS {
            assert!(man.contains(&format!(".It Cm {key}\n")), "{key} missing");
        }
    }

    #[test]
    fn man_page_defaults_match_code() {
        let man = man_page();
        for (key, default) in [
            ("beep_tone", DEFAULT_BEEP_TONE.to_string()),
            ("error_tone", DEFAULT_ERROR_TONE.to_string()),
            ("beep_duration", DEFAULT_BEEP_DURATION.to_string()),
            ("max_input_bytes", DEFAULT_MAX_INPUT_BYTES.to_string()),
            ("max_image_bytes", DEFAULT_MAX_IMAGE_BYTES.to_string()),
            ("allow_redirects", DEFAULT_ALLOW_REDIRECTS.to_string()),
            ("secret_check", DEFAULT_SECRET_CHECK.to_string()),
            ("request_timeout", DEFAULT_REQUEST_TIMEOUT_SECS.to_string()),
        ] {
            let entry = man
                .split(&format!(".It Cm {key}\n"))
                .nth(1)
                .and_then(|rest| rest.split(".It ").next())
                .unwrap_or_else(|| panic!("{key} missing"));
            assert!(
                entry.contains(&format!("default {default}")),
                "{key}: {entry}"
            );
        }
        assert!(man.contains(DEFAULT_API_URL));
        assert!(man.contains(&format!("~/.config/{APP_NAME}/config.toml")));
        assert!(man.contains(DEFAULT_PROFILE));
    }
}
