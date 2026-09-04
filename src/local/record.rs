//! Voice-note recording via ffmpeg (PipeWire/Pulse input → OGG Opus) and,
//! since wave 6F, video-note recording (camera → square h264 MP4 plus a live
//! RGBA preview stream). Orchestrator-owned: this spawns processes. One
//! recording of each kind at a time; files live in the private runtime dir
//! and are removed on cancel.

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

// ===================== wave 6F: video notes =====================

struct VideoRecording {
    child: Child,
    path: PathBuf,
    started: Instant,
    size: u32,
}

fn video_slot() -> &'static Mutex<Option<VideoRecording>> {
    static SLOT: OnceLock<Mutex<Option<VideoRecording>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Where the camera comes from: `OMG_CAMERA` (a /dev/videoN path, or "test"
/// for ffmpeg's test pattern), else the first /dev/video*, else — only in
/// smoke/mock mode — the test pattern. `OMG_MOCK_CAMERA=none` forces "no
/// camera" (probe hook).
fn camera_source() -> Result<Option<PathBuf>, String> {
    if std::env::var("OMG_MOCK_CAMERA").is_ok_and(|v| v == "none") {
        return Err("no camera found".into());
    }
    if let Some(p) = std::env::var_os("OMG_CAMERA") {
        if p == "test" {
            return Ok(None);
        }
        let p = PathBuf::from(p);
        return if p.exists() { Ok(Some(p)) } else { Err(format!("camera {} does not exist", p.display())) };
    }
    let mut devices: Vec<PathBuf> = std::fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("video")))
        .collect();
    devices.sort();
    if let Some(first) = devices.into_iter().next() {
        return Ok(Some(first));
    }
    if mock_mode() {
        return Ok(None);
    }
    Err("no camera found".into())
}

/// Starts recording a `size`×`size` video circle (h264 MP4, ≤ 60 s, with
/// microphone audio when a real camera is used). Preview frames — RGBA,
/// `size`×`size`, ~10 fps — arrive on the returned receiver; the newest frame
/// wins when the UI is slow (never more than 2 buffered).
pub async fn video_start(size: u32) -> Result<async_channel::Receiver<Vec<u8>>, String> {
    let size = size.clamp(64, 640) & !1; // h264 wants even dimensions
    let mut guard = video_slot().lock().await;
    // A start right after a cancel/stop must not race the previous teardown
    // (the UI fires cancel without awaiting it): the new recording supersedes
    // whatever is still in the slot.
    if let Some(mut stale) = guard.take() {
        let _ = stale.child.kill().await;
        let _ = std::fs::remove_file(&stale.path);
    }
    if !ffmpeg_available() {
        return Err("ffmpeg is not installed — run: sudo pacman -S ffmpeg".into());
    }
    let camera = camera_source()?;
    let path = new_path().with_extension("mp4");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-nostdin", "-loglevel", "error"]);
    match &camera {
        Some(dev) => {
            cmd.args(["-f", "v4l2", "-framerate", "30", "-video_size", "640x480", "-i"]).arg(dev);
            cmd.args(["-f", "pulse", "-i", "default"]);
        }
        None => {
            cmd.args(["-f", "lavfi", "-i", "testsrc=size=640x480:rate=30"]);
            cmd.args(["-f", "lavfi", "-i", "sine=frequency=440"]);
        }
    }
    let filter = format!(
        "[0:v]crop='min(iw,ih)':'min(iw,ih)',scale={size}:{size},split=2[rec][pre];[pre]fps=10,format=rgba[preview]"
    );
    cmd.args(["-filter_complex", &filter]);
    cmd.args(["-map", "[rec]", "-map", "1:a", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p", "-r", "30"]);
    cmd.args(["-c:a", "aac", "-b:a", "64k", "-t", "60", "-movflags", "+faststart"]);
    cmd.arg(&path);
    cmd.args(["-map", "[preview]", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"]);
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let (tx, rx) = async_channel::bounded::<Vec<u8>>(2);
    let drain = rx.clone();
    let mut stdout = child.stdout.take().expect("piped stdout");
    let frame_len = (size * size * 4) as usize;
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut frame = vec![0u8; frame_len];
        loop {
            if stdout.read_exact(&mut frame).await.is_err() {
                break;
            }
            // Drop the oldest queued frame rather than stall ffmpeg.
            if tx.is_full() {
                let _ = drain.try_recv();
            }
            if tx.try_send(frame.clone()).is_err() && tx.is_closed() {
                break;
            }
        }
    });
    *guard = Some(VideoRecording { child, path, started: Instant::now(), size });
    Ok(rx)
}

/// Stops the video recording and returns (mp4 path, duration secs ≥ 1).
pub async fn video_stop() -> Result<(PathBuf, u32), String> {
    let mut guard = video_slot().lock().await;
    let Some(mut rec) = guard.take() else {
        return Err("no video recording is running".into());
    };
    let secs = rec.started.elapsed().as_secs_f64().ceil().clamp(1.0, 60.0) as u32;
    if let Some(mut stdin) = rec.child.stdin.take() {
        let _ = stdin.write_all(b"q\n").await;
    }
    if let Some(pid) = rec.child.id() {
        unsafe { libc::kill(pid as i32, libc::SIGINT) };
    }
    let waited = tokio::time::timeout(std::time::Duration::from_secs(4), rec.child.wait()).await;
    if waited.is_err() {
        let _ = rec.child.kill().await;
    }
    let size = std::fs::metadata(&rec.path).map(|m| m.len()).unwrap_or(0);
    if size < 1000 {
        let _ = std::fs::remove_file(&rec.path);
        return Err("nothing was recorded — is the camera working?".into());
    }
    let _ = rec.size;
    Ok((rec.path, secs))
}

pub async fn video_cancel() {
    let mut guard = video_slot().lock().await;
    if let Some(mut rec) = guard.take() {
        let _ = rec.child.kill().await;
        let _ = std::fs::remove_file(&rec.path);
    }
}

#[cfg(test)]
mod video_tests {
    use super::*;

    #[tokio::test]
    async fn test_pattern_records_a_circle_and_streams_frames() {
        if !ffmpeg_available() {
            return;
        }
        unsafe { std::env::set_var("OMG_CAMERA", "test") };
        let rx = video_start(120).await.expect("start");
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await.expect("a frame in time").expect("frame");
        assert_eq!(first.len(), 120 * 120 * 4);
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let (path, secs) = video_stop().await.expect("stop");
        assert!(path.exists());
        assert!(secs >= 1 && secs <= 5, "secs {secs}");
        assert!(std::fs::metadata(&path).unwrap().len() > 1000);
        let _ = std::fs::remove_file(&path);
        assert!(video_stop().await.is_err(), "nothing running any more");
    }
}
