use anyhow::Result;
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::Color;
use std::ffi::OsStr;
use std::io::{IsTerminal, Write};

/// `NO_COLOR` disables palette output when it is present and not empty.
/// A dumb terminal cannot render color either. See https://no-color.org.
fn monochrome(no_color: Option<&OsStr>, term: Option<&str>) -> bool {
    no_color.is_some_and(|value| !value.is_empty()) || term == Some("dumb")
}

pub fn monochrome_requested() -> bool {
    monochrome(
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
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

/// A backend wrapper that strips palette colors while preserving symbols and
/// text modifiers. Ratatui itself has no global color switch.
pub struct MonochromeBackend<B> {
    inner: B,
    monochrome: bool,
}

impl<B> MonochromeBackend<B> {
    pub fn new(inner: B, monochrome: bool) -> Self {
        Self { inner, monochrome }
    }

    #[cfg(test)]
    fn into_inner(self) -> B {
        self.inner
    }
}

impl<B: Backend> Backend for MonochromeBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::result::Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        if !self.monochrome {
            return self.inner.draw(content);
        }
        let stripped: Vec<(u16, u16, Cell)> = content
            .map(|(x, y, cell)| {
                let mut cell = cell.clone();
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
                cell.underline_color = Color::Reset;
                (x, y, cell)
            })
            .collect();
        self.inner
            .draw(stripped.iter().map(|(x, y, cell)| (*x, *y, cell)))
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

impl<B: Write> Write for MonochromeBackend<B> {
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
    fn non_interactive_terminal_is_refused() {
        assert!(non_interactive_error(false, true).is_some());
        assert!(non_interactive_error(true, false).is_some());
        assert!(non_interactive_error(false, false).is_some());
        assert!(non_interactive_error(true, true).is_none());
    }

    #[test]
    fn monochrome_backend_strips_palette_colors() {
        let inner = TestBackend::new(8, 1);
        let mut backend = MonochromeBackend::new(inner, true);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
        buffer.set_string(
            0,
            0,
            "hi",
            Style::default()
                .fg(Color::Rgb(240, 198, 116))
                .bg(Color::Rgb(48, 54, 61))
                .underline_color(Color::Rgb(196, 181, 253))
                .add_modifier(Modifier::UNDERLINED),
        );
        backend
            .draw(
                buffer
                    .content
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| ((index % 8) as u16, (index / 8) as u16, cell)),
            )
            .unwrap();
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
    fn monochrome_backend_keeps_colors_when_disabled() {
        let inner = TestBackend::new(8, 1);
        let mut backend = MonochromeBackend::new(inner, false);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
        buffer.set_string(0, 0, "hi", Style::default().fg(Color::Red));
        backend
            .draw(
                buffer
                    .content
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| ((index % 8) as u16, (index / 8) as u16, cell)),
            )
            .unwrap();
        let inner = backend.into_inner();
        assert_eq!(inner.buffer().content[0].fg, Color::Red);
    }
}
