//! AI provider layer. Orchestrator-owned: API keys and network.
//!
//! One abstraction, many backends, auto-detected from what is installed and
//! which keys exist. Nothing here runs unless the UI asks (ai.enabled). Keys
//! live in `~/.config/omarchygram/config.toml` `[ai]` (chmod 600) or env vars;
//! they never appear in logs or error text.
//!
//! Chat backends:  ollama (local) · anthropic · openai · groq · gemini
//! Transcription:  whisper (local whisper.cpp) · groq · openai

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

pub mod prompts;

const HTTP_TIMEOUT: Duration = Duration::from_secs(180);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Preferences the UI passes per call (mirrors `settings::AiSettings`).
#[derive(Clone, Debug, Default)]
pub struct Prefs {
    /// "" = auto. Ids: ollama, anthropic, openai, groq, gemini.
    pub chat_provider: String,
    /// "" = auto. Ids: whisper, groq, openai.
    pub transcribe_provider: String,
    /// "" = provider default.
    pub chat_model: String,
    pub ollama_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct ChatReply {
    pub text: String,
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug)]
pub struct Transcript {
    pub text: String,
    pub provider: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Task {
    Chat,
    Transcribe,
}

impl Task {
    pub fn label(&self) -> &'static str {
        match self {
            Task::Chat => "chat",
            Task::Transcribe => "transcribe",
        }
    }
}

/// `OMG_MOCK_AI=1` (set by --smoke) answers offline with canned replies so
/// the UI's AI paths can be traversed without keys or network.
fn mock_ai() -> bool {
    std::env::var("OMG_MOCK_AI").is_ok_and(|v| !v.is_empty())
}

#[derive(Clone, Debug)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub task: Task,
    pub available: bool,
    /// Why available/unavailable, e.g. "key found", "not installed".
    pub detail: String,
}

/// Secrets, read fresh on each call so edits to config.toml apply live.
#[derive(Default, Clone)]
struct Keys {
    anthropic: Option<String>,
    openai: Option<String>,
    groq: Option<String>,
    gemini: Option<String>,
    whisper_model: Option<String>,
}

fn load_keys() -> Keys {
    let mut k = Keys::default();
    let path = crate::tg::paths::config_file();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(t) = text.parse::<toml::Table>() {
            if let Some(ai) = t.get("ai").and_then(|v| v.as_table()) {
                let g = |n: &str| ai.get(n).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
                k.anthropic = g("anthropic_api_key");
                k.openai = g("openai_api_key");
                k.groq = g("groq_api_key");
                k.gemini = g("gemini_api_key");
                k.whisper_model = g("whisper_model");
            }
        }
    }
    let env = |n: &str| std::env::var(n).ok().filter(|s| !s.is_empty());
    k.anthropic = k.anthropic.or_else(|| env("ANTHROPIC_API_KEY"));
    k.openai = k.openai.or_else(|| env("OPENAI_API_KEY"));
    k.groq = k.groq.or_else(|| env("GROQ_API_KEY"));
    k.gemini = k.gemini.or_else(|| env("GEMINI_API_KEY").or_else(|| env("GOOGLE_API_KEY")));
    k
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

fn ollama_url(prefs: &Prefs) -> String {
    let u = if prefs.ollama_url.is_empty() { "http://127.0.0.1:11434" } else { prefs.ollama_url.as_str() };
    u.trim_end_matches('/').to_string()
}

/// Models Ollama serves, or None when it is not reachable.
async fn ollama_models(prefs: &Prefs) -> Option<Vec<String>> {
    let c = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build().ok()?;
    let v: Value = c.get(format!("{}/api/tags", ollama_url(prefs))).send().await.ok()?.json().await.ok()?;
    Some(
        v.get("models")?
            .as_array()?
            .iter()
            .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect(),
    )
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

fn whisper_binary() -> Option<PathBuf> {
    ["whisper-cli", "whisper-cpp", "whisper"].iter().find_map(|b| which(b))
}

fn whisper_model(keys: &Keys) -> Option<PathBuf> {
    if let Some(m) = &keys.whisper_model {
        let p = PathBuf::from(shellexpand(m));
        return p.is_file().then_some(p);
    }
    let home = dirs::home_dir()?;
    let dirs_ = [
        home.join(".local/share/whisper.cpp"),
        home.join(".local/share/whisper"),
        home.join(".cache/whisper.cpp"),
        PathBuf::from("/usr/share/whisper.cpp/models"),
        PathBuf::from("/usr/share/whisper.cpp"),
    ];
    let mut found: Vec<PathBuf> = dirs_
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bin") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ggml")))
        .collect();
    found.sort();
    found.into_iter().next()
}

fn shellexpand(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest).to_string_lossy().to_string();
        }
    }
    s.to_string()
}

/// What can run right now, with reasons — for the settings page and the
/// Assistant's "status" answer.
pub async fn detect(prefs: &Prefs) -> Vec<ProviderInfo> {
    if mock_ai() {
        return vec![
            ProviderInfo { id: "mock", task: Task::Chat, available: true, detail: "offline stand-in (OMG_MOCK_AI)".into() },
            ProviderInfo { id: "ollama", task: Task::Chat, available: false, detail: "not probed in mock mode".into() },
            ProviderInfo { id: "mock", task: Task::Transcribe, available: true, detail: "offline stand-in (OMG_MOCK_AI)".into() },
        ];
    }
    let keys = load_keys();
    let mut out = Vec::new();
    let key_info = |id: &'static str, task: Task, key: &Option<String>| ProviderInfo {
        id,
        task,
        available: key.is_some(),
        detail: if key.is_some() { "API key found".into() } else { "no API key (config.toml [ai] or env)".into() },
    };
    match ollama_models(prefs).await {
        Some(models) if !models.is_empty() => out.push(ProviderInfo {
            id: "ollama",
            task: Task::Chat,
            available: true,
            detail: format!("{} model(s): {}", models.len(), models.join(", ")),
        }),
        Some(_) => out.push(ProviderInfo { id: "ollama", task: Task::Chat, available: false, detail: "running but no models pulled".into() }),
        None => out.push(ProviderInfo { id: "ollama", task: Task::Chat, available: false, detail: format!("not reachable at {}", ollama_url(prefs)) }),
    }
    out.push(key_info("anthropic", Task::Chat, &keys.anthropic));
    out.push(key_info("openai", Task::Chat, &keys.openai));
    out.push(key_info("groq", Task::Chat, &keys.groq));
    out.push(key_info("gemini", Task::Chat, &keys.gemini));
    let wb = whisper_binary();
    let wm = whisper_model(&keys);
    out.push(ProviderInfo {
        id: "whisper",
        task: Task::Transcribe,
        available: wb.is_some() && wm.is_some() && which("ffmpeg").is_some(),
        detail: match (&wb, &wm) {
            (Some(b), Some(m)) => format!("{} + {}", b.display(), m.display()),
            (None, _) => "whisper.cpp not installed (whisper-cli)".into(),
            (_, None) => "no ggml model found (set [ai] whisper_model in config.toml)".into(),
        },
    });
    out.push(key_info("groq", Task::Transcribe, &keys.groq));
    out.push(key_info("openai", Task::Transcribe, &keys.openai));
    out
}

async fn pick_chat(prefs: &Prefs, keys: &Keys) -> Result<(&'static str, String), String> {
    let pinned = prefs.chat_provider.trim().to_lowercase();
    let model = |default: &str| if prefs.chat_model.is_empty() { default.to_string() } else { prefs.chat_model.clone() };
    let candidates: Vec<&str> = if pinned.is_empty() {
        vec!["ollama", "anthropic", "openai", "groq", "gemini"]
    } else {
        vec![pinned.as_str()]
    };
    for id in candidates {
        match id {
            "ollama" => {
                if let Some(models) = ollama_models(prefs).await {
                    if let Some(first) = models.first() {
                        let m = if prefs.chat_model.is_empty() { first.clone() } else { prefs.chat_model.clone() };
                        return Ok(("ollama", m));
                    }
                }
            }
            "anthropic" if keys.anthropic.is_some() => return Ok(("anthropic", model("claude-opus-5"))),
            "openai" if keys.openai.is_some() => return Ok(("openai", model("gpt-4o-mini"))),
            "groq" if keys.groq.is_some() => return Ok(("groq", model("llama-3.3-70b-versatile"))),
            "gemini" if keys.gemini.is_some() => return Ok(("gemini", model("gemini-2.5-flash"))),
            _ => {}
        }
    }
    Err(if pinned.is_empty() {
        "no chat provider available — add an API key under [ai] in config.toml or run ollama".into()
    } else {
        format!("chat provider '{pinned}' is not available (missing key or not running)")
    })
}

/// One non-streaming chat turn. `system` may be empty.
pub async fn chat(prefs: &Prefs, system: &str, messages: &[ChatMessage]) -> Result<ChatReply, String> {
    if messages.is_empty() {
        return Err("nothing to send".into());
    }
    if mock_ai() {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let last = messages.last().map(|m| m.content.as_str()).unwrap_or("");
        let text = if system.contains("summarize what happened") {
            "(mock ai) Marta asked whether Thursday is still on — that needs a reply.".to_string()
        } else if system.contains("draft a reply") {
            "(mock ai) Thursday works for me, see you at 7.".to_string()
        } else if system.contains("translate") {
            format!("(mock ai) translation: {last}")
        } else {
            format!("(mock ai) You said: {}", last.chars().take(80).collect::<String>())
        };
        return Ok(ChatReply { text, provider: "mock".into(), model: "mock".into() });
    }
    let keys = load_keys();
    let (provider, model) = pick_chat(prefs, &keys).await?;
    let text = match provider {
        "anthropic" => anthropic_chat(keys.anthropic.as_deref().unwrap(), &model, system, messages).await?,
        "openai" => openai_compat_chat("https://api.openai.com/v1", keys.openai.as_deref().unwrap(), &model, system, messages).await?,
        "groq" => openai_compat_chat("https://api.groq.com/openai/v1", keys.groq.as_deref().unwrap(), &model, system, messages).await?,
        "gemini" => gemini_chat(keys.gemini.as_deref().unwrap(), &model, system, messages).await?,
        "ollama" => ollama_chat(prefs, &model, system, messages).await?,
        _ => unreachable!(),
    };
    Ok(ChatReply { text, provider: provider.to_string(), model })
}

fn http_err(provider: &str, status: reqwest::StatusCode, body: &str) -> String {
    // Extract a human message if the body is JSON; never echo headers/keys.
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/error"))
                .and_then(|m| m.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    format!("{provider}: HTTP {status}: {msg}")
}

async fn anthropic_chat(key: &str, model: &str, system: &str, messages: &[ChatMessage]) -> Result<String, String> {
    let msgs: Vec<Value> = messages
        .iter()
        .map(|m| json!({"role": if m.role == Role::User {"user"} else {"assistant"}, "content": m.content}))
        .collect();
    let mut body = json!({
        "model": model,
        "max_tokens": 16000,
        "messages": msgs,
        // Server-side refusal fallback: a request declined by the safety
        // classifiers is re-run on Anthropic's recommended substitute model.
        "fallbacks": "default",
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    let resp = client()?
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("anthropic: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(http_err("anthropic", status, &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("anthropic: bad json: {e}"))?;
    if v.get("stop_reason").and_then(|s| s.as_str()) == Some("refusal") {
        let why = v.pointer("/stop_details/explanation").and_then(|s| s.as_str()).unwrap_or("declined by safety classifiers");
        return Err(format!("anthropic: {why}"));
    }
    let out: String = v
        .get("content")
        .and_then(|c| c.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if out.trim().is_empty() {
        return Err("anthropic: empty response".into());
    }
    Ok(out)
}

async fn openai_compat_chat(base: &str, key: &str, model: &str, system: &str, messages: &[ChatMessage]) -> Result<String, String> {
    let name = if base.contains("groq") { "groq" } else { "openai" };
    let mut msgs: Vec<Value> = Vec::new();
    if !system.is_empty() {
        msgs.push(json!({"role": "system", "content": system}));
    }
    msgs.extend(messages.iter().map(|m| json!({"role": if m.role == Role::User {"user"} else {"assistant"}, "content": m.content})));
    let resp = client()?
        .post(format!("{base}/chat/completions"))
        .bearer_auth(key)
        .json(&json!({"model": model, "messages": msgs}))
        .send()
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(http_err(name, status, &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{name}: bad json: {e}"))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{name}: empty response"))
}

async fn gemini_chat(key: &str, model: &str, system: &str, messages: &[ChatMessage]) -> Result<String, String> {
    let contents: Vec<Value> = messages
        .iter()
        .map(|m| json!({"role": if m.role == Role::User {"user"} else {"model"}, "parts": [{"text": m.content}]}))
        .collect();
    let mut body = json!({"contents": contents});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    let resp = client()?
        .post(format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"))
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("gemini: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(http_err("gemini", status, &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("gemini: bad json: {e}"))?;
    let out: String = v
        .pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array())
        .map(|parts| parts.iter().filter_map(|p| p.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    if out.trim().is_empty() {
        return Err("gemini: empty response".into());
    }
    Ok(out)
}

async fn ollama_chat(prefs: &Prefs, model: &str, system: &str, messages: &[ChatMessage]) -> Result<String, String> {
    let mut msgs: Vec<Value> = Vec::new();
    if !system.is_empty() {
        msgs.push(json!({"role": "system", "content": system}));
    }
    msgs.extend(messages.iter().map(|m| json!({"role": if m.role == Role::User {"user"} else {"assistant"}, "content": m.content})));
    let resp = client()?
        .post(format!("{}/api/chat", ollama_url(prefs)))
        .json(&json!({"model": model, "messages": msgs, "stream": false}))
        .send()
        .await
        .map_err(|e| format!("ollama: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(http_err("ollama", status, &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("ollama: bad json: {e}"))?;
    v.pointer("/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "ollama: empty response".into())
}

/// Transcribe an audio file (Telegram voice notes are .oga/.ogg opus).
pub async fn transcribe(prefs: &Prefs, path: &Path) -> Result<Transcript, String> {
    if mock_ai() {
        tokio::time::sleep(Duration::from_millis(300)).await;
        return Ok(Transcript {
            text: format!("(mock transcript of {})", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
            provider: "mock".into(),
        });
    }
    let keys = load_keys();
    let pinned = prefs.transcribe_provider.trim().to_lowercase();
    let candidates: Vec<&str> = if pinned.is_empty() { vec!["whisper", "groq", "openai"] } else { vec![pinned.as_str()] };
    for id in candidates {
        match id {
            "whisper" => {
                if let (Some(bin), Some(model)) = (whisper_binary(), whisper_model(&keys)) {
                    let text = whisper_local(&bin, &model, path).await?;
                    return Ok(Transcript { text, provider: "whisper".into() });
                }
            }
            "groq" if keys.groq.is_some() => {
                let text = audio_api("groq", "https://api.groq.com/openai/v1", keys.groq.as_deref().unwrap(), "whisper-large-v3-turbo", path).await?;
                return Ok(Transcript { text, provider: "groq".into() });
            }
            "openai" if keys.openai.is_some() => {
                let text = audio_api("openai", "https://api.openai.com/v1", keys.openai.as_deref().unwrap(), "whisper-1", path).await?;
                return Ok(Transcript { text, provider: "openai".into() });
            }
            _ => {}
        }
    }
    Err(if pinned.is_empty() {
        "no transcription provider available — install whisper.cpp (+ a ggml model) or add a groq/openai key".into()
    } else {
        format!("transcription provider '{pinned}' is not available")
    })
}

async fn audio_api(name: &str, base: &str, key: &str, model: &str, path: &Path) -> Result<String, String> {
    let bytes = tokio::fs::read(path).await.map_err(|e| format!("read audio: {e}"))?;
    let fname = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "audio.ogg".into());
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("oga") | Some("ogg") | Some("opus") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a") | Some("mp4") => "audio/mp4",
        _ => "application/octet-stream",
    };
    let part = reqwest::multipart::Part::bytes(bytes).file_name(fname).mime_str(mime).map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new().text("model", model.to_string()).part("file", part);
    let resp = client()?
        .post(format!("{base}/audio/transcriptions"))
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(http_err(name, status, &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{name}: bad json: {e}"))?;
    v.get("text").and_then(|t| t.as_str()).map(|s| s.trim().to_string()).ok_or_else(|| format!("{name}: no text in response"))
}

async fn whisper_local(bin: &Path, model: &Path, path: &Path) -> Result<String, String> {
    // whisper.cpp wants 16 kHz mono wav; ffmpeg converts anything Telegram sends.
    let wav = std::env::temp_dir().join(format!("omarchygram-{}-{}.wav", std::process::id(), chrono::Local::now().timestamp_millis()));
    let ff = tokio::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-ar", "16000", "-ac", "1", "-f", "wav"])
        .arg(&wav)
        .output()
        .await
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !ff.status.success() {
        return Err(format!("ffmpeg failed: {}", String::from_utf8_lossy(&ff.stderr).trim()));
    }
    let out = tokio::time::timeout(
        Duration::from_secs(300),
        tokio::process::Command::new(bin)
            .arg("-m").arg(model)
            .arg("-f").arg(&wav)
            .args(["-nt", "-np"])
            .output(),
    )
    .await
    .map_err(|_| "whisper timed out".to_string())?
    .map_err(|e| format!("whisper: {e}"))?;
    let _ = tokio::fs::remove_file(&wav).await;
    if !out.status.success() {
        return Err(format!("whisper failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return Err("whisper produced no text".into());
    }
    Ok(text)
}
