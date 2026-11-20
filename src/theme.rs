//! One place for every style and glyph the TUI uses.
//!
//! Two rules from the design note (TUI-v2 §7) live here so no renderer can
//! break them by accident:
//!
//! - **Only the 16 ANSI colours and the terminal default.** Never `White`,
//!   `Black`, `Rgb` or `Indexed`: the user's theme decides what they look like,
//!   and a light terminal stays readable.
//! - **De-emphasis is [`Modifier::DIM`], never `Color::DarkGray`.** Bright black
//!   is the *background* in Solarized-style themes, which is how the baseline's
//!   idle status line became invisible there.
//!
//! `NO_COLOR` (any non-empty value) drops every foreground colour and keeps the
//! attributes (`BOLD`, `DIM`, `UNDERLINE`, `REVERSED`); the glyphs stay, so the
//! state is still legible. `KELIR_ASCII=1` or `TERM=dumb` swaps the Unicode
//! glyphs for ASCII ones.

use ratatui::style::{Color, Modifier, Style};

/// The glyphs a state, a list and a table are drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub ascii: bool,
}

impl Glyphs {
    pub const fn unicode() -> Self {
        Self { ascii: false }
    }

    pub const fn ascii() -> Self {
        Self { ascii: true }
    }

    /// The spinner frames. A cycle of eight so the glyph changes ~8 times a
    /// second while the elapsed time changes once.
    pub const SPINNER: [&'static str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    pub const SPINNER_ASCII: [&'static str; 4] = ["|", "/", "-", "\\"];

    pub fn spinner(&self, frame: u64) -> &'static str {
        if self.ascii {
            Self::SPINNER_ASCII[(frame as usize) % Self::SPINNER_ASCII.len()]
        } else {
            Self::SPINNER[(frame as usize) % Self::SPINNER.len()]
        }
    }

    pub fn idle(&self) -> &'static str {
        if self.ascii { "*" } else { "●" }
    }

    pub fn paused(&self) -> &'static str {
        if self.ascii { "||" } else { "⏸" }
    }

    pub fn error(&self) -> &'static str {
        if self.ascii { "x" } else { "✗" }
    }

    pub fn done(&self) -> &'static str {
        if self.ascii { "v" } else { "✓" }
    }

    pub fn stopped(&self) -> &'static str {
        if self.ascii { "#" } else { "⏹" }
    }

    /// The editor's prompt, on every row (`› ` / `  `).
    pub fn prompt(&self) -> &'static str {
        if self.ascii { "> " } else { "› " }
    }

    pub fn prompt_pad(&self) -> &'static str {
        "  "
    }

    /// Bullets by nesting level: `•`, `◦`, `▪`, then ASCII `-`, `*`, `+`.
    pub fn bullet(&self, level: usize) -> &'static str {
        if self.ascii {
            ["- ", "* ", "+ "][level.min(2)]
        } else {
            ["• ", "◦ ", "▪ "][level.min(2)]
        }
    }

    pub fn quote_bar(&self) -> &'static str {
        if self.ascii { "> " } else { "▎ " }
    }

    pub fn code_gutter(&self) -> &'static str {
        if self.ascii { "| " } else { "│ " }
    }

    pub fn rule(&self) -> char {
        if self.ascii { '-' } else { '─' }
    }

    pub fn ellipsis(&self) -> char {
        if self.ascii { '.' } else { '…' }
    }

    /// A newline shown inside a one-line preview.
    pub fn newline_mark(&self) -> &'static str {
        if self.ascii { " | " } else { " ↵ " }
    }

    /// The table border pieces.
    pub fn table(&self) -> Table {
        if self.ascii {
            Table {
                h: '-',
                v: '|',
                tl: '+',
                tm: '+',
                tr: '+',
                ml: '+',
                mm: '+',
                mr: '+',
                bl: '+',
                bm: '+',
                br: '+',
            }
        } else {
            Table {
                h: '─',
                v: '│',
                tl: '┌',
                tm: '┬',
                tr: '┐',
                ml: '├',
                mm: '┼',
                mr: '┤',
                bl: '└',
                bm: '┴',
                br: '┘',
            }
        }
    }
}

/// The box-drawing characters of one table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table {
    pub h: char,
    pub v: char,
    pub tl: char,
    pub tm: char,
    pub tr: char,
    pub ml: char,
    pub mm: char,
    pub mr: char,
    pub bl: char,
    pub bm: char,
    pub br: char,
}

/// Colour or not, Unicode or not: the two switches every style reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// `false` under `NO_COLOR`: attributes stay, colours go.
    pub color: bool,
    pub glyphs: Glyphs,
}

impl Default for Theme {
    fn default() -> Self {
        Self::plain()
    }
}

impl Theme {
    /// The theme a process gets from its environment.
    pub fn from_env() -> Self {
        let color = match std::env::var_os("NO_COLOR") {
            Some(value) => value.is_empty(),
            None => true,
        };
        let ascii = std::env::var_os("KELIR_ASCII").is_some_and(|value| !value.is_empty())
            || std::env::var("TERM")
                .map(|term| term == "dumb")
                .unwrap_or(false);
        Self {
            color,
            glyphs: if ascii {
                Glyphs::ascii()
            } else {
                Glyphs::unicode()
            },
        }
    }

    /// Colours on, Unicode glyphs: what a test (and a modern terminal) wants.
    pub const fn plain() -> Self {
        Self {
            color: true,
            glyphs: Glyphs::unicode(),
        }
    }

    /// The same theme with colours off (`NO_COLOR`), for a snapshot test.
    pub const fn no_color(self) -> Self {
        Self {
            color: false,
            ..self
        }
    }

    pub const fn ascii(self) -> Self {
        Self {
            glyphs: Glyphs::ascii(),
            ..self
        }
    }

    fn fg(self, color: Color) -> Style {
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    /// Plain default-weight text: assistant prose. The visual focus.
    pub fn prose(self) -> Style {
        Style::default()
    }

    /// The human's own words in the transcript, so a long session can be
    /// scanned for what was asked: a colour of its own, still default weight.
    pub fn user(self) -> Style {
        self.fg(Color::Cyan)
    }

    /// Tool lines, meta, hints: dim, never a colour.
    pub fn dim(self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    pub fn strong(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    pub fn italic(self) -> Style {
        Style::default().add_modifier(Modifier::ITALIC)
    }

    pub fn strike(self) -> Style {
        Style::default().add_modifier(Modifier::CROSSED_OUT)
    }

    pub fn underline(self) -> Style {
        Style::default().add_modifier(Modifier::UNDERLINED)
    }

    /// H1: bold + underline.
    pub fn h1(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }

    pub fn h2(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    /// H3 and deeper: bold, dimmed down.
    pub fn h3(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD | Modifier::DIM)
    }

    /// Inline code: yellow, or reversed when colours are off (the backticks
    /// are kept in that case, so the reader can see what it is).
    pub fn code(self) -> Style {
        if self.color {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    pub fn link(self) -> Style {
        Style::default().add_modifier(Modifier::UNDERLINED)
    }

    /// Working states: thinking, writing, running a tool.
    pub fn working(self) -> Style {
        self.fg(Color::Cyan)
    }

    /// Rate limited, paused, waiting on a human.
    pub fn waiting(self) -> Style {
        self.fg(Color::Yellow)
    }

    pub fn error(self) -> Style {
        self.fg(Color::Red)
    }

    pub fn ok(self) -> Style {
        self.fg(Color::Green)
    }

    /// The idle state row: default foreground, dimmed.
    pub fn idle(self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// Reversed video, the only "background" kelir uses (it inverts correctly
    /// on a light terminal too).
    pub fn chip(self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glyph_set_switches_to_ascii_and_keeps_every_state_distinct() {
        let unicode = Glyphs::unicode();
        let ascii = Glyphs::ascii();
        let states = [
            unicode.idle(),
            unicode.paused(),
            unicode.error(),
            unicode.done(),
        ];
        let mut unique = states.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), states.len(), "{states:?}");
        for glyph in [
            ascii.idle(),
            ascii.paused(),
            ascii.error(),
            ascii.done(),
            ascii.prompt(),
        ] {
            assert!(glyph.is_ascii(), "{glyph:?} is not ASCII");
        }
        assert_eq!(ascii.spinner(0), "|");
        assert_eq!(ascii.spinner(4), "|", "the ASCII spinner cycles in fours");
        assert_eq!(unicode.spinner(8), unicode.spinner(0));
        assert_eq!(ascii.rule(), '-');
        assert_eq!(ascii.ellipsis(), '.');
    }

    #[test]
    fn no_color_drops_every_foreground_and_keeps_the_attributes() {
        let plain = Theme::plain();
        let off = plain.no_color();
        for style in [
            off.working(),
            off.waiting(),
            off.error(),
            off.ok(),
            off.code(),
        ] {
            assert_eq!(style.fg, None, "{style:?}");
        }
        assert!(off.working() == off.error(), "they are the same style now");
        assert!(off.dim().add_modifier.contains(Modifier::DIM));
        assert!(off.h1().add_modifier.contains(Modifier::UNDERLINED));
        assert!(off.chip().add_modifier.contains(Modifier::REVERSED));
        assert_eq!(plain.working().fg, Some(Color::Cyan));
        assert_eq!(plain.error().fg, Some(Color::Red));
    }

    #[test]
    fn de_emphasis_is_dim_never_bright_black() {
        let theme = Theme::plain();
        for style in [theme.dim(), theme.idle(), theme.h3()] {
            assert_ne!(style.fg, Some(Color::DarkGray), "{style:?}");
            assert!(
                style.add_modifier.contains(Modifier::DIM) || style.fg.is_none(),
                "{style:?}"
            );
        }
        assert!(theme.dim().add_modifier.contains(Modifier::DIM));
        assert!(theme.h3().add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn the_two_switches_are_independent() {
        let plain = Theme::plain();
        assert!(plain.color);
        assert!(!plain.glyphs.ascii);
        assert!(!plain.no_color().color);
        assert!(plain.ascii().glyphs.ascii);
        assert!(plain.ascii().color, "ascii does not turn colours off");
        assert!(!plain.no_color().ascii().color);
    }
}
