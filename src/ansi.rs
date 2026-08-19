//! ANSI handling for the pane preview.
//!
//! Panes are captured with `capture-pane -e`, so the capture carries the
//! agent's own escape sequences. Two consumers pull them apart:
//!
//!   - [`strip`] gives the plain text — what status classification matches
//!     its patterns against, and what the `none` preview mode renders.
//!   - [`styled_text`] parses the escapes into a ratatui [`Text`], honouring
//!     the configured [`PreviewColors`] mode.
//!
//! Everything here is tolerant by design: a capture is terminal output, not
//! a protocol we control, so a malformed sequence degrades to plain text
//! instead of erroring.

use ansi_to_tui::IntoText as _;
use ratatui::style::Style;
use ratatui::text::Text;

use crate::config::PreviewColors;

/// Remove ANSI escape sequences, keeping the visible text verbatim.
///
/// Handles the two shapes tmux emits: CSI (`ESC [ … final`, the SGR colour
/// and attribute codes) and OSC (`ESC ] … BEL`/`ST`, used for hyperlinks and
/// title setting). Any other `ESC x` pair drops both bytes. A truncated
/// sequence at the end of the string drops what is left of it.
pub fn strip(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameter/intermediate bytes until a final byte @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: terminated by BEL or ST (ESC \)
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Any other two-byte escape (or a trailing lone ESC).
            _ => {}
        }
    }
    out
}

/// True when a line holds no visible characters — blanks, or nothing but
/// escape sequences. `capture-pane` pads a pane's empty area with such
/// lines, and with `-e` the padding can carry the pane's colours.
pub fn is_blank(line: &str) -> bool {
    strip(line).trim().is_empty()
}

/// The capture rendered for the preview panel, per the configured mode.
///
/// `Full` keeps every colour the pane emitted, `Foreground` drops the
/// background colours only, `None` throws all styling away. A capture that
/// fails to parse (invalid UTF-8, a sequence the parser rejects) falls back
/// to plain text rather than blanking the panel.
pub fn styled_text(capture: &str, mode: PreviewColors) -> Text<'static> {
    if mode == PreviewColors::None {
        return Text::from(strip(capture));
    }
    let Ok(mut text) = capture.into_text() else {
        return Text::from(strip(capture));
    };
    if mode == PreviewColors::Foreground {
        drop_backgrounds(&mut text);
    }
    text
}

/// Clear every background colour in a parsed capture, keeping foreground
/// colours and modifiers (`foreground` mode).
fn drop_backgrounds(text: &mut Text<'_>) {
    let clear = |style: &mut Style| style.bg = None;
    clear(&mut text.style);
    for line in &mut text.lines {
        clear(&mut line.style);
        for span in &mut line.spans {
            clear(&mut span.style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn strip_removes_sgr_and_keeps_text_and_newlines() {
        assert_eq!(
            strip("\x1b[31mRED\x1b[39m \x1b[1;32mBOLD\x1b[0m plain"),
            "RED BOLD plain"
        );
        assert_eq!(strip("a\n\x1b[44mb\x1b[49m\nc"), "a\nb\nc");
        assert_eq!(strip("no escapes here"), "no escapes here");
    }

    #[test]
    fn strip_handles_osc_and_truncated_sequences() {
        // OSC 8 hyperlink, BEL-terminated and ST-terminated
        assert_eq!(strip("\x1b]8;;https://x\x07link\x1b]8;;\x07"), "link");
        assert_eq!(strip("\x1b]0;title\x1b\\text"), "text");
        // a capture cut mid-sequence must not leak the fragment
        assert_eq!(strip("text\x1b[3"), "text");
        assert_eq!(strip("text\x1b"), "text");
    }

    #[test]
    fn blank_lines_stay_blank_through_escapes() {
        assert!(is_blank(""));
        assert!(is_blank("   "));
        assert!(is_blank("\x1b[49m\x1b[39m  "));
        assert!(!is_blank("\x1b[31mx\x1b[0m"));
    }

    /// The three modes over one capture: full keeps both colours, foreground
    /// keeps the text colour and drops the background, none keeps neither.
    #[test]
    fn modes_keep_full_foreground_or_no_colours() {
        let cap = "\x1b[31;44mred on blue\x1b[0m";

        let full = styled_text(cap, PreviewColors::Full);
        let span = &full.lines[0].spans[0];
        assert_eq!(span.content, "red on blue");
        assert_eq!(span.style.fg, Some(Color::Red));
        assert_eq!(span.style.bg, Some(Color::Blue));

        let no_bg = styled_text(cap, PreviewColors::Foreground);
        let span = &no_bg.lines[0].spans[0];
        assert_eq!(span.style.fg, Some(Color::Red));
        assert_eq!(span.style.bg, None);

        let plain = styled_text(cap, PreviewColors::None);
        let span = &plain.lines[0].spans[0];
        assert_eq!(span.content, "red on blue");
        assert_eq!(span.style, Style::default());
    }

    /// Modifiers (bold, underline) survive the background drop — only the
    /// background colour is thrown away.
    #[test]
    fn foreground_mode_keeps_modifiers() {
        let text = styled_text("\x1b[1;4;33;41mloud\x1b[0m", PreviewColors::Foreground);
        let span = &text.lines[0].spans[0];
        assert_eq!(span.style.fg, Some(Color::Yellow));
        assert_eq!(span.style.bg, None);
        assert!(
            span.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert!(
            span.style
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }
}
