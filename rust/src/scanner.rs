use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::types::ProviderId;

#[derive(Debug, Clone)]
pub struct DiscoveredMessage {
    pub role: String,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    pub provider: ProviderId,
    pub native_session_id: String,
    pub title: String,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub path: PathBuf,
    pub last_active_ms: i64,
    pub messages: Vec<DiscoveredMessage>,
}

const MAX_RECORDS: usize = 4000;
const MAX_MESSAGES: usize = 200;
const MAX_SCAN_FILES: usize = 500;

fn extract_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.trim().to_string(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("type").and_then(Value::as_str))
            .zip(blocks.iter())
            .filter(|(block_type, _)| *block_type == "text" || *block_type == "input_text" || *block_type == "output_text")
            .filter_map(|(_, block)| block.get("text").and_then(Value::as_str))
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn normalize_timestamp(value: Option<&str>, fallback: &str) -> String {
    match value {
        Some(value) => match chrono::DateTime::parse_from_rfc3339(value) {
            Ok(parsed) => parsed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            Err(_) => fallback.to_string(),
        },
        None => fallback.to_string(),
    }
}

fn codex_turn_id(metadata: &Value) -> Option<String> {
    let turn_id = metadata.get("turn_id")?.as_str()?.trim();
    if turn_id.is_empty() {
        None
    } else {
        Some(turn_id.to_string())
    }
}

fn is_context_injection(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("# AGENTS.md") || trimmed.starts_with("<environment_context>")
}

fn finish_session(
    provider: ProviderId,
    provider_session_id: String,
    title: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
    messages: Vec<(String, String, String, bool)>,
    _fallback_timestamp: &str,
    path: PathBuf,
    last_active_ms: i64,
) -> Option<DiscoveredSession> {
    if provider_session_id.trim().is_empty() {
        return None;
    }
    if !messages.iter().any(|(role, _, _, _)| role == "user") {
        return None;
    }
    let visible: Vec<DiscoveredMessage> = messages
        .into_iter()
        .map(|(role, text, created_at, _)| DiscoveredMessage {
            role,
            text,
            created_at,
        })
        .collect();
    let title_candidate = title.or_else(|| {
        visible
            .iter()
            .filter(|message| message.role == "user" && !is_context_injection(&message.text))
            .filter_map(|message| message.text.trim().lines().next())
            .map(|line| line.chars().take(100).collect::<String>().trim().to_string())
            .find(|line| !line.is_empty())
    });
    Some(DiscoveredSession {
        provider,
        native_session_id: provider_session_id,
        title: title_candidate.filter(|title| !title.is_empty()).unwrap_or_else(|| "Imported thread".to_string()),
        model,
        cwd,
        path,
        last_active_ms,
        messages: visible,
    })
}

pub fn parse_claude_transcript(contents: &str, path: &Path, last_active_ms: i64, fallback_session_id: &str) -> Option<DiscoveredSession> {
    let lines: Vec<&str> = contents.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.is_empty() || lines.len() > MAX_RECORDS {
        return None;
    }
    let fallback_timestamp = chrono::DateTime::from_timestamp_millis(last_active_ms)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    let mut provider_session_id = fallback_session_id.to_string();
    let mut title: Option<String> = None;
    let mut model: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut messages: Vec<(String, String, String, bool)> = Vec::new();

    for line in lines {
        let record: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if record.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || record.get("isMeta").and_then(Value::as_bool) == Some(true)
            || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        if let Some(session_id) = record.get("sessionId").and_then(Value::as_str) {
            if !session_id.trim().is_empty() {
                provider_session_id = session_id.trim().to_string();
            }
        }
        if let Some(ai_title) = record.get("aiTitle").and_then(Value::as_str) {
            if !ai_title.trim().is_empty() {
                title = Some(ai_title.trim().to_string());
            }
        }
        if let Some(record_cwd) = record.get("cwd").and_then(Value::as_str) {
            if !record_cwd.trim().is_empty() {
                cwd = Some(record_cwd.trim().to_string());
            }
        }
        let message = record.get("message");
        if let Some(message_model) = message.and_then(|message| message.get("model")).and_then(Value::as_str) {
            let trimmed = message_model.trim();
            if !trimmed.is_empty() && trimmed != "<synthetic>" {
                model = Some(trimmed.to_string());
            }
        }
        let record_type = record.get("type").and_then(Value::as_str).unwrap_or("");
        if record_type != "user" && record_type != "assistant" {
            continue;
        }
        let text = extract_text(message.and_then(|message| message.get("content")).unwrap_or(&Value::Null));
        if text.is_empty() {
            continue;
        }
        let created_at = normalize_timestamp(
            record.get("timestamp").and_then(Value::as_str),
            &fallback_timestamp,
        );
        messages.push((record_type.to_string(), text, created_at, false));
        if messages.len() > MAX_MESSAGES {
            messages.remove(0);
        }
    }

    finish_session(
        ProviderId::Claude,
        provider_session_id,
        title,
        model,
        cwd,
        messages,
        &fallback_timestamp,
        path.to_path_buf(),
        last_active_ms,
    )
}

pub fn parse_codex_transcript(contents: &str, path: &Path, last_active_ms: i64) -> Option<DiscoveredSession> {
    let lines: Vec<&str> = contents.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.is_empty() || lines.len() > MAX_RECORDS {
        return None;
    }
    let fallback_timestamp = chrono::DateTime::from_timestamp_millis(last_active_ms)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    let mut provider_session_id = String::new();
    let mut has_session_id = false;
    let mut model: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut messages: Vec<(String, String, String, bool)> = Vec::new();

    let mut records: Vec<Value> = Vec::new();
    for line in lines {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            records.push(value);
        }
    }

    let mut canonical_response_user_turns: HashSet<String> = HashSet::new();
    {
        let mut canonical_user_texts: HashSet<String> = HashSet::new();
        let mut response_users: Vec<(String, String)> = Vec::new();
        for record in &records {
            let record_type = record.get("type").and_then(Value::as_str).unwrap_or("");
            let payload = record.get("payload");
            let payload_type = payload.and_then(|payload| payload.get("type")).and_then(Value::as_str);
            let payload_role = payload.and_then(|payload| payload.get("role")).and_then(Value::as_str);
            if record_type == "response_item" && payload_type == Some("message") && payload_role == Some("assistant") {
                let canonical_turn_ids: HashSet<String> = response_users
                    .iter()
                    .filter(|(_turn_id, text)| canonical_user_texts.contains(text))
                    .map(|(turn_id, _)| turn_id.clone())
                    .collect();
                canonical_response_user_turns.extend(canonical_turn_ids);
                canonical_user_texts.clear();
                response_users.clear();
            } else if record_type == "event_msg" && payload_type == Some("user_message") {
                let text = payload
                    .and_then(|payload| payload.get("message"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                if !text.is_empty() {
                    canonical_user_texts.insert(text.to_string());
                }
            } else if record_type == "response_item" && payload_type == Some("message") && payload_role == Some("user") {
                let turn_id = payload
                    .and_then(|payload| payload.get("internal_chat_message_metadata_passthrough"))
                    .and_then(codex_turn_id);
                let text = extract_text(payload.and_then(|payload| payload.get("content")).unwrap_or(&Value::Null));
                if let Some(turn_id) = turn_id {
                    if !text.is_empty() {
                        response_users.push((turn_id, text));
                    }
                }
            }
        }
        let canonical_turn_ids: HashSet<String> = response_users
            .iter()
            .filter(|(_turn_id, text)| canonical_user_texts.contains(text))
            .map(|(turn_id, _)| turn_id.clone())
            .collect();
        canonical_response_user_turns.extend(canonical_turn_ids);
    }

    for record in &records {
        let record_type = record.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = record.get("payload").cloned().unwrap_or(Value::Null);
        let payload_type = payload.get("type").and_then(Value::as_str);
        if record_type == "session_meta" {
            let session_id = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(session_id) = session_id {
                if !has_session_id {
                    provider_session_id = session_id.to_string();
                    has_session_id = true;
                }
            }
            if let Some(payload_cwd) = payload.get("cwd").and_then(Value::as_str) {
                if !payload_cwd.trim().is_empty() {
                    cwd = Some(payload_cwd.trim().to_string());
                }
            }
            continue;
        }
        if record_type == "turn_context" {
            if let Some(context_model) = payload.get("model").and_then(Value::as_str) {
                let trimmed = context_model.trim();
                if !trimmed.is_empty() {
                    model = Some(trimmed.to_string());
                }
            }
            if let Some(payload_cwd) = payload.get("cwd").and_then(Value::as_str) {
                if !payload_cwd.trim().is_empty() {
                    cwd = Some(payload_cwd.trim().to_string());
                }
            }
            continue;
        }
        if record_type == "event_msg" && payload_type == Some("user_message") {
            let text = payload
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default();
            if text.trim().is_empty() {
                continue;
            }
            let mut index = messages.len();
            while index > 0 {
                index -= 1;
                let message = &messages[index];
                if message.0 == "assistant" {
                    break;
                }
                if message.3 && message.1.trim() == text.trim() {
                    messages.remove(index);
                    break;
                }
            }
            let created_at = normalize_timestamp(
                record.get("timestamp").and_then(Value::as_str),
                &fallback_timestamp,
            );
            messages.push(("user".to_string(), text, created_at, false));
            if messages.len() > MAX_MESSAGES {
                messages.remove(0);
            }
            continue;
        }
        if record_type != "response_item" || payload_type != Some("message") {
            continue;
        }
        let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = extract_text(payload.get("content").unwrap_or(&Value::Null));
        if text.is_empty() {
            continue;
        }
        if role == "user" {
            let turn_id = payload
                .get("internal_chat_message_metadata_passthrough")
                .and_then(codex_turn_id);
            let duplicated = turn_id
                .as_deref()
                .map(|turn_id| canonical_response_user_turns.contains(turn_id))
                .unwrap_or(false)
                || messages
                    .iter()
                    .rev()
                    .take_while(|message| message.0 != "assistant")
                    .any(|message| !message.3 && message.1.trim() == text.trim());
            if duplicated {
                continue;
            }
        }
        let created_at = normalize_timestamp(
            record.get("timestamp").and_then(Value::as_str),
            &fallback_timestamp,
        );
        messages.push((role.to_string(), text, created_at, role == "user"));
        if messages.len() > MAX_MESSAGES {
            messages.remove(0);
        }
    }

    finish_session(
        ProviderId::Codex,
        provider_session_id,
        None,
        model,
        cwd,
        messages,
        &fallback_timestamp,
        path.to_path_buf(),
        last_active_ms,
    )
}

fn list_transcript_files(root: &Path, filter: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") && filter(&path) {
                files.push(path);
            }
        }
    }
    files
}

fn mtime_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn scan_sessions(paths: Vec<PathBuf>) -> Vec<DiscoveredSession> {
    let mut sessions = Vec::new();
    for path in paths {
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        let session = if path.to_string_lossy().contains("rollout-") {
            parse_codex_transcript(&contents, &path, mtime_ms(&path))
        } else {
            let fallback = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("").to_string();
            parse_claude_transcript(&contents, &path, mtime_ms(&path), &fallback)
        };
        if let Some(session) = session {
            sessions.push(session);
        }
    }
    let mut seen = HashSet::new();
    sessions
        .into_iter()
        .filter(|session| {
            let key = format!("{}:{}", session.provider, session.native_session_id);
            if seen.contains(&key) {
                false
            } else {
                seen.insert(key);
                true
            }
        })
        .collect()
}

pub fn scan_claude_sessions(projects_dir: &Path, limit: usize) -> Vec<DiscoveredSession> {
    let mut files = list_transcript_files(projects_dir, |_| true);
    files.sort_by_key(|path| std::cmp::Reverse(mtime_ms(path)));
    files.truncate(MAX_SCAN_FILES);
    scan_sessions(files).into_iter().take(limit).collect()
}

pub fn scan_codex_sessions(sessions_dir: &Path, limit: usize) -> Vec<DiscoveredSession> {
    let mut files = list_transcript_files(sessions_dir, |path| {
        path.to_string_lossy().contains("rollout-")
    });
    files.sort_by_key(|path| std::cmp::Reverse(mtime_ms(path)));
    files.truncate(MAX_SCAN_FILES);
    scan_sessions(files).into_iter().take(limit).collect()
}
