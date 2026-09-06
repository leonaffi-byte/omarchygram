//! Local services bridge: hosts AI calls and OS actions on their own tokio
//! runtime thread, mirroring the `Tg` bridge. UI code awaits these on the
//! GLib main context. Orchestrator-owned.

use std::path::PathBuf;

use tokio::sync::{mpsc, oneshot};

use crate::ai::{self, ChatMessage, ChatReply, Prefs, ProviderInfo, Transcript};
use crate::os::{self, Action, OsPolicy, ShellTicket};

pub mod record;

enum Cmd {
    Detect(Prefs, oneshot::Sender<Vec<ProviderInfo>>),
    Chat {
        prefs: Prefs,
        system: String,
        messages: Vec<ChatMessage>,
        respond: oneshot::Sender<Result<ChatReply, String>>,
    },
    Transcribe {
        prefs: Prefs,
        path: PathBuf,
        respond: oneshot::Sender<Result<Transcript, String>>,
    },
    OsRun {
        action: Action,
        args: Vec<String>,
        policy: OsPolicy,
        respond: oneshot::Sender<Result<String, String>>,
    },
    /// Only with a ticket from `os::request_shell` + the UI's confirmation.
    OsShell {
        ticket: ShellTicket,
        respond: oneshot::Sender<Result<String, String>>,
    },
    RecordStart(oneshot::Sender<Result<(), String>>),
    RecordStop(oneshot::Sender<Result<(PathBuf, u32), String>>),
    RecordCancel(oneshot::Sender<()>),
    VideoStart(u32, oneshot::Sender<Result<async_channel::Receiver<Vec<u8>>, String>>),
    VideoStop(oneshot::Sender<Result<(PathBuf, u32), String>>),
    VideoCancel(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct Local {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Local {
    pub fn spawn() -> Local {
        let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
        std::thread::spawn(move || {
            // Wave 8B: local services mostly await subprocesses (AI, whisper,
            // OS actions) — 2 workers instead of one-per-core (was 16) saves
            // ~14 idle threads; commands still each spawn a concurrent task.
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime (local services)");
            rt.block_on(async move {
                while let Some(cmd) = rx.recv().await {
                    // Every command runs concurrently; a slow transcription
                    // never blocks an OS action.
                    tokio::spawn(async move {
                        match cmd {
                            Cmd::Detect(prefs, tx) => {
                                let _ = tx.send(ai::detect(&prefs).await);
                            }
                            Cmd::Chat { prefs, system, messages, mut respond } => {
                                tokio::select! {
                                    _ = respond.closed() => {},
                                    result = ai::chat(&prefs, &system, &messages) => { let _ = respond.send(result); }
                                }
                            }
                            Cmd::Transcribe { prefs, path, mut respond } => {
                                tokio::select! {
                                    _ = respond.closed() => {},
                                    result = ai::transcribe(&prefs, &path) => { let _ = respond.send(result); }
                                }
                            }
                            Cmd::OsRun { action, args, policy, respond } => {
                                let _ = respond.send(os::run_action(&action, &args, policy).await);
                            }
                            Cmd::OsShell { ticket, respond } => {
                                let _ = respond.send(os::run_shell(ticket).await);
                            }
                            Cmd::RecordStart(tx) => {
                                let _ = tx.send(record::start().await);
                            }
                            Cmd::RecordStop(tx) => {
                                let _ = tx.send(record::stop().await);
                            }
                            Cmd::RecordCancel(tx) => {
                                record::cancel().await;
                                let _ = tx.send(());
                            }
                            Cmd::VideoStart(size, tx) => {
                                let _ = tx.send(record::video_start(size).await);
                            }
                            Cmd::VideoStop(tx) => {
                                let _ = tx.send(record::video_stop().await);
                            }
                            Cmd::VideoCancel(tx) => {
                                record::video_cancel().await;
                                let _ = tx.send(());
                            }
                        }
                    });
                }
            });
        });
        Local { tx }
    }

    pub async fn detect(&self, prefs: Prefs) -> Vec<ProviderInfo> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Cmd::Detect(prefs, tx));
        rx.await.unwrap_or_default()
    }

    pub async fn chat(&self, prefs: Prefs, system: String, messages: Vec<ChatMessage>) -> Result<ChatReply, String> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Chat { prefs, system, messages, respond })
            .map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    pub async fn transcribe(&self, prefs: Prefs, path: PathBuf) -> Result<Transcript, String> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Transcribe { prefs, path, respond })
            .map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    /// `policy` is the CURRENT settings (re-read at dispatch), enforced here
    /// as well as in the UI.
    pub async fn os_run(&self, action: Action, args: Vec<String>, policy: OsPolicy) -> Result<String, String> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(Cmd::OsRun { action, args, policy, respond })
            .map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    /// Caller contract: the ticket came from `os::request_shell`, the user
    /// confirmed exactly `ticket.command()` in a dialog, and `os.enabled &&
    /// os.shell` were re-checked at that moment.
    pub async fn os_shell_confirmed(&self, ticket: ShellTicket) -> Result<String, String> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(Cmd::OsShell { ticket, respond })
            .map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    /// Start recording a voice note (ffmpeg → OGG Opus). One at a time.
    pub async fn record_start(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::RecordStart(tx)).map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    /// Stop and get the file plus its duration in seconds.
    pub async fn record_stop(&self) -> Result<(PathBuf, u32), String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::RecordStop(tx)).map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    pub async fn record_cancel(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::RecordCancel(tx)).is_ok() {
            let _ = rx.await;
        }
    }

    // ----- wave 6F: video notes -----

    /// Start recording a `size`×`size` video circle (ffmpeg: camera → h264
    /// MP4, ≤ 60 s). The receiver streams RGBA preview frames (`size`×`size`,
    /// ~10 fps); await it on the GLib main context and wrap each frame in a
    /// `gdk::MemoryTexture` (R8g8b8a8). Camera: `OMG_CAMERA` (device path or
    /// "test"), else the first /dev/video*, else the test pattern in smoke
    /// mode; `OMG_MOCK_CAMERA=none` → Err("no camera found").
    pub async fn video_start(&self, size: u32) -> Result<async_channel::Receiver<Vec<u8>>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::VideoStart(size, tx)).map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    /// Stop and get the MP4 plus its duration in seconds.
    pub async fn video_stop(&self) -> Result<(PathBuf, u32), String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::VideoStop(tx)).map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    pub async fn video_cancel(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::VideoCancel(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}
