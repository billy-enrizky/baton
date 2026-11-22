use std::borrow::Cow;

pub const PLACEHOLDER: &str = "[redacted]";
const MIN_SECRET_LEN: usize = 8;

/// Secrets shorter than this are too likely to appear in ordinary prose to
/// replace safely.
pub fn collect<'a>(candidates: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut secrets: Vec<String> = candidates
        .into_iter()
        .map(str::trim)
        .filter(|value| value.len() >= MIN_SECRET_LEN)
        .map(str::to_string)
        .collect();
    secrets.sort_by(|a, b| b.len().cmp(&a.len()));
    secrets.dedup();
    secrets
}

/// Replaces every occurrence of a configured secret. Longest first, so a key
/// that contains another key is redacted whole.
pub fn redact<'a>(text: &'a str, secrets: &[String]) -> Cow<'a, str> {
    let mut current: Cow<'a, str> = Cow::Borrowed(text);
    for secret in secrets {
        if !current.contains(secret.as_str()) {
            continue;
        }
        current = Cow::Owned(current.replace(secret.as_str(), PLACEHOLDER));
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_a_configured_secret() {
        let secrets = collect(["sk-live-abcdef123456"]);
        let text = "error: sk-live-abcdef123456 rejected";
        assert_eq!(redact(text, &secrets), "error: [redacted] rejected");
    }

    #[test]
    fn clean_text_is_borrowed() {
        let secrets = collect(["sk-live-abcdef123456"]);
        assert!(matches!(
            redact("nothing to hide", &secrets),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn short_values_are_not_secrets() {
        let secrets = collect(["abc", ""]);
        assert!(secrets.is_empty());
        assert_eq!(redact("abc is a word", &secrets), "abc is a word");
    }

    #[test]
    fn the_longest_secret_wins_when_one_contains_another() {
        let secrets = collect(["sk-abcdef123456", "sk-abcdef123456-extra"]);
        assert_eq!(
            redact("key sk-abcdef123456-extra here", &secrets),
            "key [redacted] here"
        );
    }

    #[test]
    fn every_occurrence_is_replaced() {
        let secrets = collect(["sk-live-abcdef123456"]);
        assert_eq!(
            redact("a sk-live-abcdef123456 b sk-live-abcdef123456", &secrets),
            "a [redacted] b [redacted]"
        );
    }
}
