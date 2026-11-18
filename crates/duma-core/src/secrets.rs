//! The OpenAI API key lives in the OS credential store (Windows Credential Manager, macOS Keychain),
//! never in settings or logs. `OPENAI_API_KEY` overrides it for development.

use anyhow::Result;

const SERVICE: &str = "Duma";
const ACCOUNT: &str = "openai-api-key";

fn entry() -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, ACCOUNT)?)
}

pub fn api_key() -> Option<String> {
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            return Some(key.trim().to_string());
        }
    }
    entry()
        .ok()?
        .get_password()
        .ok()
        .filter(|k| !k.trim().is_empty())
}

pub fn set_api_key(key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        let _ = entry()?.delete_credential();
        return Ok(());
    }
    Ok(entry()?.set_password(key)?)
}

/// For display: `sk-…abcd`.
pub fn masked(key: &str) -> String {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let head: String = key.chars().take(3).collect();
    format!("{head}…{tail}")
}
