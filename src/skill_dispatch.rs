use crate::types::ProviderId;

#[derive(Debug)]
struct Mention {
    start: usize,
    end: usize,
    name: String,
}

fn known_mentions(prompt: &str, known: &[String]) -> Vec<Mention> {
    let characters = prompt.char_indices().collect::<Vec<_>>();
    let mut mentions = Vec::new();
    for (index, (_, character)) in characters.iter().enumerate() {
        if *character != '$' {
            continue;
        }
        if index > 0 && !characters[index - 1].1.is_whitespace() {
            continue;
        }
        let name_start = index + 1;
        let Some((_, first)) = characters.get(name_start) else {
            continue;
        };
        if !first.is_ascii_alphanumeric() {
            continue;
        }
        let mut name_end = name_start + 1;
        while let Some((_, character)) = characters.get(name_end) {
            if character.is_ascii_alphanumeric() || matches!(character, ':' | '_' | '-') {
                name_end += 1;
            } else {
                break;
            }
        }
        if let Some((_, next)) = characters.get(name_end) {
            if !next.is_whitespace() {
                continue;
            }
        }
        let name_end_byte = characters
            .get(name_end)
            .map(|(byte, _)| *byte)
            .unwrap_or(prompt.len());
        let name = &prompt[characters[name_start].0..name_end_byte];
        if !name
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        {
            continue;
        }
        if known.iter().any(|known| known == name) {
            mentions.push(Mention {
                start: characters[index].0,
                end: name_end_byte,
                name: name.to_string(),
            });
        }
    }
    mentions
}

pub fn rewrite_claude_prompt(prompt: &str, known: &[String]) -> String {
    rewrite_claude_prompt_from(prompt, known, 0)
}

pub fn rewrite_claude_prompt_from(prompt: &str, known: &[String], mentions_from: usize) -> String {
    let Some(last) = known_mentions(prompt, known)
        .into_iter()
        .rev()
        .find(|mention| mention.start >= mentions_from)
    else {
        return prompt.to_string();
    };
    let before = prompt[..last.start].trim_end();
    let after = prompt[last.end..].trim_start();
    let arguments = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (true, false) => after.to_string(),
        (false, true) => before.to_string(),
        (false, false) if prompt[..last.start].ends_with('\n') => {
            format!("{before}\n{after}")
        }
        (false, false) => format!("{before} {after}"),
    };
    if arguments.is_empty() {
        format!("/{}", last.name)
    } else {
        format!("/{} {arguments}", last.name)
    }
}

pub fn prompt_for_provider_from(
    provider: ProviderId,
    prompt: &str,
    current_message: Option<&str>,
) -> String {
    if provider != ProviderId::Claude {
        return prompt.to_string();
    }
    let known = crate::sync::scan_skills()
        .into_iter()
        .filter(|skill| skill.source == "claude")
        .map(|skill| skill.name)
        .collect::<Vec<_>>();
    let mentions_from = current_message
        .and_then(|text| prompt.rfind(text))
        .unwrap_or(0);
    rewrite_claude_prompt_from(prompt, &known, mentions_from)
}

pub fn prompt_for_provider(provider: ProviderId, prompt: &str) -> String {
    prompt_for_provider_from(provider, prompt, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_rewrites_a_known_mention_to_a_leading_command() {
        let known = vec!["review".to_string()];
        assert_eq!(
            rewrite_claude_prompt("please $review this change", &known),
            "/review please this change"
        );
        assert_eq!(
            rewrite_claude_prompt("[goal: ship]\n$review this", &known),
            "/review [goal: ship]\nthis"
        );
    }

    #[test]
    fn unknown_mid_word_punctuation_and_digit_only_mentions_are_ignored() {
        let known = vec!["review".to_string(), "123".to_string()];
        assert_eq!(
            rewrite_claude_prompt("please $unknown this", &known),
            "please $unknown this"
        );
        assert_eq!(
            rewrite_claude_prompt("foo$review this", &known),
            "foo$review this"
        );
        assert_eq!(
            rewrite_claude_prompt("please $review, this", &known),
            "please $review, this"
        );
        assert_eq!(
            rewrite_claude_prompt("please $123 this", &known),
            "please $123 this"
        );
    }

    #[test]
    fn the_last_known_mention_wins_and_earlier_mentions_stay_in_arguments() {
        let known = vec!["alpha".to_string(), "beta".to_string()];
        assert_eq!(
            rewrite_claude_prompt("first $alpha then $beta", &known),
            "/beta first $alpha then"
        );
    }

    #[test]
    fn handoff_history_mentions_before_the_current_message_are_ignored() {
        let known = vec!["old".to_string(), "new".to_string()];
        let prompt = "history $old\ncurrent $new";
        let current_start = prompt.find("current $new").unwrap();
        assert_eq!(
            rewrite_claude_prompt_from(prompt, &known, current_start),
            "/new history $old\ncurrent"
        );
    }

    #[test]
    fn other_providers_keep_the_prompt_unchanged() {
        assert_eq!(
            prompt_for_provider(ProviderId::Codex, "please $review this"),
            "please $review this"
        );
    }
}
