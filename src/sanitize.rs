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
//! * other invisible format characters and fillers: soft hyphen, combining
//!   grapheme joiner, Hangul fillers, Khmer inherent vowels, Mongolian
//!   variation selectors and vowel separator, deprecated format controls
//!   (U+206A-U+206F), interlinear annotation marks and musical format
//!   controls - all can carry text a reader does not see
//! * Unicode tag characters (U+E0000-U+E007F), a known channel for hiding
//!   text from humans while still being read by LLMs
//! * hidden variation selectors: every supplementary one
//!   (U+E0100-U+E01EF) and every one (U+FE00-U+FE0F) directly following
//!   another ("emoji smuggling" hides bytes as runs of selectors behind one
//!   visible character). A single U+FE00-U+FE0F after a character is
//!   normal (e.g. U+FE0F for emoji presentation) and is kept.
//!
//! The line and paragraph separators U+2028/U+2029 become `\n`.
//!
//! On input, tag characters and hidden variation selectors are not
//! stripped but treated as an attack (see [`contains_hidden_chars`]): the
//! caller refuses to send such text at all.
//!
//! Sanitising limits the damage of a prompt injection; it cannot prevent
//! one. Instructions written in plain visible text get through untouched,
//! and the model can still produce plausible-looking malicious plain text.

/// Normalise line endings (including U+2028/U+2029) to `\n`, remove every
/// character for which [`is_unsafe_char`] is true, and remove hidden
/// variation selectors (see [`visible_chars`]).
fn clean(text: &str) -> String {
    let normalised = text
        .replace("\r\n", "\n")
        .replace(['\r', '\u{2028}', '\u{2029}'], "\n");
    let mut out = String::with_capacity(normalised.len());
    for (c, hidden_selector) in visible_chars(&normalised) {
        if !hidden_selector {
            out.push(c);
        }
    }
    out
}

/// The characters of `text` that survive [`is_unsafe_char`], each paired
/// with whether it is a hidden variation selector: a supplementary one
/// (U+E0100-U+E01EF), or a U+FE00-U+FE0F that directly follows another
/// variation selector. Removed characters are skipped before that check,
/// so a run split up by zero-width characters still counts as a run.
fn visible_chars(text: &str) -> impl Iterator<Item = (char, bool)> + '_ {
    let mut previous_was_selector = false;
    text.chars().filter(|&c| !is_unsafe_char(c)).map(move |c| {
        let selector = is_variation_selector(c);
        let hidden = is_supplementary_selector(c) || (selector && previous_was_selector);
        previous_was_selector = selector;
        (c, hidden)
    })
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
/// characters and hidden variation selectors first
/// ([`contains_hidden_chars`]); any left are removed here anyway.
pub fn sanitize_input(text: &str) -> String {
    clean(text)
}

/// True if `text` contains Unicode tag characters (U+E0000-U+E007F) or
/// hidden variation selectors (see [`visible_chars`]). Both are invisible
/// and are known ways to hide instructions or data from a human reader
/// while an LLM still reads them, so clipboard text containing them is
/// treated as an attack. The only legitimate use of tag characters is the
/// England, Scotland and Wales flag emoji, which will therefore be refused
/// too; supplementary variation selectors are only used by rare CJK
/// ideograph variants.
pub fn contains_hidden_chars(text: &str) -> bool {
    text.chars().any(is_tag_char) || visible_chars(text).any(|(_, hidden)| hidden)
}

/// True if `c` must be removed from LLM output. `\n` and `\t` are the only
/// control characters that are kept.
fn is_unsafe_char(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return false;
    }
    // char::is_control covers exactly the C0 (U+0000-U+001F), DEL (U+007F)
    // and C1 (U+0080-U+009F) ranges.
    c.is_control()
        || is_bidi_control(c)
        || is_invisible(c)
        || is_invisible_format(c)
        || is_tag_char(c)
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

/// Other characters that render as nothing (or as blank space that is not
/// white space) and so can hide text: soft hyphen (U+00AD), combining
/// grapheme joiner (U+034F), Hangul fillers (U+115F, U+1160, U+3164,
/// U+FFA0), Khmer inherent vowels (U+17B4, U+17B5), Mongolian free
/// variation selectors and vowel separator (U+180B-U+180F), deprecated
/// format characters (U+206A-U+206F), interlinear annotation marks
/// (U+FFF9-U+FFFB) and musical symbol format controls (U+1D173-U+1D17A).
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{206A}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1D173}'..='\u{1D17A}'
    )
}

/// Unicode tag characters (U+E0000-U+E007F).
fn is_tag_char(c: char) -> bool {
    matches!(c, '\u{E0000}'..='\u{E007F}')
}

/// Any variation selector: U+FE00-U+FE0F or U+E0100-U+E01EF.
fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}') || is_supplementary_selector(c)
}

/// Supplementary variation selectors (U+E0100-U+E01EF).
fn is_supplementary_selector(c: char) -> bool {
    matches!(c, '\u{E0100}'..='\u{E01EF}')
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
    fn strips_other_invisible_format_characters() {
        let text = "a\u{00AD}b\u{034F}c\u{115F}\u{1160}d\u{17B4}\u{17B5}e\u{180B}\u{180E}\u{180F}f\
                    \u{206A}\u{206F}g\u{3164}h\u{FFA0}i\u{FFF9}j\u{FFFA}\u{FFFB}k\u{1D173}\u{1D17A}l";
        assert_eq!(sanitize_output(text), "abcdefghijkl");
        assert_eq!(sanitize_input(text), "abcdefghijkl");
    }

    #[test]
    fn line_and_paragraph_separators_become_newlines() {
        assert_eq!(sanitize_output("a\u{2028}b\u{2029}c"), "a\nb\nc");
        assert_eq!(sanitize_input("a\u{2028}b"), "a\nb");
    }

    /// `n` bytes hidden as variation selectors after a visible "x", the
    /// way emoji smuggling encodes them.
    fn smuggled(bytes: &[u8]) -> String {
        let mut s = String::from("x");
        for &b in bytes {
            let c = if b < 16 {
                0xFE00 + b as u32
            } else {
                0xE0100 + (b as u32 - 16)
            };
            s.push(char::from_u32(c).unwrap());
        }
        s
    }

    #[test]
    fn single_variation_selector_after_a_character_is_kept() {
        for text in [
            "ok \u{2764}\u{FE0F}",
            "\u{FE0F}",
            "a\u{FE0E} b\u{FE0F}",
            "\u{263A}\u{FE0F}\n",
        ] {
            assert_eq!(sanitize_input(text), text, "{text:?}");
            assert!(!contains_hidden_chars(text), "{text:?}");
        }
        assert_eq!(
            sanitize_output("ok \u{2764}\u{FE0F}"),
            "ok \u{2764}\u{FE0F}"
        );
    }

    #[test]
    fn variation_selector_runs_are_detected_and_stripped() {
        let text = format!("hi {}", smuggled(&[1, 2, 3]));
        assert!(contains_hidden_chars(&text));
        // The first selector after the "x" is the normal kind and stays.
        assert_eq!(sanitize_output(&text), "hi x\u{FE01}");
        assert_eq!(sanitize_input(&text), "hi x\u{FE01}");
        assert!(!contains_hidden_chars(&sanitize_input(&text)));
    }

    #[test]
    fn supplementary_variation_selectors_are_detected_and_stripped() {
        let text = smuggled(&[200]);
        assert!(contains_hidden_chars(&text));
        assert_eq!(sanitize_output(&text), "x");
        assert!(contains_hidden_chars("\u{E01EF}"));
        assert!(!contains_hidden_chars("\u{E01F0}"));
    }

    #[test]
    fn run_split_by_removed_characters_is_still_a_run() {
        let text = "x\u{FE01}\u{200B}\u{FE02}\u{00AD}\u{FE03}";
        assert!(contains_hidden_chars(text));
        assert_eq!(sanitize_output(text), "x\u{FE01}");
    }

    #[test]
    fn detects_tag_characters() {
        let hidden: String = "do evil"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        assert!(contains_hidden_chars(&format!("innocent{hidden}")));
        assert!(contains_hidden_chars("\u{E0001}"));
        assert!(contains_hidden_chars("\u{E007F}"));
        assert!(!contains_hidden_chars(
            "plain text \u{200B} \u{FE0F} 日本語"
        ));
        assert!(!contains_hidden_chars("\u{E0080}"));
    }
}
