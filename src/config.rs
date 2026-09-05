//! `~/.config/omarchygram/config.toml` — the SECRETS file (0600).
//! Orchestrator-owned. Holds the Telegram API credentials and the optional
//! `[ai]` key table. Every write is atomic (temp + fsync + rename) and keeps
//! the file private; values are never logged.
//!
//! The UI writes through this module only (settings pages, login form).

use std::os::unix::fs::PermissionsExt;

use crate::tg::paths;

pub const AI_KEY_NAMES: [&str; 4] = ["anthropic", "openai", "groq", "gemini"];

/// Which AI keys are set (never the values).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiKeyStatus {
    pub anthropic: bool,
    pub openai: bool,
    pub groq: bool,
    pub gemini: bool,
    pub whisper_model: Option<String>,
}

fn read_table() -> toml::Table {
    crate::storage::read_table(&paths::config_file()).unwrap_or_default()
}

/// Telegram API credentials, if configured and well-formed.
pub fn credentials() -> Option<(i32, String)> {
    let t = read_table();
    let id = i32::try_from(t.get("api_id")?.as_integer()?).ok()?;
    let hash = t.get("api_hash")?.as_str()?.to_string();
    if id <= 0 || !valid_api_hash(&hash) {
        return None;
    }
    Some((id, hash))
}

pub fn has_credentials() -> bool {
    credentials().is_some()
}

/// 32 lowercase hex chars — what my.telegram.org issues.
pub fn valid_api_hash(hash: &str) -> bool {
    hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn set_credentials(api_id: i32, api_hash: &str) -> Result<(), String> {
    let api_hash = api_hash.trim().to_ascii_lowercase();
    if api_id <= 0 {
        return Err("API ID must be a positive number".into());
    }
    if !valid_api_hash(&api_hash) {
        return Err("API hash must be 32 hexadecimal characters".into());
    }
    crate::storage::update_table(&paths::config_file(), |t| {
        t.insert("api_id".into(), toml::Value::Integer(api_id as i64));
        t.insert("api_hash".into(), toml::Value::String(api_hash));
        Ok(())
    }).map_err(|e| format!("could not save config.toml: {e}"))
}

pub fn ai_keys() -> AiKeyStatus {
    let t = read_table();
    let ai = t.get("ai").and_then(|v| v.as_table());
    let set = |n: &str| {
        ai.and_then(|a| a.get(n)).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
    };
    AiKeyStatus {
        anthropic: set("anthropic_api_key"),
        openai: set("openai_api_key"),
        groq: set("groq_api_key"),
        gemini: set("gemini_api_key"),
        whisper_model: ai
            .and_then(|a| a.get("whisper_model"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

/// `provider` is one of `AI_KEY_NAMES` or "whisper_model". `None`/empty removes.
pub fn set_ai_key(provider: &str, value: Option<&str>) -> Result<(), String> {
    let key = match provider {
        "anthropic" => "anthropic_api_key",
        "openai" => "openai_api_key",
        "groq" => "groq_api_key",
        "gemini" => "gemini_api_key",
        "whisper_model" => "whisper_model",
        other => return Err(format!("unknown provider {other}")),
    };
    let value = value.map(str::trim).filter(|v| !v.is_empty());
    if let Some(v) = value
        && v.chars().any(|c| c.is_control()) {
            return Err("the key contains control characters".into());
        }
    crate::storage::update_table(&paths::config_file(), |t| {
    let ai = t
        .entry("ai")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(ai) = ai.as_table_mut() else {
        return Err(std::io::Error::other("config.toml: [ai] is not a table"));
    };
    match value {
        Some(v) => {
            ai.insert(key.into(), toml::Value::String(v.to_string()));
        }
        None => {
            ai.remove(key);
        }
    }
    Ok(())
    }).map_err(|e| format!("could not save config.toml: {e}"))
}

/// Ensure the file is private if it exists (called at startup).
pub fn enforce_permissions() {
    let p = paths::config_file();
    if p.exists() {
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_hash_validation() {
        assert!(valid_api_hash("0123456789abcdef0123456789abcdef"));
        assert!(!valid_api_hash("0123456789abcdef0123456789abcde"));
        assert!(!valid_api_hash("0123456789abcdef0123456789abcdeg"));
    }
}
