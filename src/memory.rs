use anyhow::Result;
use std::path::Path;

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
