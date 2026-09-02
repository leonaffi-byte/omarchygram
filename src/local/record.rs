//! Voice-note recording via ffmpeg (PipeWire/Pulse input → OGG Opus).
//! Orchestrator-owned: this spawns a process. One recording at a time;
//! files live in the private runtime dir and are removed on cancel.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

struct Recording {
    child: Option<Child>,
    path: PathBuf,
    started: Instant,
    mock: bool,
}

fn slot() -> &'static Mutex<Option<Recording>> {
    static SLOT: OnceLock<Mutex<Option<Recording>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn mock_mode() -> bool {
    std::env::var("OMG_MOCK_AI").is_ok_and(|v| !v.is_empty())
}

fn new_path() -> PathBuf {
    let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
    base.join(format!(
        "omarchygram-voice-{}-{}.ogg",
        std::process::id(),
        Instant::now().elapsed().as_nanos() as u64 ^ chrono::Local::now().timestamp_nanos_opt().unwrap_or(0) as u64
    ))
}

fn ffmpeg_available() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("ffmpeg").is_file()))
        .unwrap_or(false)
}

pub async fn start() -> Result<(), String> {
    let mut guard = slot().lock().await;
    if guard.is_some() {
        return Err("a recording is already running".into());
    }
    let path = new_path();
    if mock_mode() {
        *guard = Some(Recording { child: None, path, started: Instant::now(), mock: true });
        return Ok(());
    }
    if !ffmpeg_available() {
        return Err("ffmpeg is not installed — run: sudo pacman -S ffmpeg".into());
    }
    let child = Command::new("ffmpeg")
        .args([
            "-y", "-nostdin", "-loglevel", "error", "-f", "pulse", "-i", "default", "-ac", "1", "-ar", "48000",
            "-c:a", "libopus", "-b:a", "32k", "-application", "voip",
        ])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    *guard = Some(Recording { child: Some(child), path, started: Instant::now(), mock: false });
    Ok(())
}

/// Stops and returns the file + duration in seconds (≥1).
pub async fn stop() -> Result<(PathBuf, u32), String> {
    let mut guard = slot().lock().await;
    let Some(mut rec) = guard.take() else {
        return Err("no recording is running".into());
    };
    let secs = rec.started.elapsed().as_secs_f64().ceil().max(1.0) as u32;
    if rec.mock {
        std::fs::write(&rec.path, b"OggS mock voice note").map_err(|e| e.to_string())?;
        return Ok((rec.path, secs));
    }
    let mut child = rec.child.take().expect("real recording has a child");
    // ffmpeg finalizes the container on SIGINT; wait briefly, then force.
    if let Some(pid) = child.id() {
        unsafe { libc::kill(pid as i32, libc::SIGINT) };
    }
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"q\n").await;
    }
    let waited = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await;
    if waited.is_err() {
        let _ = child.kill().await;
    }
    let size = std::fs::metadata(&rec.path).map(|m| m.len()).unwrap_or(0);
    if size < 100 {
        let _ = std::fs::remove_file(&rec.path);
        return Err("nothing was recorded — is a microphone connected?".into());
    }
    Ok((rec.path, secs))
}

pub async fn cancel() {
    let mut guard = slot().lock().await;
    if let Some(mut rec) = guard.take() {
        if let Some(mut child) = rec.child.take() {
            let _ = child.kill().await;
        }
        let _ = std::fs::remove_file(&rec.path);
    }
}
