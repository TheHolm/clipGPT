//! Per-OS clipboard access.
//!
//! Clipboard access is OS-specific rather than going through a single
//! cross-platform abstraction, because the previous one
//! (`clipboard`/`x11-clipboard`, built on the `xcb` crate) is unmaintained
//! since 2022 and has real, published soundness advisories (e.g.
//! RUSTSEC-2021-0019). See README.md "Clipboard backend" for the full
//! reasoning, including why Linux/FreeBSD are Wayland-only now (X11 is
//! deliberately not supported).

/// Read the current clipboard contents as text.
pub fn get_text() -> String {
    imp::get_text()
}

/// Replace the clipboard contents with `text`.
pub fn set_text(text: String) {
    imp::set_text(text)
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod imp {
    use std::io::Read;
    use wl_clipboard_rs::copy::{MimeType as CopyMimeType, Options, Source};
    use wl_clipboard_rs::paste::{
        get_contents, ClipboardType, Error as PasteError, MimeType, Seat,
    };

    pub fn get_text() -> String {
        match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
            Ok((mut pipe, _)) => {
                let mut contents = Vec::new();
                pipe.read_to_end(&mut contents)
                    .expect("failed to read clipboard contents");
                String::from_utf8_lossy(&contents).into_owned()
            }
            // No text on the clipboard right now - treat the same as empty,
            // matching the other backends' behavior.
            Err(PasteError::NoSeats)
            | Err(PasteError::ClipboardEmpty)
            | Err(PasteError::NoMimeType) => String::new(),
            Err(err) => panic!("Failed to get clipboard content: {err}"),
        }
    }

    pub fn set_text(text: String) {
        Options::new()
            .copy(
                Source::Bytes(text.into_bytes().into()),
                CopyMimeType::Autodetect,
            )
            .expect("Failed to set clipboard content");
    }
}

#[cfg(target_os = "windows")]
mod imp {
    pub fn get_text() -> String {
        clipboard_win::get_clipboard_string().expect("Failed to get clipboard content")
    }

    pub fn set_text(text: String) {
        clipboard_win::set_clipboard_string(&text).expect("Failed to set clipboard content");
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use clipboard::{ClipboardContext, ClipboardProvider};

    pub fn get_text() -> String {
        let mut ctx: ClipboardContext =
            ClipboardProvider::new().expect("Failed to initialize clipboard");
        ctx.get_contents().expect("Failed to get clipboard content")
    }

    pub fn set_text(text: String) {
        let mut ctx: ClipboardContext =
            ClipboardProvider::new().expect("Failed to initialize clipboard");
        ctx.set_contents(text)
            .expect("Failed to set clipboard content");
    }
}
