//! Local services bridge: hosts AI calls and OS actions on their own tokio
//! runtime thread, mirroring the `Tg` bridge. UI code awaits these on the
//! GLib main context. Orchestrator-owned.

use std::path::PathBuf;

use tokio::sync::{mpsc, oneshot};

use crate::ai::{self, ChatMessage, ChatReply, Prefs, ProviderInfo, Transcript};
use crate::os::{self, Action};

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
        respond: oneshot::Sender<Result<String, String>>,
    },
    /// Only after the UI's confirmation dialog (see os::run_shell).
    OsShell {
        cmdline: String,
        respond: oneshot::Sender<Result<String, String>>,
    },
}

#[derive(Clone)]
pub struct Local {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Local {
    pub fn spawn() -> Local {
        let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime (local services)");
            rt.block_on(async move {
                while let Some(cmd) = rx.recv().await {
                    // Every command runs concurrently; a slow transcription
                    // never blocks an OS action.
                    tokio::spawn(async move {
                        match cmd {
                            Cmd::Detect(prefs, tx) => {
                                let _ = tx.send(ai::detect(&prefs).await);
                            }
                            Cmd::Chat { prefs, system, messages, respond } => {
                                let _ = respond.send(ai::chat(&prefs, &system, &messages).await);
                            }
                            Cmd::Transcribe { prefs, path, respond } => {
                                let _ = respond.send(ai::transcribe(&prefs, &path).await);
                            }
                            Cmd::OsRun { action, args, respond } => {
                                let _ = respond.send(os::run_action(&action, &args).await);
                            }
                            Cmd::OsShell { cmdline, respond } => {
                                let _ = respond.send(os::run_shell(&cmdline).await);
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

    pub async fn os_run(&self, action: Action, args: Vec<String>) -> Result<String, String> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(Cmd::OsRun { action, args, respond })
            .map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }

    /// Caller contract: `os.shell` enabled AND the user confirmed this exact
    /// command line in a dialog.
    pub async fn os_shell_confirmed(&self, cmdline: String) -> Result<String, String> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(Cmd::OsShell { cmdline, respond })
            .map_err(|_| "local services are gone".to_string())?;
        rx.await.map_err(|_| "local services dropped the request".to_string())?
    }
}
