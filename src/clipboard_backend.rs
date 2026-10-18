//! Per-OS clipboard access.
//!
//! Clipboard access is OS-specific rather than going through a single
//! cross-platform abstraction, because the previous one
//! (`clipboard`/`x11-clipboard`, built on the `xcb` crate) is unmaintained
//! since 2022 and has real, published soundness advisories (e.g.
//! RUSTSEC-2021-0019). See README.md "Clipboard backend" for the full
//! reasoning, including why Linux/FreeBSD are Wayland-only now (X11 is
//! Wayland is tried first; X11 (`x11rb`, see the `x11` submodule) is the
//! fallback for compositors without a data-control protocol (GNOME) and for
//! pure X11 sessions.
//!
//! Reading also detects clipboard contents that a password manager has
//! marked as sensitive, so they are never sent to the LLM:
//! * Wayland and X11: the `x-kde-passwordManagerHint` MIME type / target
//!   (KeePassXC, KDE and others)
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

/// MIME type (Wayland) / target (X11) password managers add to mark a
/// secret.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

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

/// Run as the background X11 clipboard owner (the hidden
/// [`crate::cli::OWNER_HELPER_ARG`] mode). Returns the exit status.
pub fn serve_owner() -> i32 {
    imp::serve_owner()
}

/// True if any of the offered Wayland MIME types or X11 targets is the
/// password-manager hint.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
fn marks_password<'a>(mut mime_types: impl Iterator<Item = &'a str>) -> bool {
    mime_types.any(|m| m == PASSWORD_HINT)
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

/// Exit status (and message) for the owner helper mode on platforms
/// without an X11 clipboard.
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn no_owner_helper() -> i32 {
    eprintln!("The X11 clipboard owner helper is only available on Linux/FreeBSD");
    2
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod x11;

/// Run the X11 fallback after Wayland failed with `wayland_err` (an error
/// meaning Wayland clipboard access is unavailable, not a real failure).
/// Without an X11 display (`display_set` false) or if X11 cannot be
/// reached either, the error names both causes; an error from a reached
/// X11 display is returned as-is. `action` is "get" or "set".
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn with_x11_fallback<T>(
    action: &str,
    wayland_err: String,
    display_set: bool,
    x11_op: impl FnOnce() -> Result<T, x11::Failure>,
) -> Result<T, String> {
    if !display_set {
        return Err(format!(
            "Failed to {action} clipboard content: {wayland_err} (and no X11 DISPLAY to fall \
             back to)"
        ));
    }
    x11_op().map_err(|failure| match failure {
        x11::Failure::Connect(x11_err) => {
            format!("Failed to {action} clipboard content: Wayland: {wayland_err}; X11: {x11_err}")
        }
        x11::Failure::Other(message) => message,
    })
}

/// True if a `DISPLAY` value names an X11 display to fall back to.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn display_is_set(display: Option<std::ffi::OsString>) -> bool {
    display.is_some_and(|d| !d.is_empty())
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod imp {
    use super::{bytes_to_text, display_is_set, marks_password, with_x11_fallback, x11, Clip};
    use std::io::Read;
    use wl_clipboard_rs::copy::{Error as CopyError, MimeType as CopyMimeType, Options, Source};
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

    /// True for the paste errors meaning Wayland clipboard access is not
    /// available at all (no compositor, or one without data-control, such
    /// as GNOME's Mutter), so X11 should be tried instead.
    pub(super) fn paste_unavailable(err: &PasteError) -> bool {
        matches!(
            err,
            PasteError::MissingProtocol { .. }
                | PasteError::WaylandConnection(_)
                | PasteError::SocketOpenError(_)
        )
    }

    /// Copy-side counterpart of [`paste_unavailable`].
    pub(super) fn copy_unavailable(err: &CopyError) -> bool {
        matches!(
            err,
            CopyError::MissingProtocol { .. }
                | CopyError::WaylandConnection(_)
                | CopyError::SocketOpenError(_)
        )
    }

    /// Whether the `DISPLAY` environment variable names an X11 display.
    fn have_display() -> bool {
        display_is_set(std::env::var_os("DISPLAY"))
    }

    /// Read the clipboard via Wayland, falling back to X11 when Wayland
    /// clipboard access is unavailable (see [`paste_unavailable`]).
    pub fn get_text(max_bytes: usize) -> Result<Clip, String> {
        finish_get(wayland_get_text(max_bytes), have_display(), || {
            x11::get_text(max_bytes)
        })
    }

    /// Turn the Wayland read result into the final one: unavailable Wayland
    /// goes to `x11_op` (if `display_set`), "nothing to paste" errors are
    /// an empty text, other Wayland errors are reported.
    pub(super) fn finish_get(
        wayland: Result<Result<Clip, String>, PasteError>,
        display_set: bool,
        x11_op: impl FnOnce() -> Result<Clip, x11::Failure>,
    ) -> Result<Clip, String> {
        match wayland {
            Err(err) if paste_unavailable(&err) => {
                with_x11_fallback("get", err.to_string(), display_set, x11_op)
            }
            Err(err) if is_empty_error(&err) => Ok(Clip::Text(String::new())),
            Err(err) => Err(format!("Failed to get clipboard content: {err}")),
            Ok(result) => result,
        }
    }

    /// Check the offered MIME types for a password hint, then read the text
    /// (see [`read_capped`]). Wayland errors are returned as-is (outer
    /// `Err`) for [`finish_get`] to classify; read/size errors are final
    /// (inner `Err`).
    fn wayland_get_text(max_bytes: usize) -> Result<Result<Clip, String>, PasteError> {
        let types = get_mime_types(ClipboardType::Regular, Seat::Unspecified)?;
        if marks_password(types.iter().map(String::as_str)) {
            return Ok(Ok(Clip::Password));
        }
        let (pipe, _) = get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text)?;
        Ok(read_capped(pipe, max_bytes).map(Clip::Text))
    }

    /// Read text from `source` with a hard cap of `max_bytes + 1` bytes, so
    /// a huge or endless source cannot exhaust memory; more than
    /// `max_bytes` is an error.
    pub(super) fn read_capped(source: impl Read, max_bytes: usize) -> Result<String, String> {
        let mut contents = Vec::new();
        source
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut contents)
            .map_err(|e| format!("Failed to read clipboard contents: {e}"))?;
        bytes_to_text(contents, max_bytes)
    }

    /// Offer `text` as the new clipboard contents via Wayland, falling back
    /// to X11 when Wayland clipboard access is unavailable.
    pub fn set_text(text: String) -> Result<(), String> {
        let source = Source::Bytes(text.clone().into_bytes().into());
        let wayland = Options::new().copy(source, CopyMimeType::Autodetect);
        finish_set(wayland, have_display(), || x11::set_text(text))
    }

    /// Copy-side counterpart of [`finish_get`].
    pub(super) fn finish_set(
        wayland: Result<(), CopyError>,
        display_set: bool,
        x11_op: impl FnOnce() -> Result<(), x11::Failure>,
    ) -> Result<(), String> {
        match wayland {
            Ok(()) => Ok(()),
            Err(err) if copy_unavailable(&err) => {
                with_x11_fallback("set", err.to_string(), display_set, x11_op)
            }
            Err(err) => Err(format!("Failed to set clipboard content: {err}")),
        }
    }

    /// Serve the X11 clipboard as the background owner helper.
    pub fn serve_owner() -> i32 {
        x11::serve_owner()
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

    /// The X11 owner helper does not exist on Windows.
    pub fn serve_owner() -> i32 {
        super::no_owner_helper()
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

    /// The X11 owner helper does not exist on macOS.
    pub fn serve_owner() -> i32 {
        super::no_owner_helper()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_hint_detected() {
        let types = ["text/plain", "x-kde-passwordManagerHint", "UTF8_STRING"];
        assert!(marks_password(types.into_iter()));
    }

    #[test]
    fn wayland_plain_text_not_password() {
        let types = ["text/plain", "text/plain;charset=utf-8", "UTF8_STRING"];
        assert!(!marks_password(types.into_iter()));
        assert!(!marks_password(std::iter::empty()));
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn missing_protocol_or_no_compositor_falls_back() {
        use wl_clipboard_rs::{copy, paste};
        let gnome = paste::Error::MissingProtocol {
            name: "ext-data-control, or wlr-data-control",
            version: 1,
        };
        assert!(imp::paste_unavailable(&gnome));
        let no_socket = || std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(imp::paste_unavailable(&paste::Error::SocketOpenError(
            no_socket()
        )));
        assert!(imp::copy_unavailable(&copy::Error::MissingProtocol {
            name: "x",
            version: 1
        }));
        assert!(imp::copy_unavailable(&copy::Error::SocketOpenError(
            no_socket()
        )));
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn real_wayland_errors_do_not_fall_back() {
        use wl_clipboard_rs::{copy, paste};
        for err in [
            paste::Error::ClipboardEmpty,
            paste::Error::NoSeats,
            paste::Error::NoMimeType,
            paste::Error::SeatNotFound,
            paste::Error::PrimarySelectionUnsupported,
        ] {
            assert!(!imp::paste_unavailable(&err), "{err}");
        }
        assert!(!imp::copy_unavailable(&copy::Error::NoSeats));
        assert!(!imp::copy_unavailable(&copy::Error::SeatNotFound));
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn display_detection() {
        assert!(display_is_set(Some(":0".into())));
        assert!(!display_is_set(Some("".into())));
        assert!(!display_is_set(None));
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn fallback_without_display_skips_x11() {
        let err = with_x11_fallback::<()>("get", "no protocol".into(), false, || {
            panic!("X11 must not be tried without DISPLAY")
        })
        .unwrap_err();
        assert_eq!(
            err,
            "Failed to get clipboard content: no protocol (and no X11 DISPLAY to fall back to)"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn fallback_results() {
        let ok = with_x11_fallback("set", "w".into(), true, || Ok(5));
        assert_eq!(ok, Ok(5));
        let both = with_x11_fallback::<()>("set", "no protocol".into(), true, || {
            Err(x11::Failure::Connect("refused".into()))
        });
        assert_eq!(
            both,
            Err("Failed to set clipboard content: Wayland: no protocol; X11: refused".into())
        );
        let other = with_x11_fallback::<()>("get", "w".into(), true, || {
            Err(x11::Failure::Other(too_large(10)))
        });
        assert_eq!(other, Err(too_large(10)));
    }

    /// A Wayland error meaning "this compositor has no data-control".
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn gnome_paste_error() -> wl_clipboard_rs::paste::Error {
        wl_clipboard_rs::paste::Error::MissingProtocol {
            name: "ext-data-control",
            version: 1,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn finish_get_falls_back_to_x11() {
        let got = imp::finish_get(Err(gnome_paste_error()), true, || {
            Ok(Clip::Text("from x11".into()))
        });
        assert_eq!(got, Ok(Clip::Text("from x11".into())));
        let no_display = imp::finish_get(Err(gnome_paste_error()), false, || {
            panic!("X11 must not be tried without DISPLAY")
        });
        assert!(no_display.unwrap_err().contains("no X11 DISPLAY"));
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn finish_get_wayland_outcomes() {
        use wl_clipboard_rs::paste::Error;
        let never = || -> Result<Clip, x11::Failure> { panic!("no fallback expected") };
        assert_eq!(
            imp::finish_get(Ok(Ok(Clip::Password)), true, never),
            Ok(Clip::Password)
        );
        assert_eq!(
            imp::finish_get(Ok(Err(too_large(3))), true, never),
            Err(too_large(3))
        );
        assert_eq!(
            imp::finish_get(Err(Error::ClipboardEmpty), true, never),
            Ok(Clip::Text(String::new()))
        );
        assert_eq!(
            imp::finish_get(Err(Error::SeatNotFound), true, never),
            Err("Failed to get clipboard content: The requested seat was not found".into())
        );
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn finish_set_outcomes() {
        use wl_clipboard_rs::copy::Error;
        let never = || -> Result<(), x11::Failure> { panic!("no fallback expected") };
        assert_eq!(imp::finish_set(Ok(()), true, never), Ok(()));
        assert_eq!(
            imp::finish_set(Err(Error::NoSeats), true, never),
            Err("Failed to set clipboard content: There are no seats".into())
        );
        let gnome = Error::MissingProtocol {
            name: "ext-data-control",
            version: 1,
        };
        assert_eq!(imp::finish_set(Err(gnome), true, || Ok(())), Ok(()));
        let gnome = Error::MissingProtocol {
            name: "ext-data-control",
            version: 1,
        };
        let err = imp::finish_set(Err(gnome), true, || {
            Err(x11::Failure::Connect("refused".into()))
        })
        .unwrap_err();
        assert!(
            err.contains("Wayland: A required Wayland protocol"),
            "{err}"
        );
        assert!(err.ends_with("X11: refused"), "{err}");
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn read_capped_limits_and_errors() {
        assert_eq!(imp::read_capped(&b"hello"[..], 5), Ok("hello".into()));
        let err = imp::read_capped(&b"secret!"[..], 6).unwrap_err();
        assert!(err.contains("max_input_bytes") && !err.contains("secret"));

        /// A source whose reads always fail.
        struct Broken;
        impl std::io::Read for Broken {
            /// Always fails.
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("pipe closed"))
            }
        }
        let err = imp::read_capped(Broken, 5).unwrap_err();
        assert_eq!(err, "Failed to read clipboard contents: pipe closed");
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
