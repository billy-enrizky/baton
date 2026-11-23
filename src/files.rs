//! Project file discovery and editor launch for terminal quick-open.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::doctor;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_FILES: usize = 20_001;

const HIDDEN_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".next",
    ".nuxt",
    ".cache",
    ".stably",
    ".yarn",
    ".pnpm-store",
    ".terraform",
    ".npm",
    ".npm-global",
    ".gvfs",
    ".venv",
];
const ROOT_HIDDEN_DIRS: &[&str] = &["target"];
const HIDDEN_PATHS: &[&str] = &[".local/share"];

#[derive(Debug, Clone, Copy)]
pub struct FileOptions<'a> {
    pub query: Option<&'a str>,
    pub limit: usize,
    pub include_ignored: bool,
    /// Most recently picked paths, newest first.
    pub recent: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileListing {
    pub files: Vec<String>,
    pub truncated: bool,
    pub capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Default)]
struct CandidateSet {
    items: Vec<String>,
    truncated: bool,
}

impl EditorCommand {
    pub fn shell_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn run(&self, cwd: &Path) -> Result<()> {
        let status = Command::new(&self.program)
            .args(&self.args)
            .current_dir(cwd)
            .status()
            .with_context(|| format!("cannot launch editor {}", self.program))?;
        if !status.success() {
            anyhow::bail!("editor {} exited with {status}", self.program);
        }
        Ok(())
    }
}

pub fn list_files(root: &Path, options: FileOptions<'_>) -> Result<FileListing> {
    if !root.is_dir() {
        anyhow::bail!("not a directory: {}", root.display());
    }
    let query = options.query.map(str::trim).unwrap_or("").to_lowercase();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let candidate_set = if let Some(rg) = doctor::find_executable("rg") {
        list_with_rg(root, &rg, options.include_ignored, &tokens)?
    } else {
        list_with_git(root, options.include_ignored, &tokens).context(
            "baton files needs ripgrep (rg) or a git repository; install ripgrep or run inside git",
        )?
    };
    let capped = candidate_set.truncated;
    let recent = options
        .recent
        .iter()
        .enumerate()
        .map(|(index, path)| (path.to_lowercase(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let mut candidates = rank_candidates_with_recent(candidate_set.items, options.query, &recent);
    let truncated = capped || candidates.len() > options.limit;
    candidates.truncate(options.limit);
    Ok(FileListing {
        files: candidates,
        truncated,
        capped,
    })
}

/// Canonical directory key for the recent-file list.
pub fn recent_project_key(cwd: &str) -> String {
    std::fs::canonicalize(cwd)
        .unwrap_or_else(|_| PathBuf::from(cwd))
        .to_string_lossy()
        .to_string()
}

pub fn resolve_project_file(root: &Path, path: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve -C directory {}", root.display()))?;
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let target = joined
        .canonicalize()
        .with_context(|| format!("file does not exist: {}", joined.display()))?;
    if !target.starts_with(&root) {
        anyhow::bail!("path is outside the -C directory: {}", path.display());
    }
    if !target.is_file() {
        anyhow::bail!("not a file: {}", path.display());
    }
    Ok(target)
}

pub fn editor_from_values(visual: Option<&str>, editor: Option<&str>) -> Option<String> {
    [visual, editor]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn editor_from_env() -> Option<String> {
    editor_from_values(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
    )
}

pub fn build_open_command(editor: &str, path: &Path) -> Result<EditorCommand> {
    build_open_command_at_line(editor, path, None)
}

pub fn build_open_command_at_line(
    editor: &str,
    path: &Path,
    line: Option<u64>,
) -> Result<EditorCommand> {
    let mut words = split_command_line(editor)?;
    if words.is_empty() {
        anyhow::bail!("VISUAL or EDITOR is empty");
    }
    let program = words.remove(0);
    if let Some(line) = line {
        match editor_program_name(&program).as_str() {
            "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => {
                words.push("--goto".into());
                words.push(format!("{}:{line}", path.to_string_lossy()));
            }
            "vim" | "vi" | "view" | "gvim" | "mvim" | "nvim" | "emacs" | "emacsclient" | "nano" => {
                words.push(format!("+{line}"));
                words.push(path.to_string_lossy().into_owned());
            }
            _ => words.push(format!("{}:{line}", path.to_string_lossy())),
        }
    } else {
        words.push(path.to_string_lossy().into_owned());
    }
    Ok(EditorCommand {
        program,
        args: words,
    })
}

fn editor_program_name(program: &str) -> String {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    name
}

/// Split a trailing `:<line>` from a quick-open path. A literal colon is escaped as `\:`.
pub fn split_line_target(query: &str) -> (String, Option<u64>) {
    let query = query.trim();
    if let Some((path, digits)) = query.rsplit_once(':') {
        if path.ends_with('\\') {
            return (query.replace("\\:", ":"), None);
        }
        let digits = digits.trim();
        if !path.trim().is_empty()
            && !digits.is_empty()
            && digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            if let Ok(line) = digits.parse::<u64>() {
                if line > 0 {
                    return (path.trim().replace("\\:", ":"), Some(line));
                }
            }
        }
    }
    (query.replace("\\:", ":"), None)
}

pub fn split_command_line(input: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut word_started = false;
    for (index, character) in input.char_indices() {
        if escaped {
            current.push(character);
            escaped = false;
            word_started = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') && escapes_next(input, index) {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                word_started = true;
            }
            character if character.is_whitespace() => {
                if word_started {
                    words.push(std::mem::take(&mut current));
                    word_started = false;
                }
            }
            _ => {
                current.push(character);
                word_started = true;
            }
        }
    }
    if quote.is_some() {
        anyhow::bail!("editor command has an unclosed quote");
    }
    if word_started {
        words.push(current);
    }
    Ok(words)
}

fn list_with_rg(
    root: &Path,
    rg: &Path,
    include_ignored: bool,
    tokens: &[&str],
) -> Result<CandidateSet> {
    let primary = run_rg(root, rg, false, tokens)?;
    if !include_ignored {
        return Ok(primary);
    }
    let ignored = run_rg(root, rg, true, tokens)?;
    Ok(merge_ignored(primary, ignored, tokens))
}

fn merge_ignored(primary: CandidateSet, ignored: CandidateSet, tokens: &[&str]) -> CandidateSet {
    let mut seen = HashSet::new();
    let mut substrings = Vec::new();
    let mut fuzzy = Vec::new();
    for path in primary.items.into_iter().chain(ignored.items) {
        if !seen.insert(path.clone()) {
            continue;
        }
        let lower = path.to_lowercase();
        if tokens.is_empty() || tokens.iter().all(|token| lower.contains(token)) {
            substrings.push(path);
        } else {
            fuzzy.push(path);
        }
    }
    let truncated =
        primary.truncated || ignored.truncated || substrings.len() + fuzzy.len() > MAX_FILES;
    substrings.extend(fuzzy);
    substrings.truncate(MAX_FILES);
    CandidateSet {
        items: substrings,
        truncated,
    }
}

fn run_rg(root: &Path, rg: &Path, ignored_pass: bool, tokens: &[&str]) -> Result<CandidateSet> {
    let mut command = Command::new(rg);
    command.current_dir(root).args([
        "--files",
        "--no-config",
        "--hidden",
        "--null",
        "--path-separator",
        "/",
    ]);
    if ignored_pass {
        command.arg("--no-ignore-vcs");
    }
    for directory in HIDDEN_DIRS {
        command.args(["--glob", &format!("!**/{directory}")]);
    }
    for directory in ROOT_HIDDEN_DIRS {
        command.args(["--glob", &format!("!/{directory}")]);
    }
    command.args(["--glob", "!**/node_modules"]);
    for path in HIDDEN_PATHS {
        command.args(["--glob", &format!("!**/{path}")]);
    }
    command.arg(".");
    let output = command
        .output()
        .with_context(|| format!("cannot run ripgrep at {}", rg.display()))?;
    if output.status.code() == Some(1) {
        return Ok(CandidateSet::default());
    }
    if !output.status.success() {
        anyhow::bail!(
            "ripgrep failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(parse_null_paths(&output.stdout, tokens))
}

fn list_with_git(root: &Path, include_ignored: bool, tokens: &[&str]) -> Result<CandidateSet> {
    let mut command = Command::new("git");
    command.current_dir(root).args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        ".",
    ]);
    let output = command.output().context("cannot run git ls-files")?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut candidates = parse_null_paths(&output.stdout, tokens);
    candidates.items.retain(|path| root.join(path).is_file());
    if include_ignored {
        let mut command = Command::new("git");
        command.current_dir(root).args([
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--",
            ".",
        ]);
        let output = command.output().context("cannot run git ls-files")?;
        if !output.status.success() {
            anyhow::bail!(
                "git ls-files ignored pass failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut ignored = parse_null_paths(&output.stdout, tokens);
        ignored.items.retain(|path| root.join(path).is_file());
        candidates = merge_ignored(candidates, ignored, tokens);
    }
    Ok(candidates)
}

fn parse_null_paths(bytes: &[u8], tokens: &[&str]) -> CandidateSet {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut fuzzy = Vec::new();
    let mut truncated = false;
    for part in bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
    {
        let raw = match std::str::from_utf8(part) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let path = if cfg!(windows) {
            raw.replace('\\', "/")
        } else {
            raw.to_string()
        };
        let path = path.strip_prefix("./").unwrap_or(&path);
        if path.is_empty() || path.starts_with('/') || path.starts_with("../") {
            continue;
        }
        if !should_include_path(path) {
            continue;
        }
        let lower = path.to_lowercase();
        let substring = tokens.iter().all(|token| lower.contains(token));
        let fuzzy_match = !substring
            && !tokens.is_empty()
            && tokens.iter().all(|token| is_subsequence(token, &lower));
        if !substring && !fuzzy_match {
            continue;
        }
        if !seen.insert(path.to_string()) {
            continue;
        }
        if substring {
            if items.len() < MAX_FILES {
                items.push(path.to_string());
            } else {
                truncated = true;
            }
        } else if fuzzy.len() < MAX_FILES {
            fuzzy.push(path.to_string());
        } else {
            truncated = true;
        }
    }
    items.extend(fuzzy);
    if items.len() > MAX_FILES {
        items.truncate(MAX_FILES);
        truncated = true;
    }
    CandidateSet { items, truncated }
}

pub(crate) fn should_include_path(path: &str) -> bool {
    if HIDDEN_PATHS.iter().any(|hidden| {
        path == *hidden
            || path.starts_with(&format!("{hidden}/"))
            || path.ends_with(&format!("/{hidden}"))
            || path.contains(&format!("/{hidden}/"))
    }) {
        return false;
    }
    if ROOT_HIDDEN_DIRS
        .iter()
        .any(|directory| path == *directory || path.starts_with(&format!("{directory}/")))
    {
        return false;
    }
    !path
        .split('/')
        .any(|segment| segment == "node_modules" || HIDDEN_DIRS.contains(&segment))
}

fn matches_lower_path(path: &str, tokens: &[&str]) -> bool {
    if tokens.is_empty() {
        return true;
    }
    tokens.iter().all(|token| path.contains(token))
        || tokens.iter().all(|token| is_subsequence(token, path))
}

fn matches_any(path: &str, tokens: &[&str]) -> bool {
    matches_lower_path(&path.to_lowercase(), tokens)
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = needle.chars();
    let Some(mut current) = chars.next() else {
        return true;
    };
    for character in haystack.chars() {
        if character == current {
            current = match chars.next() {
                Some(next) => next,
                None => return true,
            };
        }
    }
    false
}

fn subsequence_positions(needle: &str, haystack: &str) -> Option<Vec<usize>> {
    if needle.is_empty() {
        return Some(Vec::new());
    }
    let mut positions = Vec::new();
    let mut chars = needle.chars();
    let mut current = chars.next();
    for (index, character) in haystack.chars().enumerate() {
        if Some(character) == current {
            positions.push(index);
            current = chars.next();
            if current.is_none() {
                return Some(positions);
            }
        }
    }
    None
}

/// Character positions in `path` that match `query`, for picker highlighting.
pub fn match_positions(path: &str, query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return Vec::new();
    }
    let lower = path.to_lowercase();
    if path.chars().count() != lower.chars().count() {
        return Vec::new();
    }
    let mut positions = Vec::new();
    for token in &tokens {
        if let Some(byte) = lower.rfind(token) {
            let start = lower[..byte].chars().count();
            positions.extend(start..start + token.chars().count());
        } else if let Some(found) = subsequence_positions(token, &lower) {
            positions.extend(found);
        } else {
            return Vec::new();
        }
    }
    positions.sort_unstable();
    positions.dedup();
    positions
}

fn sorted_indices(
    entries: &[String],
    query: &str,
    tokens: &[&str],
    recent: &std::collections::HashMap<String, usize>,
) -> Vec<usize> {
    let mut indices = entries
        .iter()
        .enumerate()
        .filter(|(_, path)| matches_any(path, tokens))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    indices.sort_by_cached_key(|index| {
        (
            rank_key(&entries[*index], query, tokens, recent),
            entries[*index].clone(),
        )
    });
    indices
}

/// Ranked indices for a query, shared by the CLI and the TUI picker.
pub fn ranked_indices(entries: &[String], query: &str) -> Vec<usize> {
    ranked_indices_with_recent(entries, query, &[])
}

/// Ranked indices with recently picked paths breaking ties.
pub fn ranked_indices_with_recent(
    entries: &[String],
    query: &str,
    recent: &[String],
) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let recent = recent
        .iter()
        .enumerate()
        .map(|(index, path)| (path.to_lowercase(), index))
        .collect::<std::collections::HashMap<_, _>>();
    sorted_indices(entries, &query, &tokens, &recent)
}

#[cfg(test)]
fn rank_candidates(candidates: Vec<String>, query: Option<&str>) -> Vec<String> {
    rank_candidates_with_recent(candidates, query, &std::collections::HashMap::new())
}

fn rank_candidates_with_recent(
    candidates: Vec<String>,
    query: Option<&str>,
    recent: &std::collections::HashMap<String, usize>,
) -> Vec<String> {
    let query = query.map(str::trim).unwrap_or("").to_lowercase();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let indices = sorted_indices(&candidates, &query, &tokens, recent);
    let mut slots = candidates.into_iter().map(Some).collect::<Vec<_>>();
    indices
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

fn rank_key(
    path: &str,
    query: &str,
    tokens: &[&str],
    recent: &std::collections::HashMap<String, usize>,
) -> (u8, usize, usize, String) {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let path = path.to_lowercase();
    let score = if query.is_empty() {
        3
    } else if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else if tokens.iter().all(|token| name.contains(token)) {
        2
    } else if tokens.iter().all(|token| path.contains(token)) {
        3
    } else if tokens.iter().all(|token| is_subsequence(token, &name)) {
        4
    } else if tokens.iter().all(|token| is_subsequence(token, &path)) {
        5
    } else {
        6
    };
    let recency = recent.get(&path).copied().unwrap_or(usize::MAX);
    let length = if score >= 4 { path.chars().count() } else { 0 };
    (score, recency, length, path)
}

fn escapes_next(input: &str, index: usize) -> bool {
    let mut following = input[index + 1..].chars();
    match following.next() {
        None => false,
        Some(character) => character == '\'' || character == '"' || character.is_whitespace(),
    }
}

#[cfg(not(windows))]
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./:=@%+,".contains(character))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || "-_./:=@%+,\\:".contains(character)
        })
    {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_filters_tokens_and_prefers_file_names() {
        let candidates = vec![
            "src/other.rs".to_string(),
            "docs/parser.md".to_string(),
            "src/parser/mod.rs".to_string(),
        ];
        let ranked = rank_candidates(candidates, Some("parser"));
        assert_eq!(ranked, vec!["docs/parser.md", "src/parser/mod.rs"]);

        let candidates = vec![
            "aaa/parser/notes.md".to_string(),
            "zzz_parser.md".to_string(),
        ];
        let ranked = rank_candidates(candidates, Some("parser md"));
        assert_eq!(ranked, vec!["zzz_parser.md", "aaa/parser/notes.md"]);
        let single = vec!["src/parser/mod.rs".to_string()];
        assert_eq!(ranked_indices(&single, "PARSER rs"), vec![0]);
        assert_eq!(ranked_indices(&single, "prsr"), vec![0]);

        let entries = vec![
            "aaa/parser/notes.md".to_string(),
            "zzz_parser.md".to_string(),
        ];
        let indices = ranked_indices(&entries, "parser md");
        assert_eq!(indices, vec![1, 0]);
    }

    #[test]
    fn subsequence_queries_match_in_order() {
        let ranked = rank_candidates(
            vec!["src/parser/mod.rs".to_string(), "src/other.rs".to_string()],
            Some("pmod"),
        );
        assert_eq!(ranked, vec!["src/parser/mod.rs"]);
        let out_of_order = rank_candidates(vec!["src/parser/mod.rs".to_string()], Some("dopm"));
        assert!(out_of_order.is_empty());

        let ranked = rank_candidates(
            vec!["zzz/parser/mod.rs".to_string(), "pm_o_d.rs".to_string()],
            Some("pmod"),
        );
        assert_eq!(ranked, vec!["pm_o_d.rs", "zzz/parser/mod.rs"]);

        assert_eq!(
            match_positions("src/parser/mod.rs", "pmod"),
            vec![4, 11, 12, 13]
        );
        assert_eq!(
            match_positions("src/parser/mod.rs", "parser"),
            vec![4, 5, 6, 7, 8, 9]
        );
        assert!(match_positions("src/parser/mod.rs", "zzz").is_empty());
        assert_eq!(match_positions("mod/x/mod.rs", "mod"), vec![6, 7, 8]);
        assert!(match_positions("İ.rs", "rs").is_empty());

        assert_eq!(
            ranked_indices(&["src/parser/mod.rs".to_string()], "pmod rs"),
            vec![0]
        );
        assert_eq!(
            ranked_indices(&["src/parser/mod.rs".to_string()], "rs pmod"),
            vec![0]
        );
        assert!(ranked_indices(&["src/parser/mod.rs".to_string()], "pmod rz").is_empty());
        assert_eq!(
            match_positions("src/parser/mod.rs", "pmod rs"),
            vec![4, 11, 12, 13, 15, 16]
        );
        assert_eq!(
            match_positions("src/parser/mod.rs", "rs pmod"),
            vec![4, 11, 12, 13, 15, 16]
        );
        let ranked = rank_candidates(
            vec!["src/parser/mod.rs".to_string(), "pm_o_d.rs".to_string()],
            Some("pmod rs"),
        );
        assert_eq!(ranked, vec!["pm_o_d.rs", "src/parser/mod.rs"]);
        let ranked = rank_candidates(
            vec!["pm_o_d.rs".to_string(), "src/pmod_rs/notes.txt".to_string()],
            Some("pmod rs"),
        );
        assert_eq!(ranked, vec!["src/pmod_rs/notes.txt", "pm_o_d.rs"]);
        let ranked = rank_candidates(
            vec!["notes/daily.rs".to_string(), "daily notes.md".to_string()],
            Some("daily notes"),
        );
        assert_eq!(ranked, vec!["daily notes.md", "notes/daily.rs"]);
        assert_eq!(
            ranked_indices(&["src/parser/mod.rs".to_string()], "PMOD"),
            vec![0]
        );
        let ranked = rank_candidates(
            vec!["pm_o_d.rs".to_string(), "pmod.rs".to_string()],
            Some("pmod"),
        );
        assert_eq!(ranked, vec!["pmod.rs", "pm_o_d.rs"]);
    }

    #[test]
    fn substring_matches_survive_a_fuzzy_cap() {
        let mut bytes = Vec::new();
        for index in 0..(MAX_FILES + 1) {
            bytes.extend_from_slice(format!("vendor/p_m_o_d_{index}.lock\0").as_bytes());
        }
        bytes.extend_from_slice(b"src/pmod.rs\0");
        let candidates = parse_null_paths(&bytes, &["pmod"]);
        assert!(candidates.truncated);
        assert_eq!(
            candidates.items.first().map(String::as_str),
            Some("src/pmod.rs")
        );
        assert!(candidates.items.len() <= MAX_FILES);

        let mut bytes = Vec::new();
        for index in 0..(MAX_FILES + 1) {
            bytes.extend_from_slice(format!("vendor/p_m_o_d_r_s_{index}.lock\0").as_bytes());
        }
        bytes.extend_from_slice(b"src/pmod.rs\0");
        let candidates = parse_null_paths(&bytes, &["pmod", "rs"]);
        assert!(candidates.truncated);
        assert_eq!(
            candidates.items.first().map(String::as_str),
            Some("src/pmod.rs")
        );
        assert!(candidates.items.len() <= MAX_FILES);

        let mut primary_items = (0..(MAX_FILES - 1))
            .map(|index| format!("src/pmod_rs_{index}.rs"))
            .collect::<Vec<_>>();
        primary_items.push("vendor/p_m_o_d_r_s.lock".to_string());
        let primary = CandidateSet {
            items: primary_items,
            truncated: false,
        };
        let ignored = CandidateSet {
            items: vec!["src/ignored_pmod_rs.rs".into()],
            truncated: false,
        };
        let merged = merge_ignored(primary, ignored, &["pmod", "rs"]);
        assert!(merged.truncated);
        assert!(merged.items.contains(&"src/ignored_pmod_rs.rs".to_string()));
        assert!(!merged
            .items
            .contains(&"vendor/p_m_o_d_r_s.lock".to_string()));
        assert_eq!(merged.items.len(), MAX_FILES);
    }

    #[test]
    fn invalid_utf8_paths_are_skipped() {
        let candidates = parse_null_paths(b"ok.txt\0bad\xFF.txt\0", &[]);
        assert_eq!(candidates.items, vec!["ok.txt"]);
    }

    #[test]
    fn recent_paths_break_ties_without_beating_match_quality() {
        let recent = ["src/main.rs".to_string()];
        let recent_map = recent
            .iter()
            .enumerate()
            .map(|(index, path)| (path.to_lowercase(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let ranked = rank_candidates_with_recent(
            vec!["README.md".to_string(), "src/main.rs".to_string()],
            None,
            &recent_map,
        );
        assert_eq!(ranked, vec!["src/main.rs", "README.md"]);

        let recent = ["src/notes_readme.rs".to_string()];
        let recent_map = recent
            .iter()
            .enumerate()
            .map(|(index, path)| (path.to_lowercase(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let ranked = rank_candidates_with_recent(
            vec!["src/notes_readme.rs".to_string(), "README.md".to_string()],
            Some("readme"),
            &recent_map,
        );
        assert_eq!(ranked, vec!["README.md", "src/notes_readme.rs"]);

        let recent = ["src/Main.rs".to_string()];
        let recent_map = recent
            .iter()
            .enumerate()
            .map(|(index, path)| (path.to_lowercase(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let ranked = rank_candidates_with_recent(
            vec!["README.md".to_string(), "src/main.rs".to_string()],
            None,
            &recent_map,
        );
        assert_eq!(ranked, vec!["src/main.rs", "README.md"]);

        let recent = ["p/m/o/d/long_name_extra.rs".to_string()];
        let recent_map = recent
            .iter()
            .enumerate()
            .map(|(index, path)| (path.to_lowercase(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let ranked = rank_candidates_with_recent(
            vec![
                "p/m/o/d/short.rs".to_string(),
                "p/m/o/d/long_name_extra.rs".to_string(),
            ],
            Some("pmod"),
            &recent_map,
        );
        assert_eq!(
            ranked,
            vec!["p/m/o/d/long_name_extra.rs", "p/m/o/d/short.rs"]
        );
    }

    #[test]
    fn ignored_paths_rank_with_nonignored_paths() {
        let primary = CandidateSet {
            items: vec!["src/main.rs".into()],
            truncated: false,
        };
        let ignored = CandidateSet {
            items: vec![".env".into(), "src/main.rs".into()],
            truncated: false,
        };
        let merged = merge_ignored(primary, ignored, &[]);
        assert_eq!(merged.items, vec!["src/main.rs", ".env"]);
        let ranked = rank_candidates(merged.items, None);
        assert_eq!(ranked, vec![".env", "src/main.rs"]);
    }

    #[test]
    fn editor_commands_support_quoted_programs_and_paths() {
        let command = split_command_line("code --wait --user-data-dir='/tmp/a b'").unwrap();
        assert_eq!(command, vec!["code", "--wait", "--user-data-dir=/tmp/a b"]);
        let windows = split_command_line("\"C:\\Program Files\\Code\\code.exe\" --wait").unwrap();
        assert_eq!(windows, vec!["C:\\Program Files\\Code\\code.exe", "--wait"]);
        let empty = split_command_line("emacsclient -a \"\"").unwrap();
        assert_eq!(empty, vec!["emacsclient", "-a", ""]);
        let unc = split_command_line("\"\\\\server\\share\\code.exe\"").unwrap();
        assert_eq!(unc, vec!["\\\\server\\share\\code.exe"]);
        let open = build_open_command("vim -f", Path::new("/tmp/it's here.rs")).unwrap();
        assert_eq!(open.program, "vim");
        assert_eq!(open.args, vec!["-f", "/tmp/it's here.rs"]);
        assert_eq!(open.shell_line(), "vim -f '/tmp/it'\\''s here.rs'");
    }

    #[test]
    fn open_commands_apply_editor_specific_line_syntax() {
        let path = Path::new("/tmp/file.rs");
        let code = build_open_command_at_line("code --wait", path, Some(12)).unwrap();
        assert_eq!(code.program, "code");
        assert_eq!(code.args, vec!["--wait", "--goto", "/tmp/file.rs:12"]);

        let vim = build_open_command_at_line("vim -f", path, Some(12)).unwrap();
        assert_eq!(vim.args, vec!["-f", "+12", "/tmp/file.rs"]);

        let emacs = build_open_command_at_line("emacsclient -a \"\"", path, Some(7)).unwrap();
        assert_eq!(emacs.args, vec!["-a", "", "+7", "/tmp/file.rs"]);

        let nano = build_open_command_at_line("nano", path, Some(9)).unwrap();
        assert_eq!(nano.args, vec!["+9", "/tmp/file.rs"]);

        let helix = build_open_command_at_line("hx --vsplit", path, Some(3)).unwrap();
        assert_eq!(helix.args, vec!["--vsplit", "/tmp/file.rs:3"]);

        let unknown = build_open_command_at_line("my-editor --flag", path, Some(4)).unwrap();
        assert_eq!(unknown.args, vec!["--flag", "/tmp/file.rs:4"]);

        for editor in ["code-insiders", "codium", "cursor", "windsurf"] {
            let command = build_open_command_at_line(editor, path, Some(5)).unwrap();
            assert_eq!(command.args, vec!["--goto", "/tmp/file.rs:5"]);
        }

        let unscoped = build_open_command_at_line("code --wait", path, None).unwrap();
        assert_eq!(unscoped.args, vec!["--wait", "/tmp/file.rs"]);
        assert_eq!(
            editor_program_name("C:\\Program Files\\Code\\code.exe"),
            "code"
        );
        assert_eq!(editor_program_name("code.cmd"), "code");
        assert_eq!(editor_program_name("Code.BAT"), "code");
    }

    #[test]
    fn split_line_targets_are_shared_by_the_cli_and_picker() {
        assert_eq!(split_line_target("parser:42").1, Some(42));
        assert_eq!(split_line_target("parser:42").0, "parser");
        assert_eq!(split_line_target("app\\:12"), ("app:12".into(), None));
        assert_eq!(split_line_target("parser:0"), ("parser:0".into(), None));
    }

    #[test]
    fn visual_wins_over_editor_and_blank_values_are_ignored() {
        assert_eq!(
            editor_from_values(Some("code --wait"), Some("vim")),
            Some("code --wait".into())
        );
        assert_eq!(
            editor_from_values(Some("  "), Some("vim")),
            Some("vim".into())
        );
        assert_eq!(editor_from_values(None, None), None);
    }

    #[test]
    fn cache_paths_are_excluded_and_user_dotfiles_survive() {
        assert!(!should_include_path(".git/config"));
        assert!(!should_include_path("node_modules/pkg/index.js"));
        assert!(!should_include_path(".next/cache/data"));
        assert!(!should_include_path("target/debug/baton"));
        assert!(!should_include_path(
            ".venv/lib/python3.12/site-packages/mod.py"
        ));
        assert!(should_include_path("src/target/mod.rs"));
        assert!(should_include_path(".github/workflows/ci.yml"));
        assert!(should_include_path(".vscode/settings.json"));
        assert!(should_include_path(".idea/modules.xml"));
        assert!(should_include_path(".husky/pre-commit"));
        assert!(should_include_path(".env.example"));
    }

    #[test]
    fn query_matches_survive_past_the_cap_of_unmatched_candidates() {
        let mut bytes = Vec::new();
        for index in 0..(MAX_FILES + 1) {
            bytes.extend_from_slice(format!("vendor/dependency{index}.lock\0").as_bytes());
        }
        bytes.extend_from_slice(b"src/needle.rs\0");
        let candidates = parse_null_paths(&bytes, &["needle"]);
        assert_eq!(candidates.items.len(), 1);
        assert_eq!(candidates.items[0], "src/needle.rs");
        assert!(!candidates.truncated);
    }

    #[test]
    fn truncated_flag_tracks_matching_candidates_beyond_the_cap() {
        let mut bytes = Vec::new();
        for index in 0..(MAX_FILES + 1) {
            bytes.extend_from_slice(format!("src/needle{index}.rs\0").as_bytes());
        }
        let candidates = parse_null_paths(&bytes, &["needle"]);
        assert_eq!(candidates.items.len(), MAX_FILES);
        assert!(candidates.truncated);
    }

    #[test]
    fn merged_passes_deduplicate_and_share_one_inventory_cap() {
        let primary = CandidateSet {
            items: (0..MAX_FILES)
                .map(|index| format!("src/a{index}.rs"))
                .collect(),
            truncated: false,
        };
        let ignored = CandidateSet {
            items: vec![
                "src/a0.rs".into(),
                "notes.log".into(),
                "tmp/cache.log".into(),
            ],
            truncated: false,
        };
        let merged = merge_ignored(primary, ignored, &[]);
        assert_eq!(merged.items.len(), MAX_FILES);
        assert!(merged.truncated);
        assert!(merged.items.iter().all(|path| !path.ends_with(".log")));
    }

    #[test]
    fn merged_passes_keep_extra_rows_below_the_cap() {
        let primary = CandidateSet {
            items: vec!["a.rs".into()],
            truncated: false,
        };
        let ignored = CandidateSet {
            items: vec!["a.rs".into(), "b.log".into()],
            truncated: false,
        };
        let merged = merge_ignored(primary, ignored, &[]);
        assert_eq!(merged.items, vec!["a.rs", "b.log"]);
        assert!(!merged.truncated);
    }

    #[test]
    fn null_output_is_normalized_and_deduplicated() {
        let candidates = parse_null_paths(
            b"./src/main.rs\0src/main.rs\0../escape\0.git/config\0docs/read me.md\0",
            &[],
        );
        assert_eq!(candidates.items, vec!["src/main.rs", "docs/read me.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn unix_paths_keep_backslashes() {
        let candidates = parse_null_paths(b"a\\b.txt\0", &[]);
        assert_eq!(candidates.items, vec!["a\\b.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn project_paths_cannot_escape_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let inside = root.join("ok.txt");
        std::fs::write(&inside, "ok").unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "no").unwrap();
        assert_eq!(
            resolve_project_file(&root, Path::new("ok.txt")).unwrap(),
            inside.canonicalize().unwrap()
        );
        assert!(resolve_project_file(&root, Path::new("../outside.txt")).is_err());
    }

    #[test]
    fn git_fallback_lists_tracked_untracked_and_optional_ignored_files() {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/tracked.rs"), "tracked\n").unwrap();
        std::fs::write(dir.path().join("untracked.md"), "untracked\n").unwrap();
        std::fs::write(dir.path().join("ignored.log"), "ignored\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.log\n").unwrap();
        let status = Command::new("git")
            .args(["add", "src/tracked.rs", ".gitignore"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());

        let visible = list_with_git(dir.path(), false, &[]).unwrap();
        let visible = visible.items;
        assert!(visible.contains(&"src/tracked.rs".to_string()));
        assert!(visible.contains(&"untracked.md".to_string()));
        assert!(!visible.contains(&"ignored.log".to_string()));

        std::fs::write(dir.path().join("deleted.rs"), "deleted\n").unwrap();
        let status = Command::new("git")
            .args(["add", "deleted.rs"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_file(dir.path().join("deleted.rs")).unwrap();
        let visible = list_with_git(dir.path(), false, &[]).unwrap();
        assert!(!visible.items.contains(&"deleted.rs".to_string()));

        let all = list_with_git(dir.path(), true, &[]).unwrap();
        let all = all.items;
        assert!(all.contains(&"ignored.log".to_string()));
    }
}
