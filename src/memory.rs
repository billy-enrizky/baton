use anyhow::Result;
use std::path::Path;

use crate::store::MemoryRow;

const MEMORY_INJECTION_LIMIT: usize = 4000;

/// Formats the governed memories as one prompt block. Project entries come
/// first because the store orders them before global entries.
pub fn format_memories(memories: &[MemoryRow]) -> String {
    if memories.is_empty() {
        return "[memories]\n(none saved)\n[/memories]".to_string();
    }
    let mut block = String::from("[memories]\n");
    for memory in memories {
        let line = format!("- ({}) {}\n", memory.scope, memory.text.replace('\n', " "));
        if block.len() + line.len() > MEMORY_INJECTION_LIMIT {
            block.push_str("- (truncated at the memory injection limit)\n");
            break;
        }
        block.push_str(&line);
    }
    block.push_str("[/memories]");
    block
}

/// Replaces every `$memories` token with the current project and global
/// memories. A prompt without the token is unchanged.
pub fn expand_memories(memories: &[MemoryRow], text: &str) -> String {
    if !text.contains("$memories") {
        return text.to_string();
    }
    text.replace("$memories", &format_memories(memories))
}

pub fn find_memory_file(cwd: &Path) -> Option<(String, std::path::PathBuf)> {
    let candidates = [
        ("CLAUDE.md", cwd.join("CLAUDE.md")),
        ("AGENTS.md", cwd.join("AGENTS.md")),
        ("CLAUDE.md", cwd.join(".claude").join("CLAUDE.md")),
        ("AGENTS.md", cwd.join(".codex").join("AGENTS.md")),
    ];
    for (name, path) in &candidates {
        if path.is_file() {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if !contents.trim().is_empty() {
                    return Some((name.to_string(), path.clone()));
                }
            }
        }
    }
    None
}

pub fn read_memory_contents(cwd: &Path, max_chars: usize) -> Option<String> {
    let (_, path) = find_memory_file(cwd)?;
    let contents = std::fs::read_to_string(path).ok()?;
    let trimmed = if contents.len() > max_chars {
        format!("{}\n[...memory file truncated at {max_chars} chars]", &contents[..max_chars])
    } else {
        contents
    };
    Some(trimmed)
}

pub fn init_memory_file(cwd: &Path) -> Result<String> {
    let path = cwd.join("CLAUDE.md");
    if path.exists() {
        return Ok(format!("{} already exists", path.display()));
    }
    let template = r#"# CLAUDE.md

This file provides guidance to coding agents working in this repository.

## Project overview

Describe what this project does and its architecture.

## Key conventions

- Build and test commands
- Code style rules
- Important patterns to follow

## Common tasks

- How to run the app
- How to run tests
- How to deploy
"#;
    std::fs::write(&path, template)?;
    Ok(format!("created {}", path.display()))
}

pub fn build_compact_summary(
    messages: &[(String, String)],
    max_chars: usize,
) -> String {
    let mut sections: Vec<String> = Vec::new();
    sections.push("[baton compaction] The conversation below is summarized to free context. Continue from this state.".to_string());
    sections.push(String::new());

    let mut current: Vec<String> = Vec::new();
    let mut used = 0usize;
    for (role, text) in messages.iter().rev() {
        let entry_len = role.len() + text.len() + 3;
        if used + entry_len > max_chars && !current.is_empty() {
            break;
        }
        current.insert(0, format!("[{role}] {text}"));
        used += entry_len + 1;
    }
    if current.len() < messages.len() {
        let omitted = messages.len() - current.len();
        sections.push(format!("[...{omitted} earlier messages compacted]"));
    }
    sections.extend(current);
    sections.push(String::new());
    sections.push("[end compaction]".to_string());
    sections.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, scope: &str, text: &str) -> MemoryRow {
        MemoryRow {
            id,
            scope: scope.to_string(),
            cwd: String::new(),
            text: text.to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn expands_project_and_global_memories_in_order() {
        let memories = vec![row(1, "project", "Use tabs"), row(2, "global", "Answer briefly")];
        let expanded = expand_memories(&memories, "hello $memories world");
        assert!(expanded.starts_with("hello [memories]"));
        assert!(expanded.contains("- (project) Use tabs"));
        assert!(expanded.contains("- (global) Answer briefly"));
        assert!(expanded.contains("[/memories] world"));
        assert!(!expanded.contains("$memories"));
    }

    #[test]
    fn a_prompt_without_the_token_is_unchanged() {
        let memories = vec![row(1, "project", "Use tabs")];
        assert_eq!(expand_memories(&memories, "hello"), "hello");
    }

    #[test]
    fn an_empty_memory_set_leaves_a_visible_marker() {
        let expanded = expand_memories(&[], "$memories");
        assert_eq!(expanded, "[memories]\n(none saved)\n[/memories]");
    }
}
