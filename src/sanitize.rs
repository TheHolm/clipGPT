//! Cleaning of clipboard input before it is sent to the LLM, and of LLM
//! output before it is put onto the clipboard.
//!
//! Both directions are untrusted: clipboard text copied from a web page or
//! document can carry a hidden prompt injection, and the model's output is
//! then pasted by the user - often straight into a terminal, editor or chat
//! window without being read closely. This module removes characters that
//! are invisible or that can change how surrounding text is displayed or
//! interpreted:
//!
//! * C0/C1 control characters (except `\n` and `\t`) - this covers ESC, so
//!   terminal escape sequences cannot survive, and stray `\r` that can hide
//!   text on a terminal line
//! * bidirectional formatting characters ("Trojan Source", CVE-2021-42574)
//! * zero-width characters and the byte order mark (side effect: emoji
//!   built from several code points joined by U+200D fall apart into their
//!   individual emoji)
//! * Unicode tag characters (U+E0000-U+E007F), a known channel for hiding
//!   text from humans while still being read by LLMs
//!
//! On input, tag characters are not stripped but treated as an attack (see
//! [`contains_tag_chars`]): the caller refuses to send such text at all.
//!
//! Sanitising limits the damage of a prompt injection; it cannot prevent
//! one. Instructions written in plain visible text get through untouched,
//! and the model can still produce plausible-looking malicious plain text.

/// Normalise line endings to `\n` and remove every character for which
/// [`is_unsafe_char`] is true.
fn clean(text: &str) -> String {
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    normalised.chars().filter(|&c| !is_unsafe_char(c)).collect()
}

/// Return LLM output with line endings normalised to `\n`, dangerous or
/// invisible characters removed (see the module docs), and surrounding
/// whitespace trimmed.
pub fn sanitize_output(text: &str) -> String {
    clean(text).trim().to_string()
}

/// Return clipboard text cleaned the same way as output, before it is sent
/// to the LLM (or scanned for secrets). Surrounding whitespace is kept, since
/// it may matter to the user's request. Callers must reject text with tag
/// characters first ([`contains_tag_chars`]); any left are removed here
/// anyway.
pub fn sanitize_input(text: &str) -> String {
    clean(text)
}

/// True if `text` contains Unicode tag characters (U+E0000-U+E007F). These
/// are invisible and are the standard way to hide instructions from a human
/// reader while an LLM still reads them, so clipboard text containing them
/// is treated as an attack. Their only legitimate use is the England,
/// Scotland and Wales flag emoji, which will therefore be refused too.
pub fn contains_tag_chars(text: &str) -> bool {
    text.chars().any(is_tag_char)
}

/// True if `c` must be removed from LLM output. `\n` and `\t` are the only
/// control characters that are kept.
fn is_unsafe_char(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return false;
    }
    // char::is_control covers exactly the C0 (U+0000-U+001F), DEL (U+007F)
    // and C1 (U+0080-U+009F) ranges.
    c.is_control() || is_bidi_control(c) || is_invisible(c) || is_tag_char(c)
}

/// Bidirectional formatting characters: embeddings/overrides
/// (U+202A-U+202E), isolates (U+2066-U+2069), and the implicit direction
/// marks LRM, RLM and ALM.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

/// Zero-width space/joiners, word joiner, invisible math operators
/// (U+2061-U+2064), and the byte order mark / zero-width no-break space.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
}

/// Unicode tag characters (U+E0000-U+E007F).
fn is_tag_char(c: char) -> bool {
    matches!(c, '\u{E0000}'..='\u{E007F}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_untouched() {
        let text = "Arr, ye scurvy dog!\n\tSecond line with ünïcödé and 日本語.";
        assert_eq!(sanitize_output(text), text);
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(sanitize_output("  \n hello \n\n"), "hello");
    }

    #[test]
    fn normalises_line_endings() {
        assert_eq!(sanitize_output("a\r\nb\rc\nd"), "a\nb\nc\nd");
    }

    #[test]
    fn strips_terminal_escape_sequences() {
        // ESC is removed, so the sequence can no longer be interpreted.
        let out = sanitize_output("safe\x1b[2K\x1b[1Ahidden");
        assert!(!out.contains('\x1b'));
        assert_eq!(out, "safe[2K[1Ahidden");
    }

    #[test]
    fn strips_c0_c1_and_del() {
        assert_eq!(
            sanitize_output("a\x00b\x07c\x08d\x7fe\u{0085}f\u{009B}g"),
            "abcdefg"
        );
    }

    #[test]
    fn strips_bidi_controls() {
        let text = "a\u{202A}b\u{202B}c\u{202C}d\u{202D}e\u{202E}f\u{2066}g\u{2067}h\u{2068}i\u{2069}j\u{200E}k\u{200F}l\u{061C}m";
        assert_eq!(sanitize_output(text), "abcdefghijklm");
    }

    #[test]
    fn strips_zero_width_and_bom() {
        let text = "\u{FEFF}a\u{200B}b\u{200C}c\u{200D}d\u{2060}e\u{2062}f";
        assert_eq!(sanitize_output(text), "abcdef");
    }

    #[test]
    fn strips_tag_characters() {
        let hidden: String = "ignore"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        let text = format!("visible\u{E0001}{hidden}\u{E007F}");
        assert_eq!(sanitize_output(&text), "visible");
    }

    #[test]
    fn keeps_newlines_and_tabs() {
        assert_eq!(sanitize_output("a\n\tb"), "a\n\tb");
    }

    #[test]
    fn input_is_cleaned_but_not_trimmed() {
        let text = "  fix\u{200B} th\u{202E}is\x1b[0m\r\n  ";
        assert_eq!(sanitize_input(text), "  fix this[0m\n  ");
    }

    #[test]
    fn input_plain_text_is_untouched() {
        let text = "Hello,\n\twörld! 日本語\n";
        assert_eq!(sanitize_input(text), text);
    }

    #[test]
    fn detects_tag_characters() {
        let hidden: String = "do evil"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        assert!(contains_tag_chars(&format!("innocent{hidden}")));
        assert!(contains_tag_chars("\u{E0001}"));
        assert!(contains_tag_chars("\u{E007F}"));
        assert!(!contains_tag_chars("plain text \u{200B} \u{FE0F} 日本語"));
        assert!(!contains_tag_chars("\u{E0080}"));
    }

    #[test]
    fn keeps_variation_selectors() {
        // Variation selectors are not on the strip list.
        let text = "ok \u{2764}\u{FE0F}";
        assert_eq!(sanitize_output(text), text);
    }
}
