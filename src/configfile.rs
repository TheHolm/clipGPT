use pretty_ini::{ini, ini_file};

use std::env;
use std::path::Path;

const APP_NAME: &str = "clipgpt";

const SECTION: &str = "openai";

const DEFAULT_API_TYPE: &str = "OpenAI";
const DEFAULT_API_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_PROMPT_TEXT: &str =
    "Check spelling and gramma and rewrite following text on pirate speak. Be mean and pushy \n\n";
const DEFAULT_PROMPT_IMAGE: &str = "OCR attached image and return text formatted as markdown";
const DEFAULT_BEEP_TONE: u32 = 1046;
const DEFAULT_ERROR_TONE: u32 = 131;
const DEFAULT_BEEP_DURATION: u32 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiType {
    OpenAI,
}

impl ApiType {
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

// prompt_image and error_tone are not wired into main.rs yet (image prompts
// aren't implemented, and there's no error-beep code path yet) - kept here
// as reserved config surface for when those land.
#[allow(dead_code)]
pub struct Config {
    pub api_type: ApiType,
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub prompt_text: String,
    pub prompt_image: String,
    pub beep_tone: u32,
    pub error_tone: u32,
    pub beep_duration: u32,
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
        "No config.ini found. Checked '{}' and './config.ini'. Copy config.ini.example to one of those locations and fill in api_key and model.",
        preferred
    ))
}

fn get_value(ini: &ini::Ini, table: &str, key: &str) -> Option<String> {
    ini.get(table, key).ok().map(|variable| variable.value)
}

fn get_u32(ini: &ini::Ini, table: &str, key: &str, default: u32) -> u32 {
    get_value(ini, table, key)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

/// pretty_ini does not interpret escape sequences and trims whitespace off
/// values, so a literal "\n" typed in config.ini stays as a two-character
/// backslash-n instead of becoming a real newline. Turn those into real
/// newlines so prompt text written in the ini file behaves like the
/// Rust-literal defaults (which already contain real newlines).
fn unescape_newlines(value: String) -> String {
    value.replace("\\n", "\n")
}

pub fn read_ini() -> Result<Config, String> {
    let path = resolve_config_path()?;

    let mut file = ini_file::IniFile::default();
    file.set_path(&path);

    let mut ini = ini::Ini::default();
    ini.load(&mut file)?;

    let api_key = get_value(&ini, SECTION, "api_key")
        .ok_or_else(|| format!("'api_key' is required in [{}] section of {}", SECTION, path))?;

    let model = get_value(&ini, SECTION, "model")
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
    })
}
