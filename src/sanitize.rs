use std::borrow::Cow;

pub const REPLACEMENT: char = '\u{fffd}';

pub fn is_forbidden_control(ch: char) -> bool {
    match ch {
        '\n' | '\t' => false,
        '\r' => true,
        ch if (ch as u32) < 0x20 => true,
        '\u{7f}' => true,
        '\u{80}'..='\u{9f}' => true,
        '\u{200b}' | '\u{200c}' | '\u{2060}' => true,
        '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => true,
        _ => false,
    }
}

/// Replaces terminal control characters so provider and child-process text cannot
/// drive the host terminal. Runs on decoded scalars, so UTF-8 stays valid.
pub fn sanitize_terminal_text(input: &str) -> Cow<'_, str> {
    if !input.chars().any(is_forbidden_control) {
        return Cow::Borrowed(input);
    }
    Cow::Owned(
        input
            .chars()
            .map(|ch| {
                if is_forbidden_control(ch) {
                    REPLACEMENT
                } else {
                    ch
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_borrowed_unchanged() {
        let text = "hello world";
        assert!(matches!(sanitize_terminal_text(text), Cow::Borrowed(_)));
        assert_eq!(sanitize_terminal_text(text), text);
    }

    #[test]
    fn newline_and_tab_survive() {
        assert_eq!(sanitize_terminal_text("a\n\tb"), "a\n\tb");
    }

    #[test]
    fn escape_introducers_are_replaced_so_no_sequence_can_start() {
        assert_eq!(
            sanitize_terminal_text("a\u{1b}]0;title\u{7}b"),
            "a\u{fffd}]0;title\u{fffd}b"
        );
        assert_eq!(sanitize_terminal_text("\u{1b}[31mred"), "\u{fffd}[31mred");
        assert_eq!(
            sanitize_terminal_text("\u{1b}Pq#0\u{1b}\\"),
            "\u{fffd}Pq#0\u{fffd}\\"
        );
    }

    #[test]
    fn bell_and_other_c0_are_replaced() {
        assert_eq!(sanitize_terminal_text("ding\u{7}"), "ding\u{fffd}");
        assert_eq!(
            sanitize_terminal_text("a\u{0}b\u{1f}c"),
            "a\u{fffd}b\u{fffd}c"
        );
        assert_eq!(sanitize_terminal_text("a\rb"), "a\u{fffd}b");
    }

    #[test]
    fn del_and_c1_are_replaced() {
        assert_eq!(sanitize_terminal_text("a\u{7f}b"), "a\u{fffd}b");
        assert_eq!(sanitize_terminal_text("a\u{9b}31mb"), "a\u{fffd}31mb");
    }

    #[test]
    fn bidi_and_invisible_controls_are_replaced_but_zwj_survives() {
        assert_eq!(
            sanitize_terminal_text("\u{202e}reversed"),
            "\u{fffd}reversed"
        );
        assert_eq!(sanitize_terminal_text("a\u{200b}b"), "a\u{fffd}b");
        let family = "\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(sanitize_terminal_text(family), family);
    }

    #[test]
    fn utf8_and_emoji_are_not_split() {
        let text = "héllo 世界 🚀";
        assert_eq!(sanitize_terminal_text(text), text);
        let mixed = format!("{text}\u{1b}[0m");
        let cleaned = sanitize_terminal_text(&mixed);
        assert!(cleaned.starts_with(text));
        assert!(cleaned.is_char_boundary(cleaned.len()));
    }
}
