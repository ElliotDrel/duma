//! Request builders. Static instructions go first (and stay byte-identical) so prompt caching applies;
//! variable content goes in the input, delimited as data.

use crate::settings::{Preset, Settings};
use crate::store::Version;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PromptContext {
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    /// Bounded text before the cursor or selection.
    pub before_cursor: Option<String>,
    pub vocabulary: Vec<String>,
}

fn context_block(context: &PromptContext) -> String {
    let mut lines = Vec::new();
    if let Some(app) = &context.app_name {
        lines.push(format!("app: {app}"));
    }
    if let Some(window) = context.window_title.as_deref().filter(|w| !w.is_empty()) {
        lines.push(format!("window: {window}"));
    }
    if !context.vocabulary.is_empty() {
        lines.push(format!(
            "preferred spellings: {}",
            context.vocabulary.join(", ")
        ));
    }
    if let Some(before) = context
        .before_cursor
        .as_deref()
        .filter(|b| !b.trim().is_empty())
    {
        lines.push(format!("text before the cursor:\n{before}"));
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("<context>\n{}\n</context>\n", lines.join("\n"))
    }
}

pub fn dictation(
    settings: &Settings,
    transcript: &str,
    context: &PromptContext,
) -> (String, String) {
    let input = format!(
        "{}<transcript>\n{}\n</transcript>",
        context_block(context),
        transcript.trim()
    );
    (settings.dictation_prompt.clone(), input)
}

/// `lineage` is the thread so far (oldest first, ending at the version being edited), if any.
pub fn edit(
    settings: &Settings,
    lineage: &[Version],
    current: Option<&str>,
    instruction: &str,
    context: &PromptContext,
) -> (String, String) {
    let mut input = context_block(context);
    if lineage.len() > 1 {
        input.push_str("<thread>\n");
        for (n, version) in lineage.iter().enumerate() {
            if let (Some(source), true) = (
                &version.source,
                version.kind == "instruction" || version.kind == "preset",
            ) {
                input.push_str(&format!(
                    "<{} n=\"{n}\">{source}</{}>\n",
                    version.kind, version.kind
                ));
            }
            input.push_str(&format!(
                "<version n=\"{n}\" kind=\"{}\">\n{}\n</version>\n",
                version.kind, version.text
            ));
        }
        input.push_str("</thread>\n");
    }
    match current {
        Some(text) => input.push_str(&format!("<current_text>\n{text}\n</current_text>\n")),
        None => input.push_str("<current_text></current_text>\n"),
    }
    input.push_str(&format!(
        "<instruction>\n{}\n</instruction>",
        instruction.trim()
    ));
    (settings.edit_prompt.clone(), input)
}

pub fn preset(preset: &Preset, text: &str) -> (String, String) {
    (
        format!(
            "{}\nReturn only the resulting text, with no quotes or commentary.",
            preset.prompt.trim()
        ),
        format!("<text>\n{text}\n</text>"),
    )
}

/// Stable 64-bit FNV-1a hash of a prompt, logged with every model call to group results by prompt version.
pub fn hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(kind: &str, text: &str, source: Option<&str>) -> Version {
        Version {
            id: String::new(),
            thread_id: String::new(),
            parent_id: None,
            created_at: String::new(),
            kind: kind.into(),
            text: text.into(),
            source: source.map(str::to_string),
            run_id: None,
            model: None,
        }
    }

    #[test]
    fn edit_input_carries_thread_history() {
        let settings = Settings::default();
        let lineage = vec![
            version("dictation", "First draft.", Some("first draft")),
            version("instruction", "Shorter.", Some("make it shorter")),
        ];
        let (instructions, input) = edit(
            &settings,
            &lineage,
            Some("Shorter."),
            "more formal",
            &PromptContext::default(),
        );
        assert_eq!(instructions, settings.edit_prompt);
        assert!(input.contains("<instruction n=\"1\">make it shorter</instruction>"));
        assert!(input.contains("<current_text>\nShorter.\n</current_text>"));
        assert!(input.ends_with("<instruction>\nmore formal\n</instruction>"));
    }

    #[test]
    fn dictation_wraps_transcript_as_data() {
        let context = PromptContext {
            app_name: Some("Slack".into()),
            ..Default::default()
        };
        let (_, input) = dictation(&Settings::default(), " hello ", &context);
        assert_eq!(
            input,
            "<context>\napp: Slack\n</context>\n<transcript>\nhello\n</transcript>"
        );
    }

    #[test]
    fn hash_is_stable() {
        assert_eq!(hash("abc"), "e71fa2190541574b");
    }
}
