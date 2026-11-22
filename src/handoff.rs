use std::path::Path;
use std::process::Command;

pub const DEFAULT_HANDOFF_CHAR_BUDGET: usize = 24000;
const PER_MESSAGE_CHAR_LIMIT: usize = 4000;

pub struct HandoffMessage {
    pub role: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub text: String,
}

pub fn build_handoff(cwd: &str, thread_title: &str, messages: &[HandoffMessage], git_status: Option<&str>, char_budget: Option<usize>) -> String {
    let budget = char_budget.unwrap_or(DEFAULT_HANDOFF_CHAR_BUDGET);
    let mut sections: Vec<String> = Vec::new();
    sections.push("[baton context handoff]".to_string());
    sections.push(
        "You are continuing an existing task in a new harness. The prior conversation below is your context. Pick up where it left off."
            .to_string(),
    );
    sections.push(String::new());
    sections.push("## Project".to_string());
    sections.push(format!("Title: {thread_title}"));
    sections.push(format!("Working directory: {cwd}"));
    sections.push(String::new());
    sections.push("## Prior conversation (oldest first)".to_string());
    let fixed_estimate: usize = sections.iter().map(|section| section.len() + 1).sum::<usize>() + 200;
    let git_len = git_status.map(|status| status.len()).unwrap_or(0);
    let rendered = render_messages(messages, budget.saturating_sub(fixed_estimate + git_len));
    if rendered.is_empty() {
        sections.push("(no prior messages)".to_string());
    } else {
        sections.push(rendered);
    }
    sections.push(String::new());
    sections.push("## Working tree state".to_string());
    match git_status {
        Some(status) if !status.trim().is_empty() => sections.push(status.trim().to_string()),
        _ => sections.push("clean".to_string()),
    }
    sections.push(String::new());
    sections.push("[end handoff]".to_string());
    sections.join("\n")
}

pub fn wrap_prompt_with_handoff(handoff: &str, prompt: &str) -> String {
    format!("{handoff}\n\n---\n\n{prompt}")
}

pub fn handoff_prompt(
    cwd: &str,
    thread_title: &str,
    prior_messages: &[HandoffMessage],
    git_status: Option<&str>,
    prompt: &str,
    char_budget: Option<usize>,
) -> String {
    let handoff = build_handoff(cwd, thread_title, prior_messages, git_status, char_budget);
    wrap_prompt_with_handoff(&handoff, prompt)
}

fn truncate_at_boundary(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn render_messages(messages: &[HandoffMessage], budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        let mut text = message.text.clone();
        if text.len() > PER_MESSAGE_CHAR_LIMIT {
            text = format!(
                "{}\n[...truncated]",
                truncate_at_boundary(&text, PER_MESSAGE_CHAR_LIMIT)
            );
        }
        let speaker = if message.role == "user" {
            "user".to_string()
        } else {
            format!("assistant ({}/{})", message.provider.as_deref().unwrap_or("?"), message.model.as_deref().unwrap_or("?"))
        };
        let block = format!("[{speaker}]\n{text}");
        if used + block.len() > budget && !lines.is_empty() {
            break;
        }
        used += block.len() + 1;
        lines.insert(0, block);
    }
    if lines.len() < messages.len() {
        let omitted = messages.len() - lines.len();
        lines.insert(0, format!("[...{omitted} earlier messages omitted]"));
    }
    lines.join("\n\n")
}

pub fn git_status_summary(cwd: &Path, max_lines: usize) -> Option<String> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--branch"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    let capped: Vec<&str> = lines.iter().take(max_lines).copied().collect();
    if capped.is_empty() {
        None
    } else {
        Some(capped.join("\n"))
    }
}

pub fn derive_title(text: &str) -> String {
    let first_line = text.trim().lines().next().unwrap_or("");
    let title: String = first_line.chars().take(80).collect();
    let title = title.trim();
    if title.is_empty() {
        "New thread".to_string()
    } else {
        title.to_string()
    }
}

pub fn git_branch_label(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}
