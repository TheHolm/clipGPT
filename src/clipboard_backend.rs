//! Per-OS clipboard access.
//!
//! Clipboard access is OS-specific rather than going through a single
//! cross-platform abstraction, because the previous one
//! (`clipboard`/`x11-clipboard`, built on the `xcb` crate) is unmaintained
//! since 2022 and has real, published soundness advisories (e.g.
//! RUSTSEC-2021-0019). See README.md "Clipboard backend" for the full
//! reasoning. On Linux/FreeBSD Wayland is tried first; X11 (`x11rb`, see
//! the `x11` submodule) is the fallback for compositors without a
//! data-control protocol (GNOME) and for pure X11 sessions.
//!
//! Reading prefers text: an image is read only when no text, or only
//! white space, is offered (apps such as office suites and browsers often
//! offer both, the image being a rendering of the text). Kinds no selected
//! profile handles are not read at all (see [`Limits`] and
//! [`Clip::Unwanted`]), so with only image profiles selected an image is
//! read even when text is offered too.
//!
//! Reading also detects clipboard contents that a password manager has
//! marked as sensitive, so they are never sent to the LLM:
//! * Wayland and X11: the `x-kde-passwordManagerHint` MIME type / target
//!   (KeePassXC, KDE and others)
//! * Windows: the `ExcludeClipboardContentFromMonitorProcessing` and
//!   `Clipboard Viewer Ignore` formats, or `CanIncludeInClipboardHistory`
//!   set to 0
//! * macOS: the `org.nspasteboard.ConcealedType` pasteboard type

use crate::configfile::InputKind;
use crate::imagefile::{too_large as image_too_large, ClipImage};

/// What was found on the clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum Clip {
    /// Plain text (empty if the clipboard held neither text nor an image).
    Text(String),
    /// An image (only when there was no non-blank text).
    Image(ClipImage),
    /// Contents a password manager marked as sensitive. The text itself is
    /// never read.
    Password,
    /// Only content of this kind was offered, and no selected profile
    /// handles it. Nothing was read.
    Unwanted(InputKind),
}

/// What to read from the clipboard: the size limit of each kind in bytes,
/// `None` for a kind no selected profile handles (it is then not read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest text, in UTF-8 (`max_input_bytes`).
    pub text: Option<usize>,
    /// Largest image as read (`max_image_bytes`).
    pub image: Option<usize>,
}

/// Image MIME types (Wayland) / targets (X11) we can read, most preferred
/// first: formats sent as-is before those that need converting.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
const IMAGE_MIME_PREFERENCE: [&str; 6] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
    "image/tiff",
];

/// macOS pasteboard type of plain text.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
const MACOS_TEXT_TYPE: &str = "public.utf8-plain-text";

/// macOS pasteboard image types we can read, most preferred first. Screen
/// shots are usually offered as PNG and TIFF.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
const MACOS_IMAGE_PREFERENCE: [&str; 3] = ["public.png", "public.jpeg", "public.tiff"];

/// macOS pasteboard type password managers add to mark a secret (see
/// nspasteboard.org).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
const MACOS_CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";

/// The first entry of `preference` that is among `offered`, if any.
#[cfg_attr(not(any(unix, test)), allow(dead_code))]
fn pick<'a>(preference: &[&'a str], offered: &[&str]) -> Option<&'a str> {
    preference
        .iter()
        .copied()
        .find(|wanted| offered.contains(wanted))
}

/// The image MIME type / X11 target to read among `offered`, if any.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
fn choose_image_type(offered: &[&str]) -> Option<&'static str> {
    pick(&IMAGE_MIME_PREFERENCE, offered)
}

/// Decide what to read and read it, the same way on every OS:
/// `text_offered`/`image_offered` say what the clipboard offers, the
/// reader functions read one kind with the given size limit (`Ok(None)`
/// if it turns out not to be available after all).
///
/// Non-blank text wins if it is wanted; otherwise a wanted image is read.
/// Content that is offered but not wanted, with nothing wanted to read
/// instead, is [`Clip::Unwanted`]. Nothing at all (or only blank text) is
/// an empty [`Clip::Text`].
fn choose<E>(
    text_offered: bool,
    image_offered: bool,
    limits: Limits,
    read_text: impl FnOnce(usize) -> Result<Option<String>, E>,
    read_image: impl FnOnce(usize) -> Result<Option<ClipImage>, E>,
) -> Result<Clip, E> {
    let image_limit = limits.image.filter(|_| image_offered);
    let mut text = String::new();
    if text_offered {
        match limits.text {
            Some(max) => {
                text = read_text(max)?.unwrap_or_default();
                if !text.trim().is_empty() {
                    return Ok(Clip::Text(text));
                }
            }
            None if image_limit.is_none() => return Ok(Clip::Unwanted(InputKind::Text)),
            None => {}
        }
    }
    match image_limit {
        Some(max) => Ok(read_image(max)?.map_or(Clip::Text(text), Clip::Image)),
        None if image_offered => Ok(Clip::Unwanted(InputKind::Image)),
        None => Ok(Clip::Text(text)),
    }
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

/// Read the current clipboard contents. Text or an image over its limit
/// is an error; it is never returned in part.
pub fn get(limits: Limits) -> Result<Clip, String> {
    imp::get(limits)
}

/// Replace the clipboard contents with `text`.
pub fn set_text(text: String) -> Result<(), String> {
    imp::set_text(text)
}

/// Run as the background clipboard owner (the hidden
/// `--clipboard-owner=...` mode; see [`crate::cli::OwnerBackend`]).
/// Returns the process exit status.
pub fn serve_owner(backend: crate::cli::OwnerBackend) -> i32 {
    imp::serve_owner(backend)
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

/// Error for the hidden `--clipboard-owner=...` mode on a platform
/// without a background clipboard owner helper.
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn no_owner_helper() -> i32 {
    eprintln!("The clipboard owner helper is only available on Linux/FreeBSD");
    2
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod owner;
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
    use super::{
        bytes_to_text, choose, choose_image_type, display_is_set, image_too_large, marks_password,
        owner, too_large, with_x11_fallback, x11, Clip, ClipImage, Limits,
    };
    use crate::cli::{OwnerBackend, OWNER_HELPER_WAYLAND_ARG};
    use std::io::Read;
    use wl_clipboard_rs::copy::{
        Error as CopyError, MimeType as CopyMimeType, Options, PreparedCopy, Source,
    };
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
    pub fn get(limits: Limits) -> Result<Clip, String> {
        finish_get(wayland_get(limits), have_display(), || x11::get(limits))
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

    /// A failed Wayland read: a Wayland error for [`finish_get`] to
    /// classify, or a final read/size error.
    enum ReadError {
        Wayland(PasteError),
        Final(String),
    }

    /// Check the offered MIME types for a password hint, then read what
    /// [`choose`] picks (see [`read_capped`]). Wayland errors are returned
    /// as-is (outer `Err`) for [`finish_get`] to classify; read/size errors
    /// are final (inner `Err`).
    fn wayland_get(limits: Limits) -> Result<Result<Clip, String>, PasteError> {
        let types = get_mime_types(ClipboardType::Regular, Seat::Unspecified)?;
        let offered: Vec<&str> = types.iter().map(String::as_str).collect();
        if marks_password(offered.iter().copied()) {
            return Ok(Ok(Clip::Password));
        }
        let image_type = choose_image_type(&offered);
        let text_offered = offered.iter().any(|m| wl_clipboard_rs::utils::is_text(m));
        let read_text = |max| {
            match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
                Ok((pipe, _)) => read_capped(pipe, max).map(Some).map_err(ReadError::Final),
                // No text type offered after all: an image may still be.
                Err(PasteError::NoMimeType) => Ok(None),
                Err(err) => Err(ReadError::Wayland(err)),
            }
        };
        let read_image = |max| {
            let Some(image_type) = image_type else {
                return Ok(None);
            };
            let (pipe, _) = get_contents(
                ClipboardType::Regular,
                Seat::Unspecified,
                MimeType::Specific(image_type),
            )
            .map_err(ReadError::Wayland)?;
            read_bytes_capped(pipe, max, image_too_large)
                .map(|bytes| Some(ClipImage::File(bytes)))
                .map_err(ReadError::Final)
        };
        match choose(
            text_offered,
            image_type.is_some(),
            limits,
            read_text,
            read_image,
        ) {
            Ok(clip) => Ok(Ok(clip)),
            Err(ReadError::Wayland(err)) => Err(err),
            Err(ReadError::Final(message)) => Ok(Err(message)),
        }
    }

    /// Read from `source` with a hard cap of `max_bytes + 1` bytes, so a
    /// huge or endless source cannot exhaust memory; more than `max_bytes`
    /// is the error `too_big(max_bytes)`.
    pub(super) fn read_bytes_capped(
        source: impl Read,
        max_bytes: usize,
        too_big: fn(usize) -> String,
    ) -> Result<Vec<u8>, String> {
        let mut contents = Vec::new();
        source
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut contents)
            .map_err(|e| format!("Failed to read clipboard contents: {e}"))?;
        if contents.len() > max_bytes {
            return Err(too_big(max_bytes));
        }
        Ok(contents)
    }

    /// Read text from `source` like [`read_bytes_capped`], with the text
    /// size limit.
    pub(super) fn read_capped(source: impl Read, max_bytes: usize) -> Result<String, String> {
        read_bytes_capped(source, max_bytes, too_large).and_then(|b| bytes_to_text(b, max_bytes))
    }

    /// Offer `text` as the new clipboard contents: start the background
    /// owner helper for Wayland ([`OWNER_HELPER_WAYLAND_ARG`]), falling
    /// back to the X11 one when Wayland clipboard access is unavailable
    /// (see [`wayland_owner_take`]).
    pub fn set_text(text: String) -> Result<(), String> {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                return Err(format!(
                    "Failed to set clipboard content: cannot find own executable: {e}"
                ))
            }
        };
        let command = owner::helper_command(exe, OWNER_HELPER_WAYLAND_ARG);
        let result = owner::hand_over(command, text.as_bytes());
        finish_set(result, have_display(), || x11::set_text(text))
    }

    /// Copy-side counterpart of [`finish_get`].
    pub(super) fn finish_set(
        wayland: owner::HandOverResult,
        display_set: bool,
        x11_op: impl FnOnce() -> Result<(), x11::Failure>,
    ) -> Result<(), String> {
        match wayland {
            owner::HandOverResult::Ok => Ok(()),
            owner::HandOverResult::Unavailable(err) => {
                with_x11_fallback("set", err, display_set, x11_op)
            }
            owner::HandOverResult::Failed(err) => {
                Err(format!("Failed to set clipboard content: {err}"))
            }
        }
    }

    /// The Wayland owner helper's `take` step: take the clipboard in
    /// foreground mode (so [`PreparedCopy::serve`] can be called
    /// afterwards), reporting [`owner::TakeFailure::Unavailable`] for the
    /// same errors [`copy_unavailable`] classifies as such.
    fn wayland_owner_take(data: Vec<u8>) -> Result<PreparedCopy, owner::TakeFailure> {
        let text = String::from_utf8(data)
            .map_err(|e| owner::TakeFailure::Failed(format!("text is not valid UTF-8: {e}")))?;
        let mut options = Options::new();
        options.foreground(true);
        let source = Source::Bytes(text.into_bytes().into());
        options
            .prepare_copy(source, CopyMimeType::Autodetect)
            .map_err(|err| {
                if copy_unavailable(&err) {
                    owner::TakeFailure::Unavailable(err.to_string())
                } else {
                    owner::TakeFailure::Failed(err.to_string())
                }
            })
    }

    /// Serve the clipboard as the background owner helper for `backend`.
    pub fn serve_owner(backend: OwnerBackend) -> i32 {
        match backend {
            OwnerBackend::X11 => x11::serve_owner(),
            OwnerBackend::Wayland => owner::run_owner_on_stdio(wayland_owner_take, |prepared| {
                prepared.serve().map_err(|e| e.to_string())
            }),
        }
    }
}

/// Registered Windows clipboard format names under which applications
/// offer PNG data, most common first.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const WINDOWS_PNG_FORMATS: [&str; 2] = ["PNG", "image/png"];

#[cfg(target_os = "windows")]
mod imp {
    use super::{
        check_text_size, choose, image_too_large, too_large, windows_marks_password, Clip,
        ClipImage, Limits, WINDOWS_EXCLUDE_FORMATS, WINDOWS_HISTORY_FORMAT, WINDOWS_PNG_FORMATS,
    };
    use clipboard_win::{formats, raw, Clipboard, Getter};

    /// Open the clipboard, check the password-manager markers, and read
    /// what [`choose`] picks (images via [`read_image`]). The text size is
    /// checked before copying: UTF-16 takes at most twice the bytes of the
    /// same text in UTF-8, so a UTF-16 buffer over `2 * (max_bytes + 1)`
    /// bytes (incl. the terminator) is certainly too large.
    pub fn get(limits: Limits) -> Result<Clip, String> {
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

        let image_format = image_format();
        let read_text = |max: usize| {
            if let Some(size) = raw::size(formats::CF_UNICODETEXT) {
                if size.get() / 2 > max + 1 {
                    return Err(too_large(max));
                }
            }
            let mut text = String::new();
            formats::Unicode
                .read_clipboard(&mut text)
                .map_err(|e| format!("Failed to get clipboard content: {e}"))?;
            check_text_size(text, max).map(Some)
        };
        let read_image = |max| match image_format {
            Some(image) => read_image(image, max).map(Some),
            None => Ok(None),
        };
        choose(
            raw::is_format_avail(formats::CF_UNICODETEXT),
            image_format.is_some(),
            limits,
            read_text,
            read_image,
        )
    }

    /// An image format on the clipboard: registered PNG data, or a bitmap
    /// (`CF_DIB`, which Windows also synthesises from `CF_BITMAP` and
    /// `CF_DIBV5`).
    #[derive(Clone, Copy)]
    enum ImageFormat {
        Png(u32),
        Dib,
    }

    /// The best image format available, PNG preferred.
    fn image_format() -> Option<ImageFormat> {
        WINDOWS_PNG_FORMATS
            .iter()
            .filter_map(|name| raw::register_format(name))
            .map(|f| f.get())
            .find(|&f| raw::is_format_avail(f))
            .map(ImageFormat::Png)
            .or_else(|| raw::is_format_avail(formats::CF_DIB).then_some(ImageFormat::Dib))
    }

    /// Read `format` (size checked against `max_bytes` before copying).
    fn read_image(format: ImageFormat, max_bytes: usize) -> Result<ClipImage, String> {
        let id = match format {
            ImageFormat::Png(id) => id,
            ImageFormat::Dib => formats::CF_DIB,
        };
        if raw::size(id).is_some_and(|size| size.get() > max_bytes) {
            return Err(image_too_large(max_bytes));
        }
        let mut bytes = Vec::new();
        raw::get_vec(id, &mut bytes).map_err(|e| format!("Failed to get clipboard image: {e}"))?;
        if bytes.len() > max_bytes {
            return Err(image_too_large(max_bytes));
        }
        Ok(match format {
            ImageFormat::Png(_) => ClipImage::File(bytes),
            ImageFormat::Dib => ClipImage::Dib(bytes),
        })
    }

    /// Replace the clipboard contents with `text`.
    pub fn set_text(text: String) -> Result<(), String> {
        clipboard_win::set_clipboard_string(&text)
            .map_err(|e| format!("Failed to set clipboard content: {e}"))
    }

    /// The owner helper does not exist on Windows (the clipboard holds
    /// data itself, so nothing needs to keep running to serve it).
    pub fn serve_owner(_backend: crate::cli::OwnerBackend) -> i32 {
        super::no_owner_helper()
    }
}

/// What to read from the macOS pasteboard, decided from its type names.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
enum MacPlan {
    /// A password manager marked the contents; do not read them.
    Password,
    /// Read the text if `text` is set, and fall back to this image type.
    Read {
        text: bool,
        image: Option<&'static str>,
    },
}

/// Decide what to read from the pasteboard's offered `types`.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn plan_macos(types: &[&str]) -> MacPlan {
    if types.contains(&MACOS_CONCEALED_TYPE) {
        return MacPlan::Password;
    }
    MacPlan::Read {
        text: types.contains(&MACOS_TEXT_TYPE),
        image: pick(&MACOS_IMAGE_PREFERENCE, types),
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        check_text_size, choose, image_too_large, plan_macos, Clip, ClipImage, Limits, MacPlan,
        MACOS_TEXT_TYPE,
    };
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;

    /// Read the general pasteboard: password marker first, then what
    /// [`choose`] picks. The image size is checked before copying its
    /// bytes.
    pub fn get(limits: Limits) -> Result<Clip, String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let types: Vec<String> = pasteboard
            .types()
            .map(|types| types.iter().map(|t| t.to_string()).collect())
            .unwrap_or_default();
        let types: Vec<&str> = types.iter().map(String::as_str).collect();
        let (want_text, image) = match plan_macos(&types) {
            MacPlan::Password => return Ok(Clip::Password),
            MacPlan::Read { text, image } => (text, image),
        };
        let read_text = |max| match pasteboard.stringForType(&NSString::from_str(MACOS_TEXT_TYPE)) {
            Some(string) => check_text_size(string.to_string(), max).map(Some),
            None => Ok(None),
        };
        let read_image = |max| {
            let Some(data) = image.and_then(|t| pasteboard.dataForType(&NSString::from_str(t)))
            else {
                return Ok(None);
            };
            if data.length() > max {
                return Err(image_too_large(max));
            }
            Ok(Some(ClipImage::File(data.to_vec())))
        };
        choose(want_text, image.is_some(), limits, read_text, read_image)
    }

    /// Replace the pasteboard contents with `text`.
    pub fn set_text(text: String) -> Result<(), String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let written = pasteboard.setString_forType(
            &NSString::from_str(&text),
            &NSString::from_str(MACOS_TEXT_TYPE),
        );
        if written {
            Ok(())
        } else {
            Err("Failed to set clipboard content".to_string())
        }
    }

    /// The owner helper does not exist on macOS (the clipboard holds
    /// data itself, so nothing needs to keep running to serve it).
    pub fn serve_owner(_backend: crate::cli::OwnerBackend) -> i32 {
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
        let image = || Clip::Image(ClipImage::File(b"\x89PNG".to_vec()));
        assert_eq!(imp::finish_get(Ok(Ok(image())), true, never), Ok(image()));
        assert_eq!(
            imp::finish_get(Err(Error::NoMimeType), true, never),
            Ok(Clip::Text(String::new()))
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
        let never = || -> Result<(), x11::Failure> { panic!("no fallback expected") };
        assert_eq!(
            imp::finish_set(owner::HandOverResult::Ok, true, never),
            Ok(())
        );
        assert_eq!(
            imp::finish_set(
                owner::HandOverResult::Failed("no seats".into()),
                true,
                never
            ),
            Err("Failed to set clipboard content: no seats".into())
        );
        assert_eq!(
            imp::finish_set(
                owner::HandOverResult::Unavailable("no protocol".into()),
                true,
                || Ok(())
            ),
            Ok(())
        );
        let err = imp::finish_set(
            owner::HandOverResult::Unavailable("no protocol".into()),
            true,
            || Err(x11::Failure::Connect("refused".into())),
        )
        .unwrap_err();
        assert!(err.contains("Wayland: no protocol"), "{err}");
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

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn read_bytes_capped_uses_given_error() {
        assert_eq!(
            imp::read_bytes_capped(&b"\x89PNG"[..], 3, image_too_large),
            Err(image_too_large(3))
        );
        assert_eq!(
            imp::read_bytes_capped(&b"\x89PNG"[..], 4, image_too_large),
            Ok(b"\x89PNG".to_vec())
        );
        assert!(image_too_large(4).contains("max_image_bytes"));
    }

    /// Run [`choose`] on a fake clipboard offering `text` and/or an image
    /// (`image`), with text/image wanted as given (limit 100). Returns the
    /// result and which kinds were read.
    fn fake_choose(
        text: Option<&str>,
        image: bool,
        want_text: bool,
        want_image: bool,
    ) -> (Clip, Vec<&'static str>) {
        let read = std::cell::RefCell::new(Vec::new());
        let limits = Limits {
            text: want_text.then_some(100),
            image: want_image.then_some(100),
        };
        let clip = choose::<String>(
            text.is_some(),
            image,
            limits,
            |max| {
                assert_eq!(max, 100);
                read.borrow_mut().push("text");
                Ok(text.map(str::to_string))
            },
            |max| {
                assert_eq!(max, 100);
                read.borrow_mut().push("image");
                Ok(Some(ClipImage::File(b"img".to_vec())))
            },
        )
        .unwrap();
        (clip, read.into_inner())
    }

    #[test]
    fn choose_prefers_text_when_both_are_wanted() {
        let img = || Clip::Image(ClipImage::File(b"img".to_vec()));
        let text = |t: &str| Clip::Text(t.into());
        assert_eq!(
            fake_choose(Some("hi"), true, true, true),
            (text("hi"), vec!["text"])
        );
        assert_eq!(
            fake_choose(Some(" \n"), true, true, true),
            (img(), vec!["text", "image"])
        );
        assert_eq!(fake_choose(None, true, true, true), (img(), vec!["image"]));
        assert_eq!(
            fake_choose(Some("hi"), false, true, true),
            (text("hi"), vec!["text"])
        );
        assert_eq!(
            fake_choose(Some(" "), false, true, true),
            (text(" "), vec!["text"])
        );
        assert_eq!(fake_choose(None, false, true, true), (text(""), vec![]));
    }

    #[test]
    fn choose_reads_only_wanted_kinds() {
        let img = || Clip::Image(ClipImage::File(b"img".to_vec()));
        let unwanted = Clip::Unwanted;
        // Image-only selection: the image is read even with text offered.
        assert_eq!(
            fake_choose(Some("hi"), true, false, true),
            (img(), vec!["image"])
        );
        assert_eq!(
            fake_choose(Some("hi"), false, false, true),
            (unwanted(InputKind::Text), vec![])
        );
        // Text-only selection: an image is never read.
        assert_eq!(
            fake_choose(None, true, true, false),
            (unwanted(InputKind::Image), vec![])
        );
        assert_eq!(
            fake_choose(Some(""), true, true, false),
            (unwanted(InputKind::Image), vec!["text"])
        );
        assert_eq!(
            fake_choose(Some("hi"), true, true, false),
            (Clip::Text("hi".into()), vec!["text"])
        );
        // Nothing offered is empty text whatever is wanted.
        assert_eq!(
            fake_choose(None, false, false, true),
            (Clip::Text(String::new()), vec![])
        );
    }

    #[test]
    fn choose_falls_back_when_a_read_finds_nothing_and_passes_errors() {
        let limits = Limits {
            text: Some(5),
            image: Some(7),
        };
        let gone = choose::<String>(true, true, limits, |_| Ok(None), |_| Ok(None));
        assert_eq!(gone, Ok(Clip::Text(String::new())));
        let err = choose(true, true, limits, |max| Err(too_large(max)), |_| Ok(None));
        assert_eq!(err, Err(too_large(5)));
        let err = choose(
            false,
            true,
            limits,
            |_| Ok(None),
            |max| Err(image_too_large(max)),
        );
        assert_eq!(err, Err(image_too_large(7)));
    }

    #[test]
    fn image_type_preference() {
        let offered = ["text/html", "image/bmp", "image/png", "image/webp"];
        assert_eq!(choose_image_type(&offered), Some("image/png"));
        assert_eq!(
            choose_image_type(&["image/gif", "image/webp"]),
            Some("image/webp")
        );
        assert_eq!(choose_image_type(&["image/tiff"]), Some("image/tiff"));
        assert_eq!(choose_image_type(&["image/svg+xml", "text/plain"]), None);
        assert_eq!(choose_image_type(&[]), None);
    }

    #[test]
    fn macos_plan() {
        assert_eq!(
            plan_macos(&["public.utf8-plain-text", "org.nspasteboard.ConcealedType"]),
            MacPlan::Password
        );
        assert_eq!(
            plan_macos(&["public.tiff", "public.png", "public.utf8-plain-text"]),
            MacPlan::Read {
                text: true,
                image: Some("public.png")
            }
        );
        assert_eq!(
            plan_macos(&["public.tiff"]),
            MacPlan::Read {
                text: false,
                image: Some("public.tiff")
            }
        );
        assert_eq!(
            plan_macos(&["com.adobe.pdf"]),
            MacPlan::Read {
                text: false,
                image: None
            }
        );
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
