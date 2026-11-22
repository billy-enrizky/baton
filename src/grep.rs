//! Project content search for terminal references.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::doctor;

pub const SCHEMA_VERSION: u32 = 1;
const MAX_PREVIEW_CHARS: usize = 300;

#[derive(Debug, Clone, Copy)]
pub struct ContentOptions<'a> {
    pub query: &'a str,
    pub limit: usize,
    pub case_sensitive: bool,
    pub word: bool,
    pub regex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentListing {
    pub matches: Vec<ContentMatch>,
    pub truncated: bool,
}

pub fn search(root: &Path, options: ContentOptions<'_>) -> Result<ContentListing> {
    if !root.is_dir() {
        anyhow::bail!("not a directory: {}", root.display());
    }
    let query = options.query.trim();
    if query.is_empty() {
        anyhow::bail!("grep needs a non-empty query");
    }
    let options = ContentOptions { query, ..options };
    if let Some(rg) = doctor::find_executable("rg") {
        search_with_rg(root, &rg, options)
    } else {
        search_with_git(root, options).context("baton grep needs ripgrep (rg) or a git repository")
    }
}

fn search_with_rg(root: &Path, rg: &Path, options: ContentOptions<'_>) -> Result<ContentListing> {
    let mut command = Command::new(rg);
    command.current_dir(root).args([
        "--json",
        "--color",
        "never",
        "--no-config",
        "--hidden",
        "--sort",
        "path",
        "--glob",
        "!**/.git",
        "--glob",
        "!**/node_modules",
    ]);
    add_common_flags(&mut command, options);
    command.arg("--").arg(options.query).arg(".");
    let output = command
        .output()
        .with_context(|| format!("cannot run ripgrep at {}", rg.display()))?;
    if output.status.code() == Some(1) {
        return Ok(empty_listing());
    }
    if !output.status.success() {
        anyhow::bail!(
            "ripgrep failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut matches = Vec::new();
    let mut truncated = false;
    for line in output.stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let value = match serde_json::from_slice::<serde_json::Value>(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value["type"] != "match" {
            continue;
        }
        let data = &value["data"];
        let Some(path) = data["path"]["text"].as_str() else {
            continue;
        };
        let Some(number) = data["line_number"].as_u64() else {
            continue;
        };
        let Some(text) = data["lines"]["text"].as_str() else {
            continue;
        };
        let path = normalize_path(path);
        if !crate::files::should_include_path(&path) {
            continue;
        }
        if !push_match(&mut matches, options.limit, &path, number, text) {
            truncated = true;
            break;
        }
    }
    matches.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    Ok(ContentListing { matches, truncated })
}

fn search_with_git(root: &Path, options: ContentOptions<'_>) -> Result<ContentListing> {
    let mut command = Command::new("git");
    command.current_dir(root).args([
        "-c",
        "grep.column=false",
        "-c",
        "grep.fullName=false",
        "grep",
        "-z",
        "-n",
        "-I",
        "--no-color",
        "--untracked",
    ]);
    if !options.case_sensitive {
        command.arg("-i");
    }
    if options.word {
        command.arg("-w");
    }
    command.arg(if options.regex { "-E" } else { "-F" });
    command.arg("--").arg(options.query);
    let output = command.output().context("cannot run git grep")?;
    if output.status.code() == Some(1) {
        return Ok(empty_listing());
    }
    if !output.status.success() {
        anyhow::bail!(
            "git grep failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut matches = Vec::new();
    let mut truncated = false;
    let bytes = &output.stdout;
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(path_len) = bytes[cursor..].iter().position(|byte| *byte == 0) else {
            break;
        };
        let path = &bytes[cursor..cursor + path_len];
        cursor += path_len + 1;
        let Some(line_len) = bytes[cursor..].iter().position(|byte| *byte == 0) else {
            break;
        };
        let line = &bytes[cursor..cursor + line_len];
        cursor += line_len + 1;
        let Some(text_len) = bytes[cursor..].iter().position(|byte| *byte == b'\n') else {
            break;
        };
        let text = &bytes[cursor..cursor + text_len];
        cursor += text_len + 1;
        let Ok(path) = std::str::from_utf8(path) else {
            continue;
        };
        let path = normalize_path(path);
        if !crate::files::should_include_path(&path) {
            continue;
        }
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(line) = line.trim().parse::<u64>() else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(text) else {
            continue;
        };
        if !push_match(&mut matches, options.limit, &path, line, text) {
            truncated = true;
            break;
        }
    }
    matches.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    Ok(ContentListing { matches, truncated })
}

fn add_common_flags(command: &mut Command, options: ContentOptions<'_>) {
    if options.case_sensitive {
        command.arg("--case-sensitive");
    } else {
        command.arg("--ignore-case");
    }
    if options.word {
        command.arg("--word-regexp");
    }
    if !options.regex {
        command.arg("--fixed-strings");
    }
}

fn push_match(
    matches: &mut Vec<ContentMatch>,
    limit: usize,
    path: &str,
    line: u64,
    text: &str,
) -> bool {
    if matches.len() >= limit {
        return false;
    }
    matches.push(ContentMatch {
        path: normalize_path(path),
        line,
        text: preview(text),
    });
    true
}

fn normalize_path(path: &str) -> String {
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    path.strip_prefix("./").unwrap_or(&path).to_string()
}

fn preview(text: &str) -> String {
    let text = text.trim_end_matches(['\n', '\r']);
    let mut characters = text.chars();
    let preview = characters
        .by_ref()
        .take(MAX_PREVIEW_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn empty_listing() -> ContentListing {
    ContentListing {
        matches: Vec::new(),
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        for (path, contents) in files {
            let path = dir.path().join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
        dir
    }

    fn options<'a>(query: &'a str) -> ContentOptions<'a> {
        ContentOptions {
            query,
            limit: 50,
            case_sensitive: false,
            word: false,
            regex: false,
        }
    }

    #[test]
    fn finds_matching_lines_across_files() {
        let dir = project(&[
            ("src/main.rs", "fn main() {\n    println!(\"hello\");\n}\n"),
            ("src/lib.rs", "pub fn helper() {}\n"),
            ("README.md", "# hello\n"),
        ]);
        let listing = search(dir.path(), options("hello")).unwrap();
        assert_eq!(
            listing.matches,
            vec![
                ContentMatch {
                    path: "README.md".into(),
                    line: 1,
                    text: "# hello".into(),
                },
                ContentMatch {
                    path: "src/main.rs".into(),
                    line: 2,
                    text: "    println!(\"hello\");".into(),
                },
            ]
        );
        assert!(!listing.truncated);
    }

    #[test]
    fn honors_case_word_and_regex_flags() {
        let dir = project(&[("src/text.txt", "Alpha alpha alphabet\n")]);
        let listing = search(
            dir.path(),
            ContentOptions {
                case_sensitive: true,
                ..options("alpha")
            },
        )
        .unwrap();
        assert_eq!(listing.matches.len(), 1);
        assert_eq!(listing.matches[0].text, "Alpha alpha alphabet");

        let listing = search(
            dir.path(),
            ContentOptions {
                word: true,
                ..options("alpha")
            },
        )
        .unwrap();
        assert_eq!(listing.matches.len(), 1);
        assert_eq!(listing.matches[0].text, "Alpha alpha alphabet");

        let listing = search(
            dir.path(),
            ContentOptions {
                regex: true,
                ..options("al.ha")
            },
        )
        .unwrap();
        assert_eq!(listing.matches.len(), 1);
        assert_eq!(listing.matches[0].text, "Alpha alpha alphabet");
    }

    #[test]
    fn caps_results_and_marks_truncation() {
        let dir = project(&[("src/text.txt", "x1\nx2\nx3\n")]);
        let listing = search(
            dir.path(),
            ContentOptions {
                limit: 2,
                ..options("x")
            },
        )
        .unwrap();
        assert_eq!(listing.matches.len(), 2);
        assert!(listing.truncated);
    }

    #[test]
    fn searches_hidden_project_files_and_skips_caches() {
        let dir = project(&[
            ("src/main.rs", "needle\n"),
            (".github/ci.yml", "needle\n"),
            (".cache/generated.txt", "needle\n"),
        ]);
        let listing = search(dir.path(), options("needle")).unwrap();
        let paths = listing
            .matches
            .iter()
            .map(|matched| matched.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&".github/ci.yml"), "{paths:?}");
        assert!(paths.contains(&"src/main.rs"), "{paths:?}");
        assert!(!paths.contains(&".cache/generated.txt"), "{paths:?}");
    }

    #[test]
    fn limit_returns_the_first_sorted_paths() {
        let dir = project(&[
            ("src/f5.txt", "needle\n"),
            ("src/f1.txt", "needle\n"),
            ("src/f3.txt", "needle\n"),
            ("src/f2.txt", "needle\n"),
            ("src/f4.txt", "needle\n"),
        ]);
        let listing = search(
            dir.path(),
            ContentOptions {
                limit: 2,
                ..options("needle")
            },
        )
        .unwrap();
        let paths = listing
            .matches
            .iter()
            .map(|matched| matched.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["src/f1.txt", "src/f2.txt"]);
        assert!(listing.truncated);
    }

    #[test]
    fn git_fallback_parses_multiple_matches() {
        let dir = project(&[
            ("a.txt", "needle one\n"),
            ("b.txt", "middle\nneedle three\n"),
            ("c.txt", "needle four\n"),
        ]);
        let listing = search_with_git(dir.path(), options("needle")).unwrap();
        assert_eq!(
            listing
                .matches
                .iter()
                .map(|matched| matched.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.txt", "b.txt", "c.txt"]
        );
        assert_eq!(listing.matches[1].line, 2);
        assert_eq!(listing.matches[1].text, "needle three");
    }

    #[test]
    fn git_fallback_ignores_column_and_fullname_config() {
        let dir = project(&[("sub/a.txt", "needle here\n")]);
        for (key, value) in [("grep.column", "true"), ("grep.fullName", "true")] {
            let status = Command::new("git")
                .args(["config", key, value])
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success());
        }
        let listing = search_with_git(&dir.path().join("sub"), options("needle")).unwrap();
        assert_eq!(listing.matches.len(), 1);
        assert_eq!(listing.matches[0].path, "a.txt");
        assert_eq!(listing.matches[0].text, "needle here");
    }

    #[test]
    fn skips_binary_and_gitignored_files() {
        let dir = project(&[
            ("src/keep.txt", "needle\n"),
            ("src/secret.txt", "needle\n"),
            (".gitignore", "src/secret.txt\n"),
        ]);
        std::fs::write(dir.path().join("src/blob.bin"), b"needle\0binary\n").unwrap();
        let listing = search(dir.path(), options("needle")).unwrap();
        assert_eq!(
            listing
                .matches
                .iter()
                .map(|matched| matched.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/keep.txt"]
        );
    }

    #[test]
    fn rejects_an_empty_query_or_bad_root() {
        let dir = project(&[]);
        assert!(search(dir.path(), options("   ")).is_err());
        assert!(search(&dir.path().join("missing"), options("needle")).is_err());
    }

    #[test]
    fn missing_matches_are_an_empty_listing() {
        let dir = project(&[("src/main.rs", "fn main() {}\n")]);
        let listing = search(dir.path(), options("absent")).unwrap();
        assert!(listing.matches.is_empty());
        assert!(!listing.truncated);
    }
}
