//! Renders unified diffs in the codex style: a right-aligned line-number
//! gutter, a `+`/`-`/space sign, and tinted full-row backgrounds for added
//! and removed lines.
//!
//! Visual design ported from codex-rs `tui/src/diff_render.rs`
//! (openai/codex, Apache-2.0), simplified for baton: no syntax
//! highlighting and the dark palette only, since baton's theme is dark.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::rc::Rc;

use super::COLORS;

/// Muted tinted row backgrounds, dark palette (codex `#213A2B` / `#4A221D`).
const ADD_LINE_BG: Color = Color::Rgb(33, 58, 43);
const DEL_LINE_BG: Color = Color::Rgb(74, 34, 29);
const TAB_REPLACEMENT: &str = "    ";

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffLineKind {
    Insert,
    Delete,
    Context,
}

struct DiffRow {
    kind: DiffLineKind,
    line_number: usize,
    text: String,
}

struct DiffHunk {
    rows: Vec<DiffRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRowRef {
    pub path: String,
    pub line_number: usize,
    pub added: bool,
    pub removed: bool,
    pub text: String,
}

struct DiffFile {
    path: String,
    verb: &'static str,
    hunks: Vec<DiffHunk>,
    added: usize,
    removed: usize,
    max_line_number: usize,
    old_line: usize,
    new_line: usize,
}

struct DiffCacheEntry {
    diff: String,
    width: usize,
    lines: Rc<Vec<Line<'static>>>,
}

#[derive(Default)]
pub struct DiffCache {
    entries: Vec<DiffCacheEntry>,
}

impl DiffCache {
    pub fn render(&mut self, diff: &str, width: usize) -> Rc<Vec<Line<'static>>> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.width == width && entry.diff == diff)
        {
            let entry = self.entries.remove(index);
            let lines = entry.lines.clone();
            self.entries.insert(0, entry);
            return lines;
        }
        let lines = Rc::new(render_diff_lines(diff, width));
        self.entries.insert(
            0,
            DiffCacheEntry {
                diff: diff.to_string(),
                width,
                lines: lines.clone(),
            },
        );
        self.entries.truncate(8);
        lines
    }
}

fn line_number_width(max_line_number: usize) -> usize {
    if max_line_number == 0 {
        1
    } else {
        max_line_number.to_string().len()
    }
}

fn parse_hunk_header(inner: &str) -> Option<(usize, usize)> {
    // inner is the "-12,3 +13,4" part of an @@ header
    let mut parts = inner.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let old_start = old.split(',').next()?.parse::<usize>().ok()?;
    let new_start = new.split(',').next()?.parse::<usize>().ok()?;
    Some((old_start, new_start))
}

fn parse_unified_diff(diff: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest
                .rsplit(' ')
                .next()
                .map(strip_prefix_b)
                .unwrap_or_else(|| rest.to_string());
            files.push(DiffFile {
                path,
                verb: "Edited",
                hunks: Vec::new(),
                added: 0,
                removed: 0,
                max_line_number: 0,
                old_line: 1,
                new_line: 1,
            });
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        if line.starts_with("new file mode") {
            file.verb = "Added";
        } else if line.starts_with("deleted file mode") {
            file.verb = "Deleted";
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if file.hunks.is_empty() {
                file.path = strip_prefix_b(rest.trim_end());
            }
        } else if let Some(rest) = line.strip_prefix("@@") {
            let inner = rest.split("@@").next().unwrap_or(rest).trim();
            if let Some((old_start, new_start)) = parse_hunk_header(inner) {
                file.hunks.push(DiffHunk {
                    rows: Vec::new(),
                });
                file.old_line = old_start.max(1);
                file.new_line = new_start.max(1);
                continue;
            }
        } else if let Some(hunk) = file.hunks.last_mut() {
            let (kind, text) = if let Some(text) = line.strip_prefix('+') {
                (DiffLineKind::Insert, text)
            } else if let Some(text) = line.strip_prefix('-') {
                (DiffLineKind::Delete, text)
            } else if let Some(text) = line.strip_prefix(' ') {
                (DiffLineKind::Context, text)
            } else {
                continue; // "\ No newline at end of file" and friends
            };
            let line_number = match kind {
                DiffLineKind::Insert => {
                    let number = file.new_line;
                    file.new_line += 1;
                    file.added += 1;
                    number
                }
                DiffLineKind::Delete => {
                    let number = file.old_line;
                    file.old_line += 1;
                    file.removed += 1;
                    number
                }
                DiffLineKind::Context => {
                    let number = file.new_line;
                    file.old_line += 1;
                    file.new_line += 1;
                    number
                }
            };
            file.max_line_number = file.max_line_number.max(line_number);
            hunk.rows.push(DiffRow {
                kind,
                line_number,
                text: text.replace('\t', TAB_REPLACEMENT),
            });
        }
    }
    files
}

fn strip_prefix_b(path: &str) -> String {
    path.strip_prefix("b/").unwrap_or(path).to_string()
}

fn counts_summary(added: usize, removed: usize) -> Vec<Span<'static>> {
    vec![
        Span::raw("("),
        Span::styled(format!("+{added}"), Style::default().fg(COLORS.success)),
        Span::raw(" "),
        Span::styled(format!("-{removed}"), Style::default().fg(COLORS.error)),
        Span::raw(")"),
    ]
}

/// Stable content fingerprint for persisted review notes. FNV-1a is not a
/// security hash; it only keeps an old note from attaching to a different diff
/// when a turn number is reused after rewind.
pub fn fingerprint(diff: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in diff.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Every line inside a hunk, including context. Review mode uses these for
/// display while notes remain anchored to added or removed rows.
pub fn review_rows(diff: &str) -> Vec<DiffRowRef> {
    let mut rows = Vec::new();
    for file in parse_unified_diff(diff) {
        for hunk in file.hunks {
            for row in hunk.rows {
                rows.push(DiffRowRef {
                    path: file.path.clone(),
                    line_number: row.line_number,
                    added: matches!(row.kind, DiffLineKind::Insert),
                    removed: matches!(row.kind, DiffLineKind::Delete),
                    text: row.text,
                });
            }
        }
    }
    rows
}

/// Renders a unified diff as terminal lines, codex style.
pub fn render_diff_lines(diff: &str, width: usize) -> Vec<Line<'static>> {
    let files = parse_unified_diff(diff);
    if files.is_empty() {
        return Vec::new();
    }
    let total_added: usize = files.iter().map(|file| file.added).sum();
    let total_removed: usize = files.iter().map(|file| file.removed).sum();
    let single = files.len() == 1;

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut header: Vec<Span<'static>> = vec![Span::styled("• ", dim)];
    if single {
        header.push(Span::styled(files[0].verb, bold));
        header.push(Span::raw(" "));
        header.push(Span::raw(files[0].path.clone()));
        header.push(Span::raw(" "));
        header.extend(counts_summary(files[0].added, files[0].removed));
    } else {
        header.push(Span::styled("Edited", bold));
        header.push(Span::raw(format!(" {} files ", files.len())));
        header.extend(counts_summary(total_added, total_removed));
    }
    out.push(Line::from(header));

    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            out.push(Line::from(""));
        }
        if !single {
            let mut row: Vec<Span<'static>> = vec![Span::styled("  └ ", dim)];
            row.push(Span::raw(file.path.clone()));
            row.push(Span::raw(" "));
            row.extend(counts_summary(file.added, file.removed));
            out.push(Line::from(row));
        }
        let gutter_width = line_number_width(file.max_line_number).max(1);
        let available = width.saturating_sub(gutter_width + 2).max(1);
        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            if hunk_index > 0 {
                let spacer = format!("{:width$} ", "", width = gutter_width);
                out.push(Line::from(vec![
                    Span::styled(spacer, Style::default().add_modifier(Modifier::DIM)),
                    Span::styled("⋮", dim),
                ]));
            }
            for row in &hunk.rows {
                let (line_bg, content_fg) = match row.kind {
                    DiffLineKind::Insert => (Some(ADD_LINE_BG), COLORS.success),
                    DiffLineKind::Delete => (Some(DEL_LINE_BG), COLORS.error),
                    DiffLineKind::Context => (None, COLORS.text),
                };
                let sign = match row.kind {
                    DiffLineKind::Insert => '+',
                    DiffLineKind::Delete => '-',
                    DiffLineKind::Context => ' ',
                };
                let gutter_style = Style::default().add_modifier(Modifier::DIM);
                let number_prefix = Span::styled(
                    format!("{:>width$} ", row.line_number, width = gutter_width),
                    gutter_style,
                );
                let content_style = Style::default().fg(content_fg);
                let chars: Vec<char> = row.text.chars().collect();
                let mut offset = 0;
                loop {
                    let chunk: String = chars.iter().skip(offset).take(available).collect();
                    if chunk.is_empty() {
                        break;
                    }
                    let mut spans = if offset == 0 {
                        vec![number_prefix.clone(), Span::styled(sign.to_string(), content_style)]
                    } else {
                        vec![Span::styled(
                            format!("{:width$}  ", "", width = gutter_width),
                            gutter_style,
                        )]
                    };
                    spans.push(Span::styled(chunk, content_style));
                    let mut line = Line::from(spans);
                    if let Some(bg) = line_bg {
                        line = line.style(Style::default().bg(bg));
                    }
                    out.push(line);
                    offset += available;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/notes.txt b/notes.txt\nindex 700..de2 100644\n--- a/notes.txt\n+++ b/notes.txt\n@@ -1,3 +1,3 @@\n context line\n-old line\n+new line\n+another new line\n";

    #[test]
    fn parses_files_counts_and_line_numbers() {
        let files = parse_unified_diff(SAMPLE);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "notes.txt");
        assert_eq!(files[0].verb, "Edited");
        assert_eq!(files[0].added, 2);
        assert_eq!(files[0].removed, 1);
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0].rows.len(), 4);
        assert_eq!(files[0].hunks[0].rows[1].line_number, 2, "delete keeps the old number");
        assert_eq!(files[0].hunks[0].rows[2].line_number, 2, "insert reuses the new-file number");
    }

    #[test]
    fn renders_codex_style_rows() {
        let lines = render_diff_lines(SAMPLE, 80);
        let text: Vec<String> = lines.iter().map(|line| line.spans.iter().map(|span| span.content.clone()).collect()).collect();
        assert!(text[0].starts_with("• Edited notes.txt (+2 -1)"), "header: {}", text[0]);
        // gutter: right-aligned number, space, sign, content
        assert_eq!(text[1], "1  context line");
        assert_eq!(text[2], "2 -old line");
        assert_eq!(text[3], "2 +new line");
        assert_eq!(text[4], "3 +another new line");
    }

    #[test]
    fn added_rows_carry_green_text_on_tinted_background() {
        let lines = render_diff_lines(SAMPLE, 80);
        let added = &lines[3];
        assert_eq!(added.style.bg, Some(ADD_LINE_BG));
        let sign = &added.spans[1];
        assert_eq!(sign.content, "+");
        assert_eq!(sign.style.fg, Some(COLORS.success));
        let removed = &lines[2];
        assert_eq!(removed.style.bg, Some(DEL_LINE_BG));
        assert_eq!(removed.spans[1].style.fg, Some(COLORS.error));
    }

    #[test]
    fn review_rows_expose_file_line_and_side() {
        let rows: Vec<_> = review_rows(SAMPLE)
            .into_iter()
            .filter(|row| row.added || row.removed)
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].path, "notes.txt");
        assert_eq!(rows[0].line_number, 2);
        assert!(rows[0].removed);
        assert!(!rows[0].added);
        assert_eq!(rows[0].text, "old line");
        assert!(rows[1].added);
        assert_eq!(rows[2].line_number, 3);
        assert_eq!(rows[2].text, "another new line");
    }

    #[test]
    fn hunk_separator_and_wrapping() {
        let two_hunks = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-c\n+d\n";
        let lines = render_diff_lines(two_hunks, 80);
        let text: Vec<String> = lines.iter().map(|line| line.spans.iter().map(|span| span.content.clone()).collect()).collect();
        assert!(text.iter().any(|line| line.ends_with("⋮")), "hunk separator rendered");

        let long = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n+abcdefghijklmnopqrstuvwxyz\n";
        let lines = render_diff_lines(long, 12);
        let text: Vec<String> = lines.iter().map(|line| line.spans.iter().map(|span| span.content.clone()).collect()).collect();
        assert!(text[1].ends_with("+abcdefghi"), "first chunk: {}", text[1]);
        assert!(text[2].starts_with("   "), "continuation keeps the gutter indent: {}", text[2]);
        assert!(text[2].ends_with("jklmnopqr"), "second chunk: {}", text[2]);
    }

    #[test]
    fn cache_reuses_the_same_diff_and_width() {
        let mut cache = DiffCache::default();
        let first = cache.render(SAMPLE, 80);
        let second = cache.render(SAMPLE, 80);
        assert!(Rc::ptr_eq(&first, &second));
        let narrow = cache.render(SAMPLE, 40);
        assert!(!Rc::ptr_eq(&first, &narrow));
        assert_eq!(cache.entries.len(), 2);
    }

    #[test]
    fn cache_stays_bounded() {
        let mut cache = DiffCache::default();
        for index in 0..10 {
            let diff = format!(
                "diff --git a/f{index} b/f{index}\n--- a/f{index}\n+++ b/f{index}\n@@ -1 +1 @@\n-a\n+b\n"
            );
            cache.render(&diff, 80);
        }
        assert_eq!(cache.entries.len(), 8);
    }
}
