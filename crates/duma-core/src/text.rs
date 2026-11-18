//! Pure text helpers: model-output guards, spacing against the text before the cursor, bounded context,
//! UTF-16 conversions and span tracking for the edit watch.

/// Strips wrappers models sometimes add: a "Here is…:" preamble line and quotes the input did not have.
pub fn strip_wrapping(output: &str, input: &str) -> String {
    let mut text = output.trim().to_string();
    if let Some((first, rest)) = text.split_once('\n') {
        let lower = first.trim().to_lowercase();
        let preamble = [
            "here is",
            "here's",
            "sure",
            "certainly",
            "okay",
            "ok,",
            "cleaned",
            "corrected",
            "revised",
        ]
        .iter()
        .any(|p| lower.starts_with(p));
        if preamble && first.trim_end().ends_with(':') {
            text = rest.trim().to_string();
        }
    }
    for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\'')] {
        let input = input.trim();
        if text.len() > 2
            && text.starts_with(open)
            && text.ends_with(close)
            && !(input.starts_with(open) && input.ends_with(close))
        {
            text = text[open.len_utf8()..text.len() - close.len_utf8()]
                .trim()
                .to_string();
        }
    }
    text
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Rejects cleanup output that added or dropped too much. A rejected output is replaced by the transcript.
pub fn dictation_guard(transcript: &str, output: &str) -> Result<(), String> {
    let source = words(transcript);
    let result = words(output);
    if result.is_empty() && !source.is_empty() {
        return Err("output is empty".into());
    }
    let (s, r) = (source.len() as f64, result.len() as f64);
    if r > s * 1.5 + 8.0 {
        return Err(format!(
            "output has {r} words for {s} spoken: likely added content"
        ));
    }
    if s >= 8.0 && r < s * 0.35 {
        return Err(format!(
            "output has {r} words for {s} spoken: likely dropped content"
        ));
    }
    if result.len() >= 5 {
        let spoken: std::collections::HashSet<&str> = source.iter().map(String::as_str).collect();
        let kept = result
            .iter()
            .filter(|w| spoken.contains(w.as_str()) || w.chars().any(|c| c.is_ascii_digit()))
            .count();
        let ratio = kept as f64 / r;
        if ratio < 0.5 {
            return Err(format!(
                "only {:.0}% of output words were spoken: likely rewritten or answered",
                ratio * 100.0
            ));
        }
    }
    Ok(())
}

/// Minimal rule cleanup for very short dictations: trim and capitalize.
pub fn rules_cleanup(transcript: &str) -> String {
    let text = transcript.trim();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Adds a leading space when the cursor sits right after a word.
pub fn join_with_before(before_cursor: Option<&str>, output: &str) -> String {
    let needs_space = before_cursor
        .and_then(|b| b.chars().last())
        .is_some_and(|c| !c.is_whitespace() && !"([{\"'“‘/-".contains(c))
        && output
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '(' || c == '"');
    if needs_space {
        format!(" {output}")
    } else {
        output.to_string()
    }
}

/// Byte offset of a UTF-16 offset, clamped.
pub fn utf16_to_byte(s: &str, offset: usize) -> usize {
    duma_platform::utf16_to_byte(s, offset)
}

pub fn utf16_len(s: &str) -> usize {
    duma_platform::utf16_len(s)
}

/// Text before the selection, the selection and the text after, each side bounded to `chars` characters.
pub fn context_around(
    text: &str,
    selection: Option<(usize, usize)>,
    chars: usize,
) -> (String, String, String) {
    let (start, len) = selection.unwrap_or((utf16_len(text), 0));
    let a = utf16_to_byte(text, start);
    let b = utf16_to_byte(text, start + len).max(a);
    let before: String = {
        let chars_before: Vec<char> = text[..a].chars().collect();
        chars_before[chars_before.len().saturating_sub(chars)..]
            .iter()
            .collect()
    };
    let after: String = text[b..].chars().take(chars).collect();
    (before, text[a..b].to_string(), after)
}

/// Longest common prefix in bytes, on a char boundary.
pub fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map(|((i, _), _)| i)
        .unwrap_or_else(|| a.len().min(b.len()))
}

/// Longest common suffix in bytes, on a char boundary.
pub fn common_suffix(a: &str, b: &str) -> usize {
    let mut n = 0;
    for (x, y) in a.chars().rev().zip(b.chars().rev()) {
        if x != y {
            break;
        }
        n += x.len_utf8();
    }
    n
}

/// Where Duma's span sits in `now`, given the field `before` an edit and that the span occupied
/// `prefix` bytes from the start and `suffix` bytes from the end there. Returns (start, end, outside_changed).
pub fn track_span(before: &str, prefix: usize, suffix: usize, now: &str) -> (usize, usize, bool) {
    let p = common_prefix(before, now).min(prefix);
    let s = common_suffix(before, now).min(suffix).min(now.len() - p);
    let outside_changed = p < prefix || s < suffix;
    (p, now.len() - s, outside_changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_preamble_and_added_quotes() {
        assert_eq!(
            strip_wrapping("Here is the cleaned text:\nHello there.", "hello there"),
            "Hello there."
        );
        assert_eq!(
            strip_wrapping("\"Hello there.\"", "hello there"),
            "Hello there."
        );
        assert_eq!(strip_wrapping("\"Hi\"", "\"hi\""), "\"Hi\"");
        assert_eq!(
            strip_wrapping("Sure thing, see you then.", "sure thing see you then"),
            "Sure thing, see you then."
        );
    }

    #[test]
    fn guard_catches_answers_and_drops() {
        assert!(
            dictation_guard(
                "um so can we uh move the button left",
                "Can we move the button left?"
            )
            .is_ok()
        );
        assert!(
            dictation_guard(
                "what is the capital of france",
                "The capital of France is Paris, a city known for the Eiffel Tower and its museums and food."
            )
            .is_err()
        );
        assert!(
            dictation_guard("one two three four five six seven eight nine ten", "One.").is_err()
        );
        assert!(dictation_guard("meet at three thirty", "Meet at 3:30.").is_ok());
    }

    #[test]
    fn spacing_after_words_only() {
        assert_eq!(join_with_before(Some("Hello"), "world"), " world");
        assert_eq!(join_with_before(Some("Hello "), "world"), "world");
        assert_eq!(join_with_before(Some("("), "world"), "world");
        assert_eq!(join_with_before(None, "world"), "world");
        assert_eq!(join_with_before(Some("Hi"), ", there"), ", there");
    }

    #[test]
    fn context_is_bounded() {
        let (before, selected, after) = context_around("abcdefghij", Some((4, 2)), 3);
        assert_eq!(
            (before.as_str(), selected.as_str(), after.as_str()),
            ("bcd", "ef", "ghi")
        );
        let (before, selected, _) = context_around("abc", None, 10);
        assert_eq!((before.as_str(), selected.as_str()), ("abc", ""));
    }

    #[test]
    fn span_tracking() {
        let before = "Hi Bob. OUTPUT here. Bye";
        let (prefix, suffix) = (8, " Bye".len());
        let now = "Hi Bob. OUTPUT edited here. Bye";
        let (start, end, outside) = track_span(before, prefix, suffix, now);
        assert_eq!(&now[start..end], "OUTPUT edited here.");
        assert!(!outside);
        let now = "Hey Bob. OUTPUT here. Bye";
        let (start, end, outside) = track_span(before, prefix, suffix, now);
        assert!(outside);
        assert!(now[start..end].contains("OUTPUT here."));
    }
}
