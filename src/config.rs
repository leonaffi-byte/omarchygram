//! `~/.config/omarchygram/config.toml` — the SECRETS file (0600).
//! Orchestrator-owned. Holds the Telegram API credentials and the optional
//! `[ai]` key table. Every write is atomic (temp + fsync + rename) and keeps
//! the file private; values are never logged.
//!
//! The UI writes through this module only (settings pages, login form).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

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
    std::fs::read_to_string(paths::config_file())
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

fn write_table(table: &toml::Table) -> std::io::Result<()> {
    let p = paths::config_file();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let text = toml::to_string_pretty(table).map_err(std::io::Error::other)?;
    let tmp = p.with_extension("toml.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &p)?;
    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// Telegram API credentials, if configured and well-formed.
pub fn credentials() -> Option<(i32, String)> {
    let t = read_table();
    let id = t.get("api_id")?.as_integer()? as i32;
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
    let mut t = read_table();
    t.insert("api_id".into(), toml::Value::Integer(api_id as i64));
    t.insert("api_hash".into(), toml::Value::String(api_hash));
    write_table(&t).map_err(|e| format!("could not save config.toml: {e}"))
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
    if let Some(v) = value {
        if v.chars().any(|c| c.is_control()) {
            return Err("the key contains control characters".into());
        }
    }
    let mut t = read_table();
    let ai = t
        .entry("ai")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(ai) = ai.as_table_mut() else {
        return Err("config.toml: [ai] is not a table".into());
    };
    match value {
        Some(v) => {
            ai.insert(key.into(), toml::Value::String(v.to_string()));
        }
        None => {
            ai.remove(key);
        }
    }
    write_table(&t).map_err(|e| format!("could not save config.toml: {e}"))
}

/// Ensure the file is private if it exists (called at startup).
pub fn enforce_permissions() {
    let p = paths::config_file();
    if p.exists() {
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
}

#[allow(dead_code)]
fn _is_path(_p: &Path) {}

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
