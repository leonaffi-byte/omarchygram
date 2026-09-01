//! Telegram backend bridge. Orchestrator-owned: do not modify in delegations.
//!
//! The backend (grammers + tokio) runs on its own thread; the UI talks to it
//! through this module only, in the plain types below — never in grammers
//! types. All `Tg` methods are async and safe to await on the GLib main
//! context (`glib::MainContext::spawn_local`); `Event`s are read from
//! `Tg::events` the same way. Both mock and real backends behave identically.

mod mock;
mod real;

use std::path::PathBuf;

use chrono::{DateTime, Local};
use tokio::sync::{mpsc, oneshot};

pub mod paths {
    use std::path::PathBuf;

    pub fn config_file() -> PathBuf {
        dirs::config_dir().unwrap_or_default().join("omarchygram/config.toml")
    }

    pub fn session_file() -> PathBuf {
        dirs::data_dir().unwrap_or_default().join("omarchygram/omarchygram.session")
    }

    pub fn media_dir() -> PathBuf {
        dirs::cache_dir().unwrap_or_default().join("omarchygram/media")
    }
}

pub const SETUP_HELP: &str = "Omarchygram needs Telegram API credentials (one-time setup):

  1. Log in at https://my.telegram.org/apps with your Telegram account
  2. Create an application (any name, platform \"Desktop\")
  3. Save the credentials:

     mkdir -p ~/.config/omarchygram
     cat > ~/.config/omarchygram/config.toml <<EOF
     api_id = <your api_id>
     api_hash = \"<your api_hash>\"
     EOF
     chmod 600 ~/.config/omarchygram/config.toml
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthState {
    /// config.toml missing/invalid
    NeedCredentials,
    NeedPhone,
    NeedCode,
    /// 2FA
    NeedPassword,
    Ready,
}

#[derive(Debug, Clone)]
pub struct ChatSummary {
    pub id: i64,
    pub title: String,
    pub last_message: String,
    pub last_time: Option<DateTime<Local>>,
    pub unread: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Sticker,
    Voice,
    Document,
}

#[derive(Debug, Clone)]
pub struct Reaction {
    pub emoji: String,
    pub count: i32,
}

#[derive(Debug, Clone)]
pub struct Msg {
    pub id: i32,
    pub chat_id: i64,
    /// Display name; empty when unknown, "You" for own messages.
    pub sender: String,
    pub text: String,
    pub ts: DateTime<Local>,
    pub outgoing: bool,
    pub media: Option<MediaKind>,
    /// Filename for MediaKind::Document.
    pub doc_name: Option<String>,
    /// Id of the replied-to message in the same chat.
    pub reply_to: Option<i32>,
    pub reactions: Vec<Reaction>,
    pub edited: bool,
}

/// Pushed by the backend; read via `Tg::events`. Arrive on whatever context
/// awaits them — in this app, the GLib main context, so widgets may be touched
/// directly in the receive loop.
#[derive(Debug, Clone)]
pub enum Event {
    NewMessage(Msg),
    /// Edits and reaction updates to already-displayed messages.
    MessageChanged(Msg),
    /// name may be empty. UI owns the "X is typing" timeout (suggest 5s).
    Typing { chat_id: i64, name: String },
}

/// User-facing error text; show it, don't parse it.
pub type TgError = String;

enum Command {
    Start(oneshot::Sender<Result<AuthState, TgError>>),
    SubmitPhone(String, oneshot::Sender<Result<AuthState, TgError>>),
    SubmitCode(String, oneshot::Sender<Result<AuthState, TgError>>),
    SubmitPassword(String, oneshot::Sender<Result<AuthState, TgError>>),
    GetDialogs(oneshot::Sender<Result<Vec<ChatSummary>, TgError>>),
    GetHistory {
        chat_id: i64,
        before_id: Option<i32>,
        respond: oneshot::Sender<Result<Vec<Msg>, TgError>>,
    },
    DownloadMedia {
        chat_id: i64,
        msg_id: i32,
        respond: oneshot::Sender<Result<Option<PathBuf>, TgError>>,
    },
    SendText {
        chat_id: i64,
        text: String,
        reply_to: Option<i32>,
        respond: oneshot::Sender<Result<Msg, TgError>>,
    },
    SendFile {
        chat_id: i64,
        path: PathBuf,
        caption: String,
        respond: oneshot::Sender<Result<Msg, TgError>>,
    },
    EditText {
        chat_id: i64,
        msg_id: i32,
        text: String,
        respond: oneshot::Sender<Result<Msg, TgError>>,
    },
    DeleteMessage {
        chat_id: i64,
        msg_id: i32,
        respond: oneshot::Sender<Result<(), TgError>>,
    },
    MarkRead {
        chat_id: i64,
        respond: oneshot::Sender<Result<(), TgError>>,
    },
}

/// UI-side handle to the backend thread. Cheap to clone.
#[derive(Clone)]
pub struct Tg {
    cmds: mpsc::UnboundedSender<Command>,
    /// Clone the receiver only once; a single Shell-owned event loop is the model.
    pub events: async_channel::Receiver<Event>,
    pub is_mock: bool,
}

macro_rules! roundtrip {
    ($self:ident, $cmd:expr) => {{
        let (tx, rx) = oneshot::channel();
        $self
            .cmds
            .send($cmd(tx))
            .map_err(|_| "backend is gone".to_string())?;
        rx.await.map_err(|_| "backend dropped the request".to_string())?
    }};
}

impl Tg {
    pub fn spawn_mock() -> Tg {
        Self::spawn(true)
    }

    pub fn spawn_real() -> Tg {
        Self::spawn(false)
    }

    fn spawn(mock: bool) -> Tg {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            if mock {
                rt.block_on(mock::run(cmd_rx, event_tx));
            } else {
                rt.block_on(real::run(cmd_rx, event_tx));
            }
        });
        Tg {
            cmds: cmd_tx,
            events: event_rx,
            is_mock: mock,
        }
    }

    pub async fn start(&self) -> Result<AuthState, TgError> {
        roundtrip!(self, Command::Start)
    }

    pub async fn submit_phone(&self, phone: &str) -> Result<AuthState, TgError> {
        let phone = phone.trim().to_string();
        roundtrip!(self, |tx| Command::SubmitPhone(phone, tx))
    }

    pub async fn submit_code(&self, code: &str) -> Result<AuthState, TgError> {
        let code = code.trim().to_string();
        roundtrip!(self, |tx| Command::SubmitCode(code, tx))
    }

    pub async fn submit_password(&self, password: &str) -> Result<AuthState, TgError> {
        let password = password.to_string();
        roundtrip!(self, |tx| Command::SubmitPassword(password, tx))
    }

    pub async fn get_dialogs(&self) -> Result<Vec<ChatSummary>, TgError> {
        roundtrip!(self, Command::GetDialogs)
    }

    /// Newest last (display order). `before_id` pages older messages; an empty
    /// page means there is nothing older.
    pub async fn get_history(
        &self,
        chat_id: i64,
        before_id: Option<i32>,
    ) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |tx| Command::GetHistory {
            chat_id,
            before_id,
            respond: tx
        })
    }

    /// Downloads to the media cache and returns a GTK-renderable path
    /// (stickers are converted webp -> png). Cached across calls.
    /// None when the message has no media.
    pub async fn download_media(
        &self,
        chat_id: i64,
        msg_id: i32,
    ) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadMedia {
            chat_id,
            msg_id,
            respond: tx
        })
    }

    pub async fn send_text(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i32>,
    ) -> Result<Msg, TgError> {
        let text = text.to_string();
        roundtrip!(self, |tx| Command::SendText {
            chat_id,
            text,
            reply_to,
            respond: tx
        })
    }

    pub async fn send_file(
        &self,
        chat_id: i64,
        path: PathBuf,
        caption: &str,
    ) -> Result<Msg, TgError> {
        let caption = caption.to_string();
        roundtrip!(self, |tx| Command::SendFile {
            chat_id,
            path,
            caption,
            respond: tx
        })
    }

    pub async fn edit_text(
        &self,
        chat_id: i64,
        msg_id: i32,
        text: &str,
    ) -> Result<Msg, TgError> {
        let text = text.to_string();
        roundtrip!(self, |tx| Command::EditText {
            chat_id,
            msg_id,
            text,
            respond: tx
        })
    }

    pub async fn delete_message(&self, chat_id: i64, msg_id: i32) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::DeleteMessage {
            chat_id,
            msg_id,
            respond: tx
        })
    }

    pub async fn mark_read(&self, chat_id: i64) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::MarkRead {
            chat_id,
            respond: tx
        })
    }
}
