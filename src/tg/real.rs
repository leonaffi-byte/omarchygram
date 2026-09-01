//! grammers-backed Telegram backend. Orchestrator-owned: do not modify in delegations.
//!
//! Runs inside the backend tokio runtime (see tg/mod.rs). The UI never sees
//! grammers types; everything is converted at this boundary.
//!
//! Auth commands are handled serially by `Backend`; data commands (history,
//! sends, downloads, …) are spawned so a slow download or upload never blocks
//! a chat switch or send. Shared state lives in `Ctx` (std::sync::Mutex,
//! never held across an await).
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

use super::archive::Archive;
use super::{
    paths, AuthState, BackendFlags, ChatSummary, Command, Event, MediaKind, Msg, Reaction,
    TgError,
};

/// Shared between the command loop, spawned data tasks, and the update loop.
struct Ctx {
    peers: Mutex<HashMap<i64, PeerRef>>,
    titles: Mutex<HashMap<i64, String>>,
    media: Mutex<HashMap<(i64, i32), Media>>,
    /// Local archive (always on; anti-delete/edit-history read from it).
    archive: Option<Archive>,
    flags: Mutex<BackendFlags>,
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
        if let Err(e) = std::fs::set_permissions(path, perm) {
            eprintln!("omarchygram: could not restrict {}: {e}", path.display());
        }
    }
}

/// The sqlite session plus its -journal/-wal/-shm sidecars all hold auth-key
/// material; restrict every one that exists (the 0700 parent dir is the
/// primary barrier, this is defense in depth).
fn chmod_session_files(session_path: &std::path::Path) {
    chmod_600(session_path);
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut os = session_path.as_os_str().to_owned();
        os.push(suffix);
        let sidecar = PathBuf::from(os);
        if sidecar.exists() {
            chmod_600(&sidecar);
        }
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
            archive: match Archive::open().await {
                Ok(a) => Some(a),
                Err(e) => {
                    eprintln!("omarchygram: archive disabled: {e}");
                    None
                }
            },
            flags: Mutex::new(BackendFlags::default()),
        }),
        events,
        update_loop_started: false,
    };

    while let Some(cmd) = cmds.recv().await {
        // Auth commands mutate Backend and run serially; everything else is
        // spawned with clones of (Client, Arc<Ctx>).
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
            Command::SetFlags(flags, tx) => {
                // Flags take effect HERE, in the serial loop, before any later
                // MarkRead is even spawned — ghost mode can never race a
                // receipt. The status RPC (offline on/off) runs in the background.
                let previous = std::mem::replace(&mut *be.ctx.flags.lock().unwrap(), flags);
                match be.client.clone() {
                    Some(client) => {
                        let ctx = be.ctx.clone();
                        tokio::spawn(async move {
                            let _ = tx.send(apply_ghost_status(&client, &ctx, previous.ghost_mode, flags.ghost_mode).await);
                        });
                    }
                    None => drop(tx.send(Ok(()))),
                }
            }
            data_cmd => {
                let Some(client) = be.client.clone() else {
                    respond_not_connected(data_cmd);
                    continue;
                };
                let ctx = be.ctx.clone();
                tokio::spawn(handle_data(client, ctx, data_cmd));
            }
        }
    }
}

/// Answer a data command received before the backend connected.
fn respond_not_connected(cmd: Command) {
    const E: &str = "not connected";
    match cmd {
        Command::GetDialogs(tx) => drop(tx.send(Err(E.into()))),
        Command::GetHistory { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::DownloadMedia { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::SendText { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::SendFile { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::EditText { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::DeleteMessage { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::MarkRead { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::SetFlags(_, tx) => drop(tx.send(Err(E.into()))),
        Command::GetHistoryAtDate { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::GetEditHistory { respond, .. } => drop(respond.send(Err(E.into()))),
        Command::Start(_) | Command::SubmitPhone(..) | Command::SubmitCode(..)
        | Command::SubmitPassword(..) => unreachable!("auth commands handled serially"),
    }
}

async fn handle_data(client: Client, ctx: Arc<Ctx>, cmd: Command) {
    match cmd {
        Command::GetDialogs(tx) => {
            let _ = tx.send(get_dialogs(&client, &ctx).await);
        }
        Command::GetHistory { chat_id, before_id, respond } => {
            let _ = respond.send(get_history(&client, &ctx, chat_id, before_id).await);
        }
        Command::DownloadMedia { chat_id, msg_id, respond } => {
            let _ = respond.send(download_media(&client, &ctx, chat_id, msg_id).await);
        }
        Command::SendText { chat_id, text, reply_to, respond } => {
            let _ = respond.send(send_text(&client, &ctx, chat_id, &text, reply_to).await);
        }
        Command::SendFile { chat_id, path, caption, respond } => {
            let _ = respond.send(send_file(&client, &ctx, chat_id, &path, &caption).await);
        }
        Command::EditText { chat_id, msg_id, text, respond } => {
            let _ = respond.send(edit_text(&client, &ctx, chat_id, msg_id, &text).await);
        }
        Command::DeleteMessage { chat_id, msg_id, respond } => {
            let _ = respond.send(delete_message(&client, &ctx, chat_id, msg_id).await);
        }
        Command::MarkRead { chat_id, up_to, respond } => {
            let _ = respond.send(mark_read(&client, &ctx, chat_id, up_to).await);
        }
        Command::SetFlags(..) => unreachable!("flags are applied in the serial loop"),
        Command::GetHistoryAtDate { chat_id, date, respond } => {
            let _ = respond.send(get_history_at_date(&client, &ctx, chat_id, date).await);
        }
        Command::GetEditHistory { chat_id, msg_id, respond } => {
            let versions = match &ctx.archive {
                Some(a) => a.versions(chat_id, msg_id).await,
                None => vec![],
            };
            let _ = respond.send(Ok(versions));
        }
        Command::Start(_) | Command::SubmitPhone(..) | Command::SubmitCode(..)
        | Command::SubmitPassword(..) => unreachable!("auth commands handled serially"),
    }
}

impl Backend {
    fn client(&self) -> Result<&Client, TgError> {
        self.client.as_ref().ok_or_else(|| "not connected".to_string())
    }

    /// Starts the live-update stream. Must succeed BEFORE Ready is reported —
    /// a silently dead update path is worse than a visible startup error.
    async fn on_authorized(&mut self) -> Result<(), TgError> {
        if self.update_loop_started {
            return Ok(());
        }
        let Some(updates_rx) = self.updates_rx.take() else {
            // The receiver was consumed by a failed earlier attempt; there is
            // no way to rebuild it — never report Ready with dead updates.
            return Err(
                "live updates are unavailable after an earlier failure — restart Omarchygram"
                    .to_string(),
            );
        };
        let client = self.client.as_ref().unwrap().clone();
        let stream = client
            .stream_updates(updates_rx, UpdatesConfiguration::default())
            .await
            .map_err(|e| format!("could not start live updates: {e}"))?;
        self.update_loop_started = true;
        let ctx = self.ctx.clone();
        let events = self.events.clone();
        tokio::spawn(consume_updates(stream, client, ctx, events));
        Ok(())
    }

    async fn start(&mut self) -> Result<AuthState, TgError> {
        // Idempotent: a UI retry must never open a second session/sender pool
        // over the same auth key.
        if let Some(client) = self.client.clone() {
            let authorized = client.is_authorized().await.map_err(|e| e.to_string())?;
            return if authorized {
                self.on_authorized().await?;
                Ok(AuthState::Ready)
            } else {
                Ok(AuthState::NeedPhone)
            };
        }
        let Some((api_id, api_hash)) = load_credentials() else {
            return Ok(AuthState::NeedCredentials);
        };
        self.api_hash = Some(api_hash);
        if let Some(dir) = self.session_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            // The 0700 dir is the primary barrier around the auth key —
            // failing to establish it is a startup error, not a log line.
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("could not restrict session dir: {e}"))?;
        }
        let session = Arc::new(
            SqliteSession::open(&self.session_path)
                .await
                .map_err(|e| format!("could not open session: {e}"))?,
        );
        chmod_session_files(&self.session_path);
        let SenderPool { runner, updates, handle } = SenderPool::new(session, api_id);
        tokio::spawn(runner.run());
        let client = Client::new(handle);
        // Store BEFORE the fallible RPC below: a network failure here must not
        // let a Retry open a second sender pool over the same session.
        self.client = Some(client.clone());
        self.updates_rx = Some(updates);
        let authorized = client.is_authorized().await.map_err(|e| e.to_string())?;
        if authorized {
            self.on_authorized().await?;
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
                self.on_authorized().await.map_err(|e| {
                    format!("signed in, but {e} — restart Omarchygram to continue")
                })?;
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
                self.on_authorized().await.map_err(|e| {
                    format!("signed in, but {e} — restart Omarchygram to continue")
                })?;
                Ok(AuthState::Ready)
            }
            Err(SignInError::InvalidPassword(fresh_token)) => {
                // The server hands back a fresh token so the user can retry.
                self.password_token = Some(fresh_token);
                Err("Wrong password — try again.".into())
            }
            Err(e) => {
                // The password token was consumed and only InvalidPassword
                // returns a fresh one; the code flow must be redone.
                self.login_token = None;
                Err(format!(
                    "password check failed: {e} — restart Omarchygram and log in again"
                ))
            }
        }
    }
}

async fn get_dialogs(client: &Client, ctx: &Arc<Ctx>) -> Result<Vec<ChatSummary>, TgError> {
    let mut iter = client.iter_dialogs().limit(50);
    let mut out = Vec::new();
    while let Some(dialog) = iter.next().await.map_err(|e| e.to_string())? {
        let chat_id = dialog.peer_id().bot_api_dialog_id_unchecked();
        let title = dialog.peer().name().unwrap_or("Unknown").to_string();
        ctx.remember(chat_id, dialog.peer_ref(), Some(&title));
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
            // convert() registers media AND records the message in the archive,
            // so a message deleted before its chat is ever opened is recoverable.
            let _ = convert(ctx, m, chat_id);
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
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    before_id: Option<i32>,
) -> Result<Vec<Msg>, TgError> {
    let peer = ctx.peer(chat_id)?;
    let mut iter = client.iter_messages(peer).limit(50);
    if let Some(before) = before_id {
        iter = iter.offset_id(before);
    }
    let mut out = Vec::new();
    while let Some(m) = iter.next().await.map_err(|e| e.to_string())? {
        out.push(convert(ctx, &m, chat_id));
        if out.len() >= 50 {
            break;
        }
    }
    out.reverse(); // newest last (display order)
    merge_deleted(ctx, chat_id, before_id, &mut out).await;
    Ok(out)
}

/// Anti-delete: splice archived deleted messages into a history page. The
/// page covers ids [oldest returned, before_id) — or up to the newest when
/// this is the first page — so deleted messages in that window reappear.
async fn merge_deleted(ctx: &Ctx, chat_id: i64, before_id: Option<i32>, page: &mut Vec<Msg>) {
    if !ctx.flags.lock().unwrap().anti_delete {
        return;
    }
    let Some(archive) = &ctx.archive else { return };
    let min_id = page.first().map(|m| m.id).unwrap_or(1);
    let max_id = before_id.map(|b| b - 1).unwrap_or(i32::MAX);
    if max_id < min_id {
        return;
    }
    let mut deleted = archive.deleted_between(chat_id, min_id, max_id).await;
    if deleted.is_empty() {
        return;
    }
    // Keep the newest 200 on an unbounded first page; older ones come with paging.
    if deleted.len() > 200 {
        let cut = deleted.len() - 200;
        deleted.drain(..cut);
    }
    let title = ctx.titles.lock().unwrap().get(&chat_id).cloned().unwrap_or_default();
    for mut d in deleted {
        if page.iter().any(|m| m.id == d.id) {
            continue;
        }
        d.chat_title = title.clone();
        page.push(d);
    }
    page.sort_by_key(|m| m.id);
}

async fn get_history_at_date(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    date: chrono::DateTime<Local>,
) -> Result<Vec<Msg>, TgError> {
    let peer = ctx.peer(chat_id)?;
    // offset_date = messages strictly older than this unix time, newest first.
    let mut iter = client
        .iter_messages(peer)
        .offset_date(date.timestamp() as i32 + 1)
        .limit(50);
    let mut out = Vec::new();
    while let Some(m) = iter.next().await.map_err(|e| e.to_string())? {
        out.push(convert(ctx, &m, chat_id));
        if out.len() >= 50 {
            break;
        }
    }
    out.reverse();
    Ok(out)
}

/// Ghost on: tell Telegram we're offline now and keep re-asserting it every
/// minute (sending a message or fetching can flip us online). Ghost off:
/// send `offline: false` once. Read receipts are already suppressed by the
/// flag itself; this only covers presence.
async fn apply_ghost_status(client: &Client, ctx: &Arc<Ctx>, was: bool, now: bool) -> Result<(), TgError> {
    if now == was {
        return Ok(());
    }
    client
        .invoke(&tl::functions::account::UpdateStatus { offline: now })
        .await
        .map_err(|e| format!("could not update online status: {e}"))?;
    if now {
        let client = client.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if !ctx.flags.lock().unwrap().ghost_mode {
                    return;
                }
                let _ = client.invoke(&tl::functions::account::UpdateStatus { offline: true }).await;
            }
        });
    }
    Ok(())
}

async fn download_media(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    msg_id: i32,
) -> Result<Option<PathBuf>, TgError> {
    let media = { ctx.media.lock().unwrap().get(&(chat_id, msg_id)).cloned() };
    let Some(media) = media else {
        return Ok(None);
    };
    let dir = paths::media_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
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
        // The extension comes from a REMOTE sender's filename and decides
        // which handler later opens the file — allow only plain ascii.
        Media::Document(d) => d
            .name()
            .and_then(|n| std::path::Path::new(n).extension())
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .filter(|e| {
                (1..=8).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric())
            })
            .unwrap_or_else(|| "bin".to_string()),
        _ => "bin".to_string(),
    };
    let path = dir.join(format!("{stem}.{ext}"));
    client
        .download_media(&media, &path)
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    chmod_600(&path);

    // GdkPixbuf on this system has no webp loader; convert stickers to png.
    // (Animated .tgs stickers fail to decode; the UI shows them as unavailable.)
    if ext == "webp" {
        let png = dir.join(format!("{stem}.png"));
        let mut reader = image::ImageReader::open(&path)
            .map_err(|e| e.to_string())?
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let img = reader.decode().map_err(|e| format!("sticker decode failed: {e}"))?;
        img.save(&png).map_err(|e| e.to_string())?;
        chmod_600(&png);
        let _ = std::fs::remove_file(&path);
        return Ok(Some(png));
    }
    Ok(Some(path))
}

async fn send_text(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    text: &str,
    reply_to: Option<i32>,
) -> Result<Msg, TgError> {
    let peer = ctx.peer(chat_id)?;
    let input = InputMessage::new().text(text).reply_to(reply_to);
    let sent = client
        .send_message(peer, input)
        .await
        .map_err(|e| format!("send failed: {e}"))?;
    Ok(convert(ctx, &sent, chat_id))
}

async fn send_file(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    path: &std::path::Path,
    caption: &str,
) -> Result<Msg, TgError> {
    let peer = ctx.peer(chat_id)?;
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
    Ok(convert(ctx, &sent, chat_id))
}

async fn edit_text(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    msg_id: i32,
    text: &str,
) -> Result<Msg, TgError> {
    let peer = ctx.peer(chat_id)?;
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
        Some(m) => Ok(convert(ctx, &m, chat_id)),
        None => Err("edited message vanished".into()),
    }
}

async fn delete_message(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    msg_id: i32,
) -> Result<(), TgError> {
    let peer = ctx.peer(chat_id)?;
    client
        .delete_messages(peer, &[msg_id])
        .await
        .map_err(|e| format!("delete failed: {e}"))?;
    Ok(())
}

async fn mark_read(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    up_to: i32,
) -> Result<(), TgError> {
    if ctx.flags.lock().unwrap().ghost_mode {
        return Ok(()); // ghost mode: never send read receipts
    }
    let peer = ctx.peer(chat_id)?;
    let input_peer: tl::enums::InputPeer = peer.into();
    // ReadHistory with max_id = the id the UI actually displayed, so a message
    // racing this request is never marked read unseen.
    match input_peer {
        tl::enums::InputPeer::Channel(c) => {
            client
                .invoke(&tl::functions::channels::ReadHistory {
                    channel: tl::enums::InputChannel::Channel(tl::types::InputChannel {
                        channel_id: c.channel_id,
                        access_hash: c.access_hash,
                    }),
                    max_id: up_to,
                })
                .await
                .map_err(|e| e.to_string())?;
        }
        peer => {
            client
                .invoke(&tl::functions::messages::ReadHistory { peer, max_id: up_to })
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
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
    // Telegram's own flag, not a filename/mime guess.
    if let Some(tl::enums::Document::Document(doc)) = d.raw.document.as_ref() {
        for attr in &doc.attributes {
            if let tl::enums::DocumentAttribute::Audio(a) = attr {
                return a.voice;
            }
        }
    }
    false
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
                    .filter_map(|rc| {
                        let tl::enums::ReactionCount::Count(rc) = rc;
                        let emoji = match &rc.reaction {
                            tl::enums::Reaction::Emoji(e) => e.emoticon.clone(),
                            tl::enums::Reaction::CustomEmoji(_) => "✦".to_string(),
                            tl::enums::Reaction::Paid => "⭐".to_string(),
                            tl::enums::Reaction::Empty => return None,
                        };
                        Some(Reaction { emoji, count: rc.count })
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

    let chat_title = {
        let known = ctx.titles.lock().unwrap().get(&chat_id).cloned();
        known
            .or_else(|| m.peer().and_then(|p| p.name()).map(str::to_string))
            .unwrap_or_default()
    };

    let msg = Msg {
        id: m.id(),
        chat_id,
        chat_title,
        sender,
        sender_id: m
            .sender_id()
            .filter(|p| p.kind() == grammers_client::session::types::PeerKind::User)
            .and_then(|p| p.bot_api_dialog_id()),
        text: m.text().to_string(),
        ts: m.date().with_timezone(&Local),
        outgoing: m.outgoing(),
        media: media_kind,
        doc_name,
        reply_to: m.reply_to_message_id(),
        reactions,
        edited: m.edit_date().is_some() && !m.edit_hide(),
        deleted: false,
    };
    if let Some(archive) = &ctx.archive {
        archive.record(msg.clone());
    }
    msg
}

async fn consume_updates(
    mut stream: grammers_client::client::UpdateStream,
    client: Client,
    ctx: Arc<Ctx>,
    events: async_channel::Sender<Event>,
) {
    loop {
        match stream.next().await {
            // Outgoing messages are forwarded too: they are how sends from the
            // user's OTHER devices appear. The UI dedupes local sends by id.
            Ok(Update::NewMessage(m)) => {
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
            Ok(Update::MessageDeleted(d)) => {
                let channel_chat = d
                    .channel_id()
                    .and_then(PeerId::channel)
                    .map(|p| p.bot_api_dialog_id_unchecked());
                let flagged: Vec<(i64, i32)> = match &ctx.archive {
                    Some(a) => a.mark_deleted(channel_chat, d.messages().to_vec()).await,
                    // Without an archive only channel deletions are attributable.
                    None => match channel_chat {
                        Some(c) => d.messages().iter().map(|&id| (c, id)).collect(),
                        None => {
                            eprintln!("omarchygram: archive disabled — cannot attribute a deletion of {} message(s)", d.messages().len());
                            vec![]
                        }
                    },
                };
                let mut by_chat: HashMap<i64, Vec<i32>> = HashMap::new();
                for (c, id) in flagged {
                    by_chat.entry(c).or_default().push(id);
                }
                for (chat_id, msg_ids) in by_chat {
                    if events.send(Event::MessageDeleted { chat_id, msg_ids }).await.is_err() {
                        return;
                    }
                }
            }
            Ok(Update::Raw(raw)) => {
                if let Some((chat_id, name)) = typing_from_raw(&ctx, &raw.raw) {
                    if events.send(Event::Typing { chat_id, name }).await.is_err() {
                        return;
                    }
                }
                if let tl::enums::Update::MessageReactions(u) = &raw.raw {
                    if let Some(msg) = refetch_for_reactions(&client, &ctx, u).await {
                        if events.send(Event::MessageChanged(msg)).await.is_err() {
                            return;
                        }
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

/// Reaction changes arrive only as raw updates; refetch the message so the UI
/// gets a normal MessageChanged. Best-effort — None on any failure.
async fn refetch_for_reactions(
    client: &Client,
    ctx: &Arc<Ctx>,
    u: &tl::types::UpdateMessageReactions,
) -> Option<Msg> {
    let peer_id = match &u.peer {
        tl::enums::Peer::User(p) => PeerId::user(p.user_id)?,
        tl::enums::Peer::Chat(p) => PeerId::chat(p.chat_id)?,
        tl::enums::Peer::Channel(p) => PeerId::channel(p.channel_id)?,
    };
    let chat_id = peer_id.bot_api_dialog_id_unchecked();
    let peer = ctx.peer(chat_id).ok()?;
    let fetched = client.get_messages_by_id(peer, &[u.msg_id]).await.ok()?;
    let m = fetched.into_iter().flatten().next()?;
    Some(convert(ctx, &m, chat_id))
}

async fn remember_from_message(ctx: &Ctx, m: &Message, chat_id: i64) {
    // Always refresh: a message can carry a newer access hash or a renamed title.
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
