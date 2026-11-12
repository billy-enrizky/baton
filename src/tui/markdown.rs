//! Small Markdown renderer for assistant text. It deliberately covers the
//! shapes coding agents emit most often: headings, lists, blockquotes, fenced
//! code, inline code, bold text, and links. Unknown syntax stays literal.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::rc::Rc;

use super::COLORS;

fn code_style() -> Style {
    Style::default().fg(COLORS.text).bg(COLORS.code_bg)
}

fn inline_code_style() -> Style {
    Style::default().fg(COLORS.secondary).bg(COLORS.code_bg)
}

fn flush_text(
    spans: &mut Vec<Span<'static>>,
    text: &mut String,
    style: &mut Option<Style>,
) {
    if !text.is_empty() {
        spans.push(Span::styled(std::mem::take(text), style.unwrap_or_default()));
    }
    *style = None;
}

/// Splits styled spans into rows no wider than `width` characters. Each row is
/// an exact terminal line, which keeps transcript scrolling deterministic.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_text = String::new();
    let mut current_style: Option<Style> = None;
    let mut used = 0usize;

    for span in spans {
        let style = span.style;
        for character in span.content.chars() {
            if used >= width {
                flush_text(&mut current, &mut current_text, &mut current_style);
                lines.push(Line::from(std::mem::take(&mut current)));
                used = 0;
            }
            if current_style != Some(style) {
                flush_text(&mut current, &mut current_text, &mut current_style);
                current_style = Some(style);
            }
            current_text.push(character);
            used += 1;
        }
    }
    flush_text(&mut current, &mut current_text, &mut current_style);
    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

fn push_plain(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if !text.is_empty() {
        spans.push(Span::styled(text.to_string(), style));
    }
}

fn inline_spans(line: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                push_plain(&mut spans, &plain, base);
                plain.clear();
                push_plain(
                    &mut spans,
                    &after[..end],
                    base.add_modifier(Modifier::BOLD),
                );
                rest = &after[end + 2..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                push_plain(&mut spans, &plain, base);
                plain.clear();
                push_plain(&mut spans, &after[..end], inline_code_style());
                rest = &after[end + 1..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('[') {
            if let Some(label_end) = after.find("](") {
                let url_start = label_end + 2;
                if let Some(url_end) = after[url_start..].find(')') {
                    push_plain(&mut spans, &plain, base);
                    plain.clear();
                    let label = &after[..label_end];
                    let url = &after[url_start..url_start + url_end];
                    push_plain(
                        &mut spans,
                        label,
                        base.fg(COLORS.secondary)
                            .add_modifier(Modifier::UNDERLINED),
                    );
                    push_plain(&mut spans, &format!(" ({url})"), base.fg(COLORS.muted));
                    rest = &after[url_start + url_end + 1..];
                    continue;
                }
            }
        }
        let character = rest.chars().next().expect("non-empty rest");
        plain.push(character);
        rest = &rest[character.len_utf8()..];
    }
    push_plain(&mut spans, &plain, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn heading(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|character| *character == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = trimmed.get(level..)?;
    let mut content = rest.strip_prefix(' ')?.trim_end();
    let without_closing = content.trim_end_matches('#');
    if without_closing.ends_with(' ') {
        content = without_closing.trim_end();
    }
    (!content.is_empty()).then_some(content)
}

fn list_prefix(line: &str) -> Option<(String, &str)> {
    let indent_len = line.len() - line.trim_start().len();
    let indent = &line[..indent_len];
    let rest = &line[indent_len..];
    for marker in ["- ", "* ", "+ "] {
        if let Some(content) = rest.strip_prefix(marker) {
            return Some((format!("{indent}• "), content));
        }
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(content) = rest.get(digits..).and_then(|tail| tail.strip_prefix(". ")) {
            return Some((format!("{indent}{}. ", &rest[..digits]), content));
        }
    }
    None
}

fn horizontal_rule(line: &str) -> bool {
    let line = line.trim();
    if line.len() < 3 {
        return false;
    }
    ["-", "*", "_"].iter().any(|marker| {
        let mut chars = line.chars();
        chars.all(|character| character.to_string() == *marker)
    })
}

/// Renders Markdown into terminal lines. Fence markers are removed, code keeps
/// its own tinted background, and every returned row is at most `width` cells.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            let expanded = line.replace('\t', "    ");
            lines.extend(wrap_spans(
                vec![Span::styled(format!("  {expanded}"), code_style())],
                width,
            ));
            continue;
        }
        if line.trim().is_empty() {
            lines.push(Line::default());
            continue;
        }
        if horizontal_rule(line) {
            lines.push(Line::styled(
                "─".repeat(width),
                Style::default().fg(COLORS.border),
            ));
            continue;
        }
        if let Some(content) = heading(line) {
            lines.extend(wrap_spans(
                vec![Span::styled(
                    content.to_string(),
                    Style::default()
                        .fg(COLORS.accent)
                        .add_modifier(Modifier::BOLD),
                )],
                width,
            ));
            continue;
        }
        if let Some(content) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(COLORS.border))];
            spans.extend(inline_spans(content, Style::default().fg(COLORS.muted)));
            lines.extend(wrap_spans(spans, width));
            continue;
        }
        if let Some((prefix, content)) = list_prefix(line) {
            let mut spans = vec![Span::styled(prefix, Style::default().fg(COLORS.accent))];
            spans.extend(inline_spans(content, Style::default().fg(COLORS.text)));
            lines.extend(wrap_spans(spans, width));
            continue;
        }
        lines.extend(wrap_spans(
            inline_spans(line, Style::default().fg(COLORS.text)),
            width,
        ));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

struct MarkdownCacheEntry {
    text: String,
    width: usize,
    lines: Rc<Vec<Line<'static>>>,
}

#[derive(Default)]
pub struct MarkdownCache {
    entries: Vec<MarkdownCacheEntry>,
}

impl MarkdownCache {
    pub fn render(&mut self, text: &str, width: usize) -> Rc<Vec<Line<'static>>> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.width == width && entry.text == text)
        {
            let entry = self.entries.remove(index);
            let lines = entry.lines.clone();
            self.entries.insert(0, entry);
            return lines;
        }
        let lines = Rc::new(render(text, width));
        self.entries.insert(
            0,
            MarkdownCacheEntry {
                text: text.to_string(),
                width,
                lines: lines.clone(),
            },
        );
        self.entries.truncate(16);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn strips_markdown_markers_and_styles_code() {
        let lines = render("# Title\n\n- one **bold** and `code`\n\n```rust\nlet x = 1;\n```", 80);
        let rendered = text(&lines);
        assert!(rendered.contains("Title"));
        assert!(!rendered.contains('#'));
        assert!(rendered.contains("• one bold and code"));
        assert!(rendered.contains("  let x = 1;"));
        assert!(!rendered.contains("```"));
        assert!(lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.style.bg == Some(COLORS.code_bg)));
    }

    #[test]
    fn wraps_every_row_to_the_requested_width() {
        let lines = render("alpha beta gamma delta", 8);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| {
            line.spans.iter().map(|span| span.content.chars().count()).sum::<usize>() <= 8
        }));
        assert_eq!(text(&lines).replace('\n', ""), "alpha beta gamma delta");
    }

    #[test]
    fn links_keep_the_label_and_url() {
        let rendered = text(&render("[docs](https://example.com)", 80));
        assert_eq!(rendered, "docs (https://example.com)");
    }

    #[test]
    fn cache_reuses_the_same_markdown_and_width() {
        let mut cache = MarkdownCache::default();
        let first = cache.render("# title", 80);
        let second = cache.render("# title", 80);
        assert!(Rc::ptr_eq(&first, &second));
        let narrow = cache.render("# title", 20);
        assert!(!Rc::ptr_eq(&first, &narrow));
        assert_eq!(cache.entries.len(), 2);
    }

    #[test]
    fn cache_stays_bounded() {
        let mut cache = MarkdownCache::default();
        for index in 0..20 {
            cache.render(&format!("# heading {index}"), 80);
        }
        assert_eq!(cache.entries.len(), 16);
    }
}
