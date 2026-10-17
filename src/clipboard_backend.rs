//! Per-OS clipboard access.
//!
//! Clipboard access is OS-specific rather than going through a single
//! cross-platform abstraction, because the previous one
//! (`clipboard`/`x11-clipboard`, built on the `xcb` crate) is unmaintained
//! since 2022 and has real, published soundness advisories (e.g.
//! RUSTSEC-2021-0019). See README.md "Clipboard backend" for the full
//! reasoning, including why Linux/FreeBSD are Wayland-only now (X11 is
//! deliberately not supported).
//!
//! Reading also detects clipboard contents that a password manager has
//! marked as sensitive, so they are never sent to the LLM:
//! * Wayland: the `x-kde-passwordManagerHint` MIME type (KeePassXC, KDE
//!   and others)
//! * Windows: the `ExcludeClipboardContentFromMonitorProcessing` and
//!   `Clipboard Viewer Ignore` formats, or `CanIncludeInClipboardHistory`
//!   set to 0
//! * macOS: not detected - the `clipboard` crate cannot list formats

/// What was found on the clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum Clip {
    /// Plain text (empty if the clipboard held no text).
    Text(String),
    /// Contents a password manager marked as sensitive. The text itself is
    /// never read.
    Password,
}

/// Wayland MIME type password managers add to mark a secret.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
const WAYLAND_PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

/// Windows clipboard formats whose mere presence marks a secret.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const WINDOWS_EXCLUDE_FORMATS: [&str; 2] = [
    "ExcludeClipboardContentFromMonitorProcessing",
    "Clipboard Viewer Ignore",
];

/// Windows clipboard format holding a DWORD; 0 means "keep out of history".
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const WINDOWS_HISTORY_FORMAT: &str = "CanIncludeInClipboardHistory";

/// Read the current clipboard contents. Text longer than `max_bytes` (in
/// UTF-8) is an error; it is never returned in part.
pub fn get_text(max_bytes: usize) -> Result<Clip, String> {
    imp::get_text(max_bytes)
}

/// Replace the clipboard contents with `text`.
pub fn set_text(text: String) -> Result<(), String> {
    imp::set_text(text)
}

/// True if any of the offered Wayland MIME types is the password-manager
/// hint.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
fn wayland_marks_password<'a>(mut mime_types: impl Iterator<Item = &'a str>) -> bool {
    mime_types.any(|m| m == WAYLAND_PASSWORD_HINT)
}

/// Decide from the Windows markers whether the clipboard holds a secret:
/// `exclude_present` is whether any of [`WINDOWS_EXCLUDE_FORMATS`] is on the
/// clipboard, `history_flag` the raw bytes of
/// [`WINDOWS_HISTORY_FORMAT`] if present (a little-endian DWORD, 0 = do not
/// include in history).
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn windows_marks_password(exclude_present: bool, history_flag: Option<&[u8]>) -> bool {
    if exclude_present {
        return true;
    }
    match history_flag {
        Some(bytes) if bytes.len() >= 4 => {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == 0
        }
        _ => false,
    }
}

/// Error for clipboard text over the size limit. Only sizes are reported,
/// never content.
fn too_large(max_bytes: usize) -> String {
    format!(
        "Clipboard text is larger than max_input_bytes ({} bytes); not sent",
        max_bytes
    )
}

/// Turn raw clipboard bytes (already capped at `max_bytes + 1`) into text,
/// rejecting anything over `max_bytes`.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
fn bytes_to_text(bytes: Vec<u8>, max_bytes: usize) -> Result<String, String> {
    if bytes.len() > max_bytes {
        return Err(too_large(max_bytes));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reject already-decoded text over `max_bytes`.
#[cfg_attr(
    not(any(target_os = "windows", target_os = "macos", test)),
    allow(dead_code)
)]
fn check_text_size(text: String, max_bytes: usize) -> Result<String, String> {
    if text.len() > max_bytes {
        Err(too_large(max_bytes))
    } else {
        Ok(text)
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod imp {
    use super::{bytes_to_text, wayland_marks_password, Clip};
    use std::io::Read;
    use wl_clipboard_rs::copy::{MimeType as CopyMimeType, Options, Source};
    use wl_clipboard_rs::paste::{
        get_contents, get_mime_types, ClipboardType, Error as PasteError, MimeType, Seat,
    };

    /// True for the paste errors that just mean "nothing to paste".
    fn is_empty_error(err: &PasteError) -> bool {
        matches!(
            err,
            PasteError::NoSeats | PasteError::ClipboardEmpty | PasteError::NoMimeType
        )
    }

    /// Check the offered MIME types for a password hint, then read the text
    /// with a hard cap of `max_bytes + 1` so a huge or endless source
    /// cannot exhaust memory.
    pub fn get_text(max_bytes: usize) -> Result<Clip, String> {
        match get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
            Ok(types) => {
                if wayland_marks_password(types.iter().map(String::as_str)) {
                    return Ok(Clip::Password);
                }
            }
            Err(err) if is_empty_error(&err) => return Ok(Clip::Text(String::new())),
            Err(err) => return Err(format!("Failed to get clipboard content: {err}")),
        }

        match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
            Ok((pipe, _)) => {
                let mut contents = Vec::new();
                pipe.take(max_bytes as u64 + 1)
                    .read_to_end(&mut contents)
                    .map_err(|e| format!("Failed to read clipboard contents: {e}"))?;
                bytes_to_text(contents, max_bytes).map(Clip::Text)
            }
            Err(err) if is_empty_error(&err) => Ok(Clip::Text(String::new())),
            Err(err) => Err(format!("Failed to get clipboard content: {err}")),
        }
    }

    /// Offer `text` as the new clipboard contents.
    pub fn set_text(text: String) -> Result<(), String> {
        Options::new()
            .copy(
                Source::Bytes(text.into_bytes().into()),
                CopyMimeType::Autodetect,
            )
            .map_err(|e| format!("Failed to set clipboard content: {e}"))
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{
        check_text_size, too_large, windows_marks_password, Clip, WINDOWS_EXCLUDE_FORMATS,
        WINDOWS_HISTORY_FORMAT,
    };
    use clipboard_win::{formats, raw, Clipboard, Getter};

    /// Open the clipboard, check the password-manager markers, and read the
    /// text if there is any. The size is checked before copying: UTF-16
    /// takes at most twice the bytes of the same text in UTF-8, so a UTF-16
    /// buffer over `2 * (max_bytes + 1)` bytes (incl. the terminator) is
    /// certainly too large.
    pub fn get_text(max_bytes: usize) -> Result<Clip, String> {
        let _clip =
            Clipboard::new_attempts(10).map_err(|e| format!("Failed to open clipboard: {e}"))?;

        let exclude_present = WINDOWS_EXCLUDE_FORMATS
            .iter()
            .any(|name| raw::register_format(name).is_some_and(|f| raw::is_format_avail(f.get())));
        let mut history = Vec::new();
        let history_flag = raw::register_format(WINDOWS_HISTORY_FORMAT)
            .filter(|f| raw::is_format_avail(f.get()))
            .and_then(|f| raw::get_vec(f.get(), &mut history).ok())
            .map(|_| history.as_slice());
        if windows_marks_password(exclude_present, history_flag) {
            return Ok(Clip::Password);
        }

        if !raw::is_format_avail(formats::CF_UNICODETEXT) {
            return Ok(Clip::Text(String::new()));
        }
        if let Some(size) = raw::size(formats::CF_UNICODETEXT) {
            if size.get() / 2 > max_bytes + 1 {
                return Err(too_large(max_bytes));
            }
        }
        let mut text = String::new();
        formats::Unicode
            .read_clipboard(&mut text)
            .map_err(|e| format!("Failed to get clipboard content: {e}"))?;
        check_text_size(text, max_bytes).map(Clip::Text)
    }

    /// Replace the clipboard contents with `text`.
    pub fn set_text(text: String) -> Result<(), String> {
        clipboard_win::set_clipboard_string(&text)
            .map_err(|e| format!("Failed to set clipboard content: {e}"))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{check_text_size, Clip};
    use clipboard::{ClipboardContext, ClipboardProvider};

    /// Read the clipboard text. Password markers cannot be detected here.
    pub fn get_text(max_bytes: usize) -> Result<Clip, String> {
        let mut ctx: ClipboardContext =
            ClipboardProvider::new().map_err(|e| format!("Failed to initialize clipboard: {e}"))?;
        let text = ctx
            .get_contents()
            .map_err(|e| format!("Failed to get clipboard content: {e}"))?;
        check_text_size(text, max_bytes).map(Clip::Text)
    }

    /// Replace the clipboard contents with `text`.
    pub fn set_text(text: String) -> Result<(), String> {
        let mut ctx: ClipboardContext =
            ClipboardProvider::new().map_err(|e| format!("Failed to initialize clipboard: {e}"))?;
        ctx.set_contents(text)
            .map_err(|e| format!("Failed to set clipboard content: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_hint_detected() {
        let types = ["text/plain", "x-kde-passwordManagerHint", "UTF8_STRING"];
        assert!(wayland_marks_password(types.into_iter()));
    }

    #[test]
    fn wayland_plain_text_not_password() {
        let types = ["text/plain", "text/plain;charset=utf-8", "UTF8_STRING"];
        assert!(!wayland_marks_password(types.into_iter()));
        assert!(!wayland_marks_password(std::iter::empty()));
    }

    #[test]
    fn windows_exclude_format_detected() {
        assert!(windows_marks_password(true, None));
    }

    #[test]
    fn windows_history_zero_detected() {
        assert!(windows_marks_password(false, Some(&[0, 0, 0, 0])));
    }

    #[test]
    fn windows_history_nonzero_or_missing_not_password() {
        assert!(!windows_marks_password(false, Some(&[1, 0, 0, 0])));
        assert!(!windows_marks_password(false, None));
        assert!(!windows_marks_password(false, Some(&[0, 0])));
    }

    #[test]
    fn bytes_within_limit_accepted() {
        assert_eq!(bytes_to_text(b"hello".to_vec(), 5), Ok("hello".to_string()));
    }

    #[test]
    fn bytes_over_limit_rejected_without_content() {
        let err = bytes_to_text(b"secret!".to_vec(), 6).unwrap_err();
        assert!(err.contains("max_input_bytes"));
        assert!(!err.contains("secret"));
    }

    #[test]
    fn text_size_counts_utf8_bytes() {
        // "é" is 2 bytes in UTF-8.
        assert!(check_text_size("éé".into(), 4).is_ok());
        assert!(check_text_size("ééé".into(), 4).is_err());
    }
}
