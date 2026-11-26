use anyhow::Result;
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::Color;
use std::ffi::OsStr;
use std::io::{IsTerminal, Write};

const ANSI16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (128, 0, 0),
    (0, 128, 0),
    (128, 128, 0),
    (0, 0, 128),
    (128, 0, 128),
    (0, 128, 128),
    (192, 192, 192),
    (128, 128, 128),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (0, 0, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Monochrome,
    Ansi16,
    Ansi256,
    TrueColor,
}

/// `NO_COLOR` disables palette output when it is present and not empty.
/// A dumb terminal cannot render color either. See https://no-color.org.
fn monochrome(no_color: Option<&OsStr>, term: Option<&str>) -> bool {
    no_color.is_some_and(|value| !value.is_empty()) || term == Some("dumb")
}

/// RGB output is used only when the terminal advertises truecolor. A
/// `-256color` term gets the 256-color cube and anything else gets 16 colors.
fn color_mode(no_color: Option<&OsStr>, term: Option<&str>, colorterm: Option<&str>) -> ColorMode {
    if monochrome(no_color, term) {
        return ColorMode::Monochrome;
    }
    let term = term.unwrap_or("");
    let colorterm = colorterm.unwrap_or("");
    if colorterm.eq_ignore_ascii_case("truecolor")
        || colorterm.eq_ignore_ascii_case("24bit")
        || term.contains("direct")
    {
        ColorMode::TrueColor
    } else if term.contains("256color") {
        ColorMode::Ansi256
    } else {
        ColorMode::Ansi16
    }
}

pub fn color_mode_requested() -> ColorMode {
    color_mode(
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var("COLORTERM").ok().as_deref(),
    )
}

fn xterm_rgb(index: u8) -> Option<(u8, u8, u8)> {
    let index = usize::from(index);
    if index < 16 {
        return ANSI16.get(index).copied();
    }
    if index < 232 {
        let cube = index - 16;
        let levels = [0u8, 95, 135, 175, 215, 255];
        return Some((levels[cube / 36], levels[(cube / 6) % 6], levels[cube % 6]));
    }
    if index < 256 {
        let value = 8 + (index as u8 - 232) * 10;
        return Some((value, value, value));
    }
    None
}

fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Reset => None,
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((205, 0, 0)),
        Color::Green => Some((0, 205, 0)),
        Color::Yellow => Some((205, 205, 0)),
        Color::Blue => Some((0, 0, 238)),
        Color::Magenta => Some((205, 0, 205)),
        Color::Cyan => Some((0, 205, 205)),
        Color::Gray => Some((229, 229, 229)),
        Color::DarkGray => Some((127, 127, 127)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((92, 92, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        Color::Indexed(index) => xterm_rgb(index),
    }
}

fn distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> u32 {
    let channel = |a: u8, b: u8| {
        let difference = i32::from(a) - i32::from(b);
        (difference * difference) as u32
    };
    channel(left.0, right.0) + channel(left.1, right.1) + channel(left.2, right.2)
}

fn nearest_index(color: Color, range: std::ops::RangeInclusive<u8>) -> Option<u8> {
    let rgb = color_rgb(color)?;
    range.min_by_key(|index| {
        xterm_rgb(*index)
            .map(|candidate| distance(candidate, rgb))
            .unwrap_or(u32::MAX)
    })
}

/// The 16 basic palette colors use named SGR codes (30-37 and 90-97), not the
/// extended `38;5;n` form that a 16-color terminal may not understand.
fn basic_color(index: u8) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

fn named_basic(color: Color) -> Option<Color> {
    matches!(
        color,
        Color::Black
            | Color::Red
            | Color::Green
            | Color::Yellow
            | Color::Blue
            | Color::Magenta
            | Color::Cyan
            | Color::Gray
            | Color::DarkGray
            | Color::LightRed
            | Color::LightGreen
            | Color::LightYellow
            | Color::LightBlue
            | Color::LightMagenta
            | Color::LightCyan
            | Color::White
    )
    .then_some(color)
}

/// `fallbacks` maps a theme color to the basic color that carries its meaning.
/// It wins over nearest-RGB, which cannot tell the accent, success, and error
/// roles apart on a 16-color terminal.
fn degrade(color: Color, mode: ColorMode, fallbacks: &[(Color, Color)]) -> Color {
    match mode {
        ColorMode::TrueColor => color,
        ColorMode::Monochrome => Color::Reset,
        ColorMode::Ansi256 => match color {
            Color::Reset | Color::Indexed(_) => color,
            named if named_basic(named).is_some() => named,
            other => nearest_index(other, 16..=255)
                .map(Color::Indexed)
                .unwrap_or(Color::Reset),
        },
        ColorMode::Ansi16 => match color {
            Color::Reset => Color::Reset,
            named if named_basic(named).is_some() => named,
            other => fallbacks
                .iter()
                .find(|(rich, _)| *rich == other)
                .map(|(_, basic)| *basic)
                .or_else(|| nearest_index(other, 0..=15).map(basic_color))
                .unwrap_or(Color::Reset),
        },
    }
}

pub fn non_interactive_error(stdin_tty: bool, stdout_tty: bool) -> Option<&'static str> {
    if stdin_tty && stdout_tty {
        None
    } else {
        Some(
            "baton needs an interactive terminal for the TUI; use `baton exec` for scripts and pipes",
        )
    }
}

pub fn require_interactive_terminal() -> Result<()> {
    if let Some(message) = non_interactive_error(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    ) {
        anyhow::bail!(message);
    }
    Ok(())
}

/// A backend wrapper that degrades palette colors to the detected color depth
/// while preserving symbols and text modifiers. Ratatui has no global switch.
pub struct ColorBackend<B> {
    inner: B,
    mode: ColorMode,
    fallbacks: Vec<(Color, Color)>,
    cache: Vec<(Color, Color)>,
}

impl<B> ColorBackend<B> {
    pub fn new(inner: B, mode: ColorMode, fallbacks: &[(Color, Color)]) -> Self {
        Self {
            inner,
            mode,
            fallbacks: fallbacks.to_vec(),
            cache: Vec::new(),
        }
    }

    fn map(&mut self, color: Color) -> Color {
        if let Some((_, mapped)) = self.cache.iter().find(|(source, _)| *source == color) {
            return *mapped;
        }
        let mapped = degrade(color, self.mode, &self.fallbacks);
        self.cache.push((color, mapped));
        mapped
    }

    #[cfg(test)]
    fn into_inner(self) -> B {
        self.inner
    }
}

impl<B: Backend> Backend for ColorBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::result::Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        if self.mode == ColorMode::TrueColor {
            return self.inner.draw(content);
        }
        let mapped: Vec<(u16, u16, Cell)> = content
            .map(|(x, y, cell)| {
                let mut cell = cell.clone();
                cell.fg = self.map(cell.fg);
                cell.bg = self.map(cell.bg);
                cell.underline_color = self.map(cell.underline_color);
                (x, y, cell)
            })
            .collect();
        self.inner
            .draw(mapped.iter().map(|(x, y, cell)| (*x, *y, cell)))
    }

    fn hide_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> std::result::Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(
        &mut self,
        position: P,
    ) -> std::result::Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> std::result::Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> std::result::Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> std::result::Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.flush()
    }
}

impl<B: Write> Write for ColorBackend<B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};

    fn draw_into(backend: &mut ColorBackend<TestBackend>, style: Style) {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
        buffer.set_string(0, 0, "hi", style);
        backend
            .draw(
                buffer
                    .content
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| ((index % 8) as u16, (index / 8) as u16, cell)),
            )
            .unwrap();
    }

    #[test]
    fn no_color_values_decide_monochrome() {
        assert!(monochrome(Some(OsStr::new("1")), None));
        assert!(monochrome(
            Some(OsStr::new("anything")),
            Some("xterm-256color")
        ));
        assert!(!monochrome(Some(OsStr::new("")), Some("xterm-256color")));
        assert!(!monochrome(None, Some("xterm-256color")));
        assert!(monochrome(None, Some("dumb")));
        assert!(!monochrome(None, None));
    }

    #[test]
    fn color_depth_follows_term_and_colorterm() {
        assert_eq!(
            color_mode(
                Some(OsStr::new("1")),
                Some("xterm-256color"),
                Some("truecolor")
            ),
            ColorMode::Monochrome
        );
        assert_eq!(
            color_mode(None, Some("xterm-256color"), Some("truecolor")),
            ColorMode::TrueColor
        );
        assert_eq!(
            color_mode(None, Some("xterm-256color"), Some("24bit")),
            ColorMode::TrueColor
        );
        assert_eq!(
            color_mode(None, Some("xterm-direct"), None),
            ColorMode::TrueColor
        );
        assert_eq!(
            color_mode(None, Some("xterm-256color"), None),
            ColorMode::Ansi256
        );
        assert_eq!(color_mode(None, Some("xterm"), None), ColorMode::Ansi16);
        assert_eq!(
            color_mode(None, Some("dumb"), Some("truecolor")),
            ColorMode::Monochrome
        );
    }

    #[test]
    fn ansi256_mapping_uses_the_cube() {
        let mapped = degrade(Color::Rgb(240, 198, 116), ColorMode::Ansi256, &[]);
        let Color::Indexed(index) = mapped else {
            panic!("expected an indexed color, got {mapped:?}");
        };
        assert!(index >= 16, "{mapped:?}");
    }

    #[test]
    fn ansi16_mapping_uses_named_basic_colors() {
        assert_eq!(
            degrade(Color::Rgb(255, 0, 0), ColorMode::Ansi16, &[]),
            Color::LightRed
        );
        assert_eq!(
            degrade(Color::Rgb(0, 0, 0), ColorMode::Ansi16, &[]),
            Color::Black
        );
        assert_eq!(
            degrade(Color::Rgb(48, 54, 61), ColorMode::Ansi16, &[]),
            Color::Black
        );
        assert_eq!(degrade(Color::Red, ColorMode::Ansi16, &[]), Color::Red);
        assert_eq!(
            degrade(Color::LightRed, ColorMode::Ansi16, &[]),
            Color::LightRed
        );
    }

    #[test]
    fn ansi16_prefers_the_declared_fallback_over_nearest_rgb() {
        let accent = Color::Rgb(240, 198, 116);
        let fallbacks = [(accent, Color::LightYellow)];
        assert_eq!(
            degrade(accent, ColorMode::Ansi16, &fallbacks),
            Color::LightYellow
        );
        assert_eq!(
            degrade(accent, ColorMode::Ansi16, &[]),
            Color::Gray,
            "without a fallback, nearest RGB reads the cream accent as gray"
        );
        assert_eq!(
            degrade(accent, ColorMode::Ansi256, &fallbacks),
            degrade(accent, ColorMode::Ansi256, &[]),
            "the fallback table only applies to 16 colors"
        );
    }

    #[test]
    fn non_interactive_terminal_is_refused() {
        assert!(non_interactive_error(false, true).is_some());
        assert!(non_interactive_error(true, false).is_some());
        assert!(non_interactive_error(false, false).is_some());
        assert!(non_interactive_error(true, true).is_none());
    }

    #[test]
    fn monochrome_backend_strips_palette_colors() {
        let inner = TestBackend::new(8, 1);
        let mut backend = ColorBackend::new(inner, ColorMode::Monochrome, &[]);
        draw_into(
            &mut backend,
            Style::default()
                .fg(Color::Rgb(240, 198, 116))
                .bg(Color::Rgb(48, 54, 61))
                .underline_color(Color::Rgb(196, 181, 253))
                .add_modifier(Modifier::UNDERLINED),
        );
        let inner = backend.into_inner();
        for cell in inner.buffer().content.iter().take(2) {
            assert_eq!(cell.fg, Color::Reset);
            assert_eq!(cell.bg, Color::Reset);
            assert_eq!(cell.underline_color, Color::Reset);
            assert!(cell.modifier.contains(Modifier::UNDERLINED));
            assert_ne!(cell.symbol(), " ");
        }
    }

    #[test]
    fn ansi_backend_rewrites_rgb_and_keeps_modifiers() {
        let inner = TestBackend::new(8, 1);
        let mut backend = ColorBackend::new(inner, ColorMode::Ansi256, &[]);
        draw_into(
            &mut backend,
            Style::default()
                .fg(Color::Rgb(240, 198, 116))
                .bg(Color::Rgb(48, 54, 61))
                .add_modifier(Modifier::BOLD),
        );
        let inner = backend.into_inner();
        let cell = &inner.buffer().content[0];
        assert!(matches!(cell.fg, Color::Indexed(_)), "{:?}", cell.fg);
        assert!(matches!(cell.bg, Color::Indexed(_)), "{:?}", cell.bg);
        assert!(cell.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn ansi16_backend_emits_named_colors() {
        let accent = Color::Rgb(240, 198, 116);
        let inner = TestBackend::new(8, 1);
        let mut backend =
            ColorBackend::new(inner, ColorMode::Ansi16, &[(accent, Color::LightYellow)]);
        draw_into(
            &mut backend,
            Style::default().fg(accent).bg(Color::Rgb(48, 54, 61)),
        );
        let inner = backend.into_inner();
        let cell = &inner.buffer().content[0];
        assert_eq!(cell.fg, Color::LightYellow);
        assert_eq!(cell.bg, Color::Black);
    }

    #[test]
    fn ansi16_backend_maps_stray_rgb_to_named_colors() {
        let inner = TestBackend::new(8, 1);
        let mut backend = ColorBackend::new(inner, ColorMode::Ansi16, &[]);
        draw_into(
            &mut backend,
            Style::default()
                .fg(Color::Rgb(12, 34, 56))
                .bg(Color::Rgb(200, 210, 220))
                .underline_color(Color::Rgb(90, 30, 60)),
        );
        let inner = backend.into_inner();
        let cell = &inner.buffer().content[0];
        for color in [cell.fg, cell.bg, cell.underline_color] {
            assert!(
                named_basic(color).is_some(),
                "a 16-color terminal may not understand {color:?}"
            );
        }
    }

    #[test]
    fn truecolor_backend_keeps_rgb() {
        let inner = TestBackend::new(8, 1);
        let mut backend = ColorBackend::new(inner, ColorMode::TrueColor, &[]);
        draw_into(&mut backend, Style::default().fg(Color::Rgb(240, 198, 116)));
        let inner = backend.into_inner();
        assert_eq!(inner.buffer().content[0].fg, Color::Rgb(240, 198, 116));
    }
}
