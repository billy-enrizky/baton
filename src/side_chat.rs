//! The merge-back payload for a side chat. A side chat shares its source's
//! working tree, so only the conversation delta needs to move.

use crate::store::MessageRow;

const EXCERPT_CHARS: usize = 240;
const MAX_MESSAGES: usize = 12;
const MAX_TOTAL_CHARS: usize = 4_000;

pub fn merge_back_summary(
    source_id: &str,
    side_id: &str,
    side_title: &str,
    messages: &[MessageRow],
) -> String {
    let mut lines = vec![
        format!("Merge-back context from side chat \"{side_title}\"."),
        format!("Source thread: {source_id}"),
        format!("Side chat: {side_id}"),
        format!("Fork delta: {} new message(s)", messages.len()),
        "Working tree: shared with the source, so no file restore is included.".to_string(),
        String::new(),
        "Delta:".to_string(),
    ];
    for message in messages.iter().take(MAX_MESSAGES) {
        let speaker = match message.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            _ => message.role.as_str(),
        };
        lines.push(format!("- {speaker}: {}", excerpt(&message.text)));
    }
    if messages.len() > MAX_MESSAGES {
        lines.push(format!(
            "- ... {} earlier message(s) omitted",
            messages.len() - MAX_MESSAGES
        ));
    }
    let mut summary = lines.join("\n");
    if summary.chars().count() > MAX_TOTAL_CHARS {
        summary = summary.chars().take(MAX_TOTAL_CHARS - 3).collect();
        summary.push_str("...");
    }
    summary
}

fn excerpt(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= EXCERPT_CHARS {
        return compact;
    }
    let mut short: String = compact.chars().take(EXCERPT_CHARS - 3).collect();
    short.push_str("...");
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, text: &str) -> MessageRow {
        MessageRow {
            id: 1,
            thread_id: "side".to_string(),
            turn: 1,
            role: role.to_string(),
            provider: None,
            model: None,
            text: text.to_string(),
            created_at: "2026-11-25T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn summary_names_both_threads_and_the_delta() {
        let summary = merge_back_summary(
            "source-id",
            "side-id",
            "source (side chat)",
            &[
                message("user", "why does this work?"),
                message("assistant", "because the cache stays warm"),
            ],
        );
        assert!(summary.contains("Merge-back context from side chat \"source (side chat)\"."));
        assert!(summary.contains("Source thread: source-id"));
        assert!(summary.contains("Side chat: side-id"));
        assert!(summary.contains("Fork delta: 2 new message(s)"));
        assert!(summary.contains("- User: why does this work?"));
        assert!(summary.contains("- Assistant: because the cache stays warm"));
    }

    #[test]
    fn excerpts_compact_whitespace_and_truncate_long_text() {
        let long = "word ".repeat(100);
        let summary =
            merge_back_summary("s", "d", "t", &[message("user", &format!("a\n\nb {long}"))]);
        assert!(summary.contains("- User: a b word word"));
        assert!(summary.ends_with("..."));
        assert!(!summary.contains("\n\nword"));
    }

    #[test]
    fn a_long_delta_is_capped() {
        let messages: Vec<MessageRow> = (0..20)
            .map(|index| message("user", &format!("message {index}")))
            .collect();
        let summary = merge_back_summary("s", "d", "t", &messages);
        assert!(summary.contains("- ... 8 earlier message(s) omitted"));
        assert!(!summary.contains("message 12"));
    }
}
