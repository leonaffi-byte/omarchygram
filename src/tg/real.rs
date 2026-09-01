//! grammers-backed Telegram backend. Orchestrator-owned: do not modify in delegations.
//!
//! Runs inside the backend tokio runtime (see tg/mod.rs). The UI never sees
//! grammers types; everything is converted at this boundary.
//!
//! Chat ids exposed to the UI are Bot-API dialog ids (what `PeerId`
//! bitpacks): positive for users, negative for groups/channels — no collisions.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Local;
use grammers_client::client::UpdatesConfiguration;
use grammers_client::media::{Document, Media};
use grammers_client::message::{InputMessage, Message};
use grammers_client::session::storages::SqliteSession;
use grammers_client::session::types::{PeerId, PeerRef};
use grammers_client::session::updates::UpdatesLike;
use grammers_client::update::Update;
use grammers_client::{Client, SenderPool, SignInError};
use grammers_tl_types as tl;
use tokio::sync::mpsc;

use super::{
    paths, AuthState, ChatSummary, Command, Event, MediaKind, Msg, Reaction, TgError,
};

/// Shared between the command loop and the update loop.
struct Ctx {
    peers: Mutex<HashMap<i64, PeerRef>>,
    titles: Mutex<HashMap<i64, String>>,
    media: Mutex<HashMap<(i64, i32), Media>>,
}

impl Ctx {
    fn remember(&self, chat_id: i64, peer_ref: PeerRef, title: Option<&str>) {
        self.peers.lock().unwrap().insert(chat_id, peer_ref);
        if let Some(title) = title {
            self.titles.lock().unwrap().insert(chat_id, title.to_string());
        }
    }

    fn peer(&self, chat_id: i64) -> Result<PeerRef, TgError> {
        self.peers
            .lock()
            .unwrap()
            .get(&chat_id)
            .copied()
            .ok_or_else(|| format!("unknown chat {chat_id} — reopen it from the chat list"))
    }
}

struct Backend {
    client: Option<Client>,
    api_hash: Option<String>,
    login_token: Option<grammers_client::client::LoginToken>,
    password_token: Option<grammers_client::client::PasswordToken>,
    updates_rx: Option<mpsc::UnboundedReceiver<UpdatesLike>>,
    session_path: PathBuf,
    ctx: Arc<Ctx>,
    events: async_channel::Sender<Event>,
    update_loop_started: bool,
}

fn load_credentials() -> Option<(i32, String)> {
    let text = std::fs::read_to_string(paths::config_file()).ok()?;
    let parsed = text.parse::<toml::Table>().ok()?;
    let api_id = parsed.get("api_id")?.as_integer()? as i32;
    let api_hash = parsed.get("api_hash")?.as_str()?.to_string();
    Some((api_id, api_hash))
}

fn chmod_600(path: &std::path::Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perm = meta.permissions();
        perm.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perm);
    }
}

pub async fn run(mut cmds: mpsc::UnboundedReceiver<Command>, events: async_channel::Sender<Event>) {
    let mut be = Backend {
        client: None,
        api_hash: None,
        login_token: None,
        password_token: None,
        updates_rx: None,
        session_path: paths::session_file(),
        ctx: Arc::new(Ctx {
            peers: Mutex::new(HashMap::new()),
            titles: Mutex::new(HashMap::new()),
            media: Mutex::new(HashMap::new()),
        }),
        events,
        update_loop_started: false,
    };

    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Command::Start(tx) => {
                let _ = tx.send(be.start().await);
            }
            Command::SubmitPhone(phone, tx) => {
                let _ = tx.send(be.submit_phone(&phone).await);
            }
            Command::SubmitCode(code, tx) => {
                let _ = tx.send(be.submit_code(&code).await);
            }
            Command::SubmitPassword(password, tx) => {
                let _ = tx.send(be.submit_password(&password).await);
            }
            Command::GetDialogs(tx) => {
                let _ = tx.send(be.get_dialogs().await);
            }
            Command::GetHistory { chat_id, before_id, respond } => {
                let _ = respond.send(be.get_history(chat_id, before_id).await);
            }
            Command::DownloadMedia { chat_id, msg_id, respond } => {
                let _ = respond.send(be.download_media(chat_id, msg_id).await);
            }
            Command::SendText { chat_id, text, reply_to, respond } => {
                let _ = respond.send(be.send_text(chat_id, &text, reply_to).await);
            }
            Command::SendFile { chat_id, path, caption, respond } => {
                let _ = respond.send(be.send_file(chat_id, &path, &caption).await);
            }
            Command::EditText { chat_id, msg_id, text, respond } => {
                let _ = respond.send(be.edit_text(chat_id, msg_id, &text).await);
            }
            Command::DeleteMessage { chat_id, msg_id, respond } => {
                let _ = respond.send(be.delete_message(chat_id, msg_id).await);
            }
            Command::MarkRead { chat_id, respond } => {
                let _ = respond.send(be.mark_read(chat_id).await);
            }
        }
    }
}

impl Backend {
    fn client(&self) -> Result<&Client, TgError> {
        self.client.as_ref().ok_or_else(|| "not connected".to_string())
    }

    fn on_authorized(&mut self) {
        if self.update_loop_started {
            return;
        }
        let Some(updates_rx) = self.updates_rx.take() else {
            return;
        };
        self.update_loop_started = true;
        let client = self.client.as_ref().unwrap().clone();
        let ctx = self.ctx.clone();
        let events = self.events.clone();
        tokio::spawn(update_loop(client, updates_rx, ctx, events));
    }

    async fn start(&mut self) -> Result<AuthState, TgError> {
        let Some((api_id, api_hash)) = load_credentials() else {
            return Ok(AuthState::NeedCredentials);
        };
        self.api_hash = Some(api_hash);
        if let Some(dir) = self.session_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let session = Arc::new(
            SqliteSession::open(&self.session_path)
                .await
                .map_err(|e| format!("could not open session: {e}"))?,
        );
        chmod_600(&self.session_path);
        let SenderPool { runner, updates, handle } = SenderPool::new(session, api_id);
        tokio::spawn(runner.run());
        let client = Client::new(handle);
        let authorized = client.is_authorized().await.map_err(|e| e.to_string())?;
        self.client = Some(client);
        self.updates_rx = Some(updates);
        if authorized {
            self.on_authorized();
            Ok(AuthState::Ready)
        } else {
            Ok(AuthState::NeedPhone)
        }
    }

    async fn submit_phone(&mut self, phone: &str) -> Result<AuthState, TgError> {
        let api_hash = self.api_hash.clone().ok_or_else(|| "not connected".to_string())?;
        let token = self
            .client()?
            .request_login_code(phone, &api_hash)
            .await
            .map_err(|e| format!("could not send the code: {e}"))?;
        self.login_token = Some(token);
        Ok(AuthState::NeedCode)
    }

    async fn submit_code(&mut self, code: &str) -> Result<AuthState, TgError> {
        let token = self
            .login_token
            .as_ref()
            .ok_or_else(|| "enter your phone first".to_string())?;
        match self.client()?.sign_in(token, code).await {
            Ok(_) => {
                self.on_authorized();
                Ok(AuthState::Ready)
            }
            Err(SignInError::PasswordRequired(ptoken)) => {
                self.password_token = Some(ptoken);
                Ok(AuthState::NeedPassword)
            }
            Err(SignInError::InvalidCode) => {
                Err("That code is not right — check Telegram and try again.".into())
            }
            Err(e) => Err(format!("sign-in failed: {e}")),
        }
    }

    async fn submit_password(&mut self, password: &str) -> Result<AuthState, TgError> {
        let token = self
            .password_token
            .take()
            .ok_or_else(|| "enter the login code first".to_string())?;
        match self.client()?.check_password(token, password).await {
            Ok(_) => {
                self.on_authorized();
                Ok(AuthState::Ready)
            }
            Err(SignInError::InvalidPassword(fresh_token)) => {
                // The server hands back a fresh token so the user can retry.
                self.password_token = Some(fresh_token);
                Err("Wrong password — try again.".into())
            }
            Err(e) => Err(format!("password check failed: {e}")),
        }
    }

    async fn get_dialogs(&mut self) -> Result<Vec<ChatSummary>, TgError> {
        let client = self.client()?.clone();
        let mut iter = client.iter_dialogs().limit(50);
        let mut out = Vec::new();
        while let Some(dialog) = iter.next().await.map_err(|e| e.to_string())? {
            let chat_id = dialog.peer_id().bot_api_dialog_id_unchecked();
            let title = dialog.peer().name().unwrap_or("Unknown").to_string();
            self.ctx.remember(chat_id, dialog.peer_ref(), Some(&title));
            let last = dialog.last_message.as_ref();
            let preview = last
                .map(|m| {
                    if m.text().is_empty() {
                        media_placeholder(m.media().as_ref()).to_string()
                    } else {
                        m.text().to_string()
                    }
                })
                .unwrap_or_default();
            let unread = match &dialog.raw {
                tl::enums::Dialog::Dialog(d) => d.unread_count,
                tl::enums::Dialog::Folder(_) => 0,
            };
            if let Some(m) = last {
                register_media(&self.ctx, m, chat_id);
            }
            out.push(ChatSummary {
                id: chat_id,
                title,
                last_message: preview,
                last_time: last.map(|m| m.date().with_timezone(&Local)),
                unread,
            });
        }
        Ok(out)
    }

    async fn get_history(
        &mut self,
        chat_id: i64,
        before_id: Option<i32>,
    ) -> Result<Vec<Msg>, TgError> {
        let peer = self.ctx.peer(chat_id)?;
        let client = self.client()?.clone();
        let mut iter = client.iter_messages(peer).limit(50);
        if let Some(before) = before_id {
            iter = iter.offset_id(before);
        }
        let mut out = Vec::new();
        while let Some(m) = iter.next().await.map_err(|e| e.to_string())? {
            out.push(convert(&self.ctx, &m, chat_id));
            if out.len() >= 50 {
                break;
            }
        }
        out.reverse(); // newest last (display order)
        Ok(out)
    }

    async fn download_media(
        &mut self,
        chat_id: i64,
        msg_id: i32,
    ) -> Result<Option<PathBuf>, TgError> {
        let media = { self.ctx.media.lock().unwrap().get(&(chat_id, msg_id)).cloned() };
        let Some(media) = media else {
            return Ok(None);
        };
        let dir = paths::media_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stem = format!("{chat_id}_{msg_id}");

        // Serve from cache when already downloaded (webp never survives; see below).
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(&format!("{stem}.")) && !name.ends_with(".webp") {
                    return Ok(Some(entry.path()));
                }
            }
        }

        let ext = match &media {
            Media::Photo(_) => "jpg".to_string(),
            Media::Sticker(_) => "webp".to_string(),
            Media::Document(d) => d
                .name()
                .and_then(|n| std::path::Path::new(n).extension())
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_else(|| "bin".to_string()),
            _ => "bin".to_string(),
        };
        let path = dir.join(format!("{stem}.{ext}"));
        self.client()?
            .download_media(&media, &path)
            .await
            .map_err(|e| format!("download failed: {e}"))?;

        // GdkPixbuf on this system has no webp loader; convert stickers to png.
        // (Animated .tgs stickers fail to decode; the UI shows them as unavailable.)
        if ext == "webp" {
            let png = dir.join(format!("{stem}.png"));
            let img = image::open(&path).map_err(|e| format!("sticker decode failed: {e}"))?;
            img.save(&png).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&path);
            return Ok(Some(png));
        }
        Ok(Some(path))
    }

    async fn send_text(
        &mut self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i32>,
    ) -> Result<Msg, TgError> {
        let peer = self.ctx.peer(chat_id)?;
        let input = InputMessage::new().text(text).reply_to(reply_to);
        let sent = self
            .client()?
            .send_message(peer, input)
            .await
            .map_err(|e| format!("send failed: {e}"))?;
        Ok(convert(&self.ctx, &sent, chat_id))
    }

    async fn send_file(
        &mut self,
        chat_id: i64,
        path: &std::path::Path,
        caption: &str,
    ) -> Result<Msg, TgError> {
        let peer = self.ctx.peer(chat_id)?;
        let client = self.client()?.clone();
        let uploaded = client
            .upload_file(path)
            .await
            .map_err(|e| format!("upload failed: {e}"))?;
        let is_image = path.extension().is_some_and(|e| {
            ["png", "jpg", "jpeg", "webp"]
                .iter()
                .any(|ext| e.eq_ignore_ascii_case(ext))
        });
        let input = if is_image {
            InputMessage::new().text(caption).photo(uploaded)
        } else {
            InputMessage::new().text(caption).document(uploaded)
        };
        let sent = client
            .send_message(peer, input)
            .await
            .map_err(|e| format!("send failed: {e}"))?;
        Ok(convert(&self.ctx, &sent, chat_id))
    }

    async fn edit_text(&mut self, chat_id: i64, msg_id: i32, text: &str) -> Result<Msg, TgError> {
        let peer = self.ctx.peer(chat_id)?;
        let client = self.client()?.clone();
        client
            .edit_message(peer, msg_id, InputMessage::new().text(text))
            .await
            .map_err(|e| format!("edit failed: {e}"))?;
        // Re-fetch so the returned Msg carries the server's view (edited flag).
        let fresh = client
            .get_messages_by_id(peer, &[msg_id])
            .await
            .map_err(|e| e.to_string())?;
        match fresh.into_iter().flatten().next() {
            Some(m) => Ok(convert(&self.ctx, &m, chat_id)),
            None => Err("edited message vanished".into()),
        }
    }

    async fn delete_message(&mut self, chat_id: i64, msg_id: i32) -> Result<(), TgError> {
        let peer = self.ctx.peer(chat_id)?;
        self.client()?
            .delete_messages(peer, &[msg_id])
            .await
            .map_err(|e| format!("delete failed: {e}"))?;
        Ok(())
    }

    async fn mark_read(&mut self, chat_id: i64) -> Result<(), TgError> {
        let peer = self.ctx.peer(chat_id)?;
        let client = self.client()?.clone();
        let mut iter = client.iter_messages(peer).limit(1);
        if let Some(latest) = iter.next().await.map_err(|e| e.to_string())? {
            latest.mark_as_read().await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn media_placeholder(media: Option<&Media>) -> &'static str {
    match media {
        Some(Media::Photo(_)) => "[photo]",
        Some(Media::Sticker(_)) => "[sticker]",
        Some(Media::Document(d)) if is_voice(d) => "[voice message]",
        Some(_) => "[file]",
        None => "",
    }
}

fn is_voice(d: &Document) -> bool {
    d.name().is_none_or(|n| n.is_empty())
        && d.mime_type().is_some_and(|m| m.starts_with("audio/"))
}

fn register_media(ctx: &Ctx, m: &Message, chat_id: i64) {
    if let Some(media) = m.media() {
        ctx.media.lock().unwrap().insert((chat_id, m.id()), media);
    }
}

fn convert(ctx: &Ctx, m: &Message, chat_id: i64) -> Msg {
    let (media_kind, doc_name) = match m.media() {
        Some(Media::Photo(_)) => (Some(MediaKind::Photo), None),
        Some(Media::Sticker(_)) => (Some(MediaKind::Sticker), None),
        Some(Media::Document(ref d)) if is_voice(d) => (Some(MediaKind::Voice), None),
        Some(Media::Document(ref d)) => (
            Some(MediaKind::Document),
            Some(d.name().filter(|n| !n.is_empty()).unwrap_or("file").to_string()),
        ),
        _ => (None, None),
    };
    register_media(ctx, m, chat_id);

    let reactions = match &m.raw {
        tl::enums::Message::Message(raw) => raw
            .reactions
            .as_ref()
            .map(|r| {
                let tl::enums::MessageReactions::Reactions(r) = r;
                r.results
                    .iter()
                    .map(|rc| {
                        let tl::enums::ReactionCount::Count(rc) = rc;
                        let emoji = match &rc.reaction {
                            tl::enums::Reaction::Emoji(e) => e.emoticon.clone(),
                            _ => "★".to_string(),
                        };
                        Reaction { emoji, count: rc.count }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => vec![],
    };

    let sender = if m.outgoing() {
        "You".to_string()
    } else {
        m.sender()
            .and_then(|p| p.name())
            .unwrap_or_default()
            .to_string()
    };

    Msg {
        id: m.id(),
        chat_id,
        sender,
        text: m.text().to_string(),
        ts: m.date().with_timezone(&Local),
        outgoing: m.outgoing(),
        media: media_kind,
        doc_name,
        reply_to: m.reply_to_message_id(),
        reactions,
        edited: m.edit_date().is_some(),
    }
}

async fn update_loop(
    client: Client,
    updates_rx: mpsc::UnboundedReceiver<UpdatesLike>,
    ctx: Arc<Ctx>,
    events: async_channel::Sender<Event>,
) {
    let mut stream = match client
        .stream_updates(updates_rx, UpdatesConfiguration::default())
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("omarchygram: could not start update stream: {e}");
            return;
        }
    };
    loop {
        match stream.next().await {
            Ok(Update::NewMessage(m)) if !m.outgoing() => {
                let chat_id = m.peer_id().bot_api_dialog_id_unchecked();
                remember_from_message(&ctx, &m, chat_id).await;
                let msg = convert(&ctx, &m, chat_id);
                if events.send(Event::NewMessage(msg)).await.is_err() {
                    return;
                }
            }
            Ok(Update::MessageEdited(m)) => {
                let chat_id = m.peer_id().bot_api_dialog_id_unchecked();
                remember_from_message(&ctx, &m, chat_id).await;
                let msg = convert(&ctx, &m, chat_id);
                if events.send(Event::MessageChanged(msg)).await.is_err() {
                    return;
                }
            }
            Ok(Update::Raw(raw)) => {
                if let Some((chat_id, name)) = typing_from_raw(&ctx, &raw.raw) {
                    if events.send(Event::Typing { chat_id, name }).await.is_err() {
                        return;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("omarchygram: update loop error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
}

async fn remember_from_message(ctx: &Ctx, m: &Message, chat_id: i64) {
    if ctx.peers.lock().unwrap().contains_key(&chat_id) {
        return;
    }
    if let Ok(Some(peer_ref)) = m.peer_ref().await {
        let title = m.peer().and_then(|p| p.name()).map(str::to_string);
        ctx.remember(chat_id, peer_ref, title.as_deref());
    }
}

fn typing_from_raw(ctx: &Ctx, raw: &tl::enums::Update) -> Option<(i64, String)> {
    use tl::enums::SendMessageAction::SendMessageTypingAction as Typing;
    match raw {
        tl::enums::Update::UserTyping(u) if matches!(u.action, Typing) => {
            let chat_id = PeerId::user(u.user_id)?.bot_api_dialog_id_unchecked();
            let name = ctx.titles.lock().unwrap().get(&chat_id).cloned().unwrap_or_default();
            Some((chat_id, name))
        }
        tl::enums::Update::ChatUserTyping(u) if matches!(u.action, Typing) => {
            let chat_id = PeerId::chat(u.chat_id)?.bot_api_dialog_id_unchecked();
            Some((chat_id, String::new()))
        }
        tl::enums::Update::ChannelUserTyping(u) if matches!(u.action, Typing) => {
            let chat_id = PeerId::channel(u.channel_id)?.bot_api_dialog_id_unchecked();
            Some((chat_id, String::new()))
        }
        _ => None,
    }
}
