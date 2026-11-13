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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnAlign {
    Left,
    Center,
    Right,
}

fn table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    let cells = trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect::<Vec<_>>();
    if cells.len() < 2 || cells.iter().all(|cell| cell.is_empty()) {
        return None;
    }
    Some(cells)
}

fn table_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim();
            let unsigned = cell.strip_prefix(':').unwrap_or(cell);
            let unsigned = unsigned.strip_suffix(':').unwrap_or(unsigned);
            unsigned.len() >= 3 && unsigned.chars().all(|character| character == '-')
        })
}

fn column_align(cell: &str) -> ColumnAlign {
    let trimmed = cell.trim();
    match (trimmed.starts_with(':'), trimmed.ends_with(':')) {
        (true, true) => ColumnAlign::Center,
        (false, true) => ColumnAlign::Right,
        _ => ColumnAlign::Left,
    }
}

fn cell_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let push_word = |word: &str, lines: &mut Vec<String>, current: &mut String| {
        let word_len = word.chars().count();
        if current.is_empty() {
            if word_len <= width {
                current.push_str(word);
            } else {
                let mut chunk = String::new();
                for character in word.chars() {
                    if chunk.chars().count() == width {
                        lines.push(std::mem::take(&mut chunk));
                    }
                    chunk.push(character);
                }
                *current = chunk;
            }
            return;
        }
        if current.chars().count() + 1 + word_len <= width {
            current.push(' ');
            current.push_str(word);
            return;
        }
        lines.push(std::mem::take(current));
        if word_len <= width {
            current.push_str(word);
        } else {
            let mut chunk = String::new();
            for character in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(character);
            }
            *current = chunk;
        }
    };
    for word in text.split_whitespace() {
        push_word(word, &mut lines, &mut current);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn pad_cell(text: &str, width: usize, align: ColumnAlign) -> String {
    let length = text.chars().count();
    if length >= width {
        return text.to_string();
    }
    let padding = width - length;
    match align {
        ColumnAlign::Left => format!("{text}{}", " ".repeat(padding)),
        ColumnAlign::Right => format!("{}{text}", " ".repeat(padding)),
        ColumnAlign::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{text}{}", " ".repeat(left), " ".repeat(right))
        }
    }
}

fn table_row(
    cells: &[String],
    widths: &[usize],
    aligns: &[ColumnAlign],
    style: Style,
) -> Vec<Line<'static>> {
    let wrapped = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            cell_lines(
                cells.get(index).map(String::as_str).unwrap_or(""),
                *width,
            )
        })
        .collect::<Vec<_>>();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut lines = Vec::new();
    for row in 0..height {
        let mut spans = Vec::new();
        for (column, width) in widths.iter().enumerate() {
            if column > 0 {
                spans.push(Span::styled(" │ ", Style::default().fg(COLORS.border)));
            }
            let text = wrapped[column]
                .get(row)
                .map(String::as_str)
                .unwrap_or("");
            spans.push(Span::styled(
                pad_cell(text, *width, aligns[column]),
                style,
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn render_table(
    header: &[String],
    aligns: &[ColumnAlign],
    rows: &[Vec<String>],
    width: usize,
) -> Option<Vec<Line<'static>>> {
    let columns = header.len();
    if columns == 0 {
        return None;
    }
    let fixed = 3 * (columns - 1);
    if width <= fixed + columns {
        return None;
    }
    let available = width - fixed;
    let mut widths = vec![3usize; columns];
    for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
        for (index, cell) in row.iter().enumerate().take(columns) {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    while widths.iter().sum::<usize>() > available {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 3)
            .max_by_key(|(_, width)| **width)
        else {
            return None;
        };
        widths[index] -= 1;
    }
    let mut lines = table_row(
        header,
        &widths,
        aligns,
        Style::default()
            .fg(COLORS.accent)
            .add_modifier(Modifier::BOLD),
    );
    let mut separator = String::new();
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            separator.push('─');
            separator.push('┼');
            separator.push('─');
        }
        separator.push_str(&"─".repeat(*width));
    }
    lines.push(Line::styled(separator, Style::default().fg(COLORS.border)));
    for row in rows {
        lines.extend(table_row(row, &widths, aligns, Style::default().fg(COLORS.text)));
    }
    Some(lines)
}

/// Renders Markdown into terminal lines. Fence markers are removed, code keeps
/// its own tinted background, and every returned row is at most `width` cells.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let raw_lines = text
        .split('\n')
        .map(|raw| raw.strip_suffix('\r').unwrap_or(raw))
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut in_code = false;
    let mut index = 0usize;
    while index < raw_lines.len() {
        let line = raw_lines[index];
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            index += 1;
            continue;
        }
        if in_code {
            let expanded = line.replace('\t', "    ");
            lines.extend(wrap_spans(
                vec![Span::styled(format!("  {expanded}"), code_style())],
                width,
            ));
            index += 1;
            continue;
        }
        if line.trim().is_empty() {
            lines.push(Line::default());
            index += 1;
            continue;
        }
        if let Some(header) = table_cells(line) {
            if let Some(separator) = raw_lines.get(index + 1).and_then(|line| table_cells(*line)) {
                if table_separator(&separator) {
                    let columns = header.len();
                    let aligns = (0..columns)
                        .map(|column| {
                            separator
                                .get(column)
                                .map(|cell| column_align(cell))
                                .unwrap_or(ColumnAlign::Left)
                        })
                        .collect::<Vec<_>>();
                    let mut rows = Vec::new();
                    let mut cursor = index + 2;
                    while cursor < raw_lines.len() {
                        let Some(cells) = table_cells(raw_lines[cursor]) else {
                            break;
                        };
                        if table_separator(&cells) {
                            break;
                        }
                        rows.push(cells);
                        cursor += 1;
                    }
                    if let Some(table) = render_table(&header, &aligns, &rows, width) {
                        lines.extend(table);
                        index = cursor;
                        continue;
                    }
                }
            }
        }
        if horizontal_rule(line) {
            lines.push(Line::styled(
                "─".repeat(width),
                Style::default().fg(COLORS.border),
            ));
            index += 1;
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
            index += 1;
            continue;
        }
        if let Some(content) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(COLORS.border))];
            spans.extend(inline_spans(content, Style::default().fg(COLORS.muted)));
            lines.extend(wrap_spans(spans, width));
            index += 1;
            continue;
        }
        if let Some((prefix, content)) = list_prefix(line) {
            let mut spans = vec![Span::styled(prefix, Style::default().fg(COLORS.accent))];
            spans.extend(inline_spans(content, Style::default().fg(COLORS.text)));
            lines.extend(wrap_spans(spans, width));
            index += 1;
            continue;
        }
        lines.extend(wrap_spans(
            inline_spans(line, Style::default().fg(COLORS.text)),
            width,
        ));
        index += 1;
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
    fn renders_tables_with_alignment_and_removes_markdown_pipes() {
        let lines = render(
            "| Name | Value |\n| --- | ---: |\n| alpha | 1 |\n| beta | 22 |\n\nafter",
            40,
        );
        let rendered = text(&lines);
        assert!(rendered.contains("Name"), "{rendered}");
        assert!(rendered.contains("alpha"), "{rendered}");
        assert!(rendered.contains("beta"), "{rendered}");
        assert!(rendered.contains("alpha │     1"), "right alignment: {rendered}");
        assert!(rendered.contains("│"), "{rendered}");
        assert!(rendered.contains("┼"), "{rendered}");
        assert!(!rendered.contains("| ---"), "{rendered}");
        assert!(rendered.contains("after"), "{rendered}");
        assert!(
            lines
                .iter()
                .all(|line| line.spans.iter().map(|span| span.content.chars().count()).sum::<usize>() <= 40),
            "every table row stays within the width"
        );
    }

    #[test]
    fn narrow_tables_wrap_cells_without_exceeding_the_width() {
        let lines = render(
            "| Key | Detail |\n| --- | --- |\n| alpha | one two three four five |",
            24,
        );
        let rendered = text(&lines);
        assert!(rendered.contains("alpha"), "{rendered}");
        assert!(rendered.contains("one two"), "{rendered}");
        assert!(rendered.contains("three"), "{rendered}");
        assert!(lines.iter().all(|line| {
            line.spans.iter().map(|span| span.content.chars().count()).sum::<usize>() <= 24
        }));
    }

    #[test]
    fn plain_pipe_text_is_not_mistaken_for_a_table() {
        let rendered = text(&render("cmd | grep x\nnot a table", 80));
        assert!(rendered.contains("cmd | grep x"), "{rendered}");
        assert!(rendered.contains("not a table"), "{rendered}");
        assert!(!rendered.contains("┼"), "{rendered}");
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
