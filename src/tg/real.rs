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

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local, TimeZone};
use grammers_client::client::UpdatesConfiguration;
use grammers_client::media::{ChatPhoto, Document, Media};
use grammers_client::peer::Role;
use grammers_client::peer::Peer;
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
    parse_markdown, paths, reject, to_markdown, AuthState, BackendFlags, BotCommand, ButtonKind, ChatInfo, ChatKind,
    ChatSummary, Command, Contact, ContactCard, DiceInfo, Event, Folder, GeoPoint, Gif, KeyButton, Keyboard,
    LiveLocation, LocationInfo, Me, MediaKind, Member, MemberRole, Msg, MuteMode, Poll, PollOption, Presence, Reaction,
    SharedKind, Span, SpanKind, Sticker, StickerPack, StoryRing, TgError, WebPreview,
};

/// Shared between the command loop, spawned data tasks, and the update loop.
struct Ctx {
    peers: Mutex<HashMap<i64, PeerRef>>,
    titles: Mutex<HashMap<i64, String>>,
    media: Mutex<HashMap<(i64, i32), Media>>,
    /// Local archive (always on; anti-delete/edit-history read from it).
    archive: Option<Archive>,
    flags: Mutex<BackendFlags>,
    /// chat/user id -> profile photo id (for download_avatar).
    photos: Mutex<HashMap<i64, i64>>,
    /// Raw documents seen in sticker/gif lists, by document id.
    documents: Mutex<HashMap<i64, tl::types::Document>>,
    /// Sticker set id -> access hash.
    sticker_sets: Mutex<HashMap<i64, i64>>,
    /// Per-dialog facts from the last get_dialogs (folder membership).
    meta: Mutex<HashMap<i64, DialogMeta>>,
    /// Forum supergroups (chat ids) seen in dialogs — their messages get `topic_id`.
    forums: Mutex<HashSet<i64>>,
    /// Story rings from the last stories fetch (wave 6F).
    story_rings: Mutex<HashMap<i64, StoryRing>>,
}

#[derive(Clone, Copy)]
struct DialogMeta {
    kind: ChatKind,
    contact: bool,
    muted: bool,
    unread: bool,
    archived: bool,
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
    crate::config::credentials()
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
            photos: Mutex::new(HashMap::new()),
            documents: Mutex::new(HashMap::new()),
            sticker_sets: Mutex::new(HashMap::new()),
            forums: Mutex::new(HashSet::new()),
            story_rings: Mutex::new(HashMap::new()),
            meta: Mutex::new(HashMap::new()),
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
            Command::SubmitCredentials { api_id, api_hash, respond } => {
                let r = match crate::config::set_credentials(api_id, &api_hash) {
                    Ok(()) => be.start().await,
                    Err(e) => Err(e),
                };
                let _ = respond.send(r);
            }
            Command::LogOut(tx) => {
                let _ = tx.send(be.log_out().await);
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
    reject(cmd, "not connected");
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
        Command::DeleteMessages { chat_id, ids, respond } => {
            let _ = respond.send(delete_messages(&client, &ctx, chat_id, &ids).await);
        }
        Command::MarkRead { chat_id, up_to, respond } => {
            let _ = respond.send(mark_read(&client, &ctx, chat_id, up_to).await);
        }
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
        Command::GetMe(tx) => {
            let _ = tx.send(get_me(&client).await);
        }
        Command::GetMessages { chat_id, ids, respond } => {
            let _ = respond.send(get_messages(&client, &ctx, chat_id, &ids).await);
        }
        Command::DownloadAvatar { chat_id, respond } => {
            let _ = respond.send(download_avatar(&client, &ctx, chat_id).await);
        }
        Command::SendVoice { chat_id, path, duration, respond } => {
            let _ = respond.send(send_voice(&client, &ctx, chat_id, &path, duration).await);
        }
        Command::SendSticker { chat_id, sticker_id, respond } => {
            let _ = respond.send(send_document_by_id(&client, &ctx, chat_id, sticker_id).await);
        }
        Command::SendGif { chat_id, gif_id, respond } => {
            let _ = respond.send(send_document_by_id(&client, &ctx, chat_id, gif_id).await);
        }
        Command::ForwardMessages { from_chat, ids, to_chat, respond } => {
            let _ = respond.send(forward_messages(&client, &ctx, from_chat, &ids, to_chat).await);
        }
        Command::SearchMessages { chat_id, query, before_id, respond } => {
            let _ = respond.send(search_messages(&client, &ctx, chat_id, &query, before_id, None).await);
        }
        Command::SearchGlobal { query, respond } => {
            let _ = respond.send(search_global(&client, &ctx, &query).await);
        }
        Command::SearchChats { query, respond } => {
            let _ = respond.send(search_chats(&client, &ctx, &query).await);
        }
        Command::GetPinnedMessage { chat_id, respond } => {
            let _ = respond.send(get_pinned_message(&client, &ctx, chat_id).await);
        }
        Command::PinMessage { chat_id, msg_id, pinned, respond } => {
            let _ = respond.send(pin_message(&client, &ctx, chat_id, msg_id, pinned).await);
        }
        Command::SendReaction { chat_id, msg_id, emoji, respond } => {
            let _ = respond.send(send_reaction(&client, &ctx, chat_id, msg_id, emoji).await);
        }
        Command::GetAvailableReactions(tx) => {
            let _ = tx.send(available_reactions(&client).await);
        }
        Command::SetPinned { chat_id, pinned, respond } => {
            let _ = respond.send(set_pinned(&client, &ctx, chat_id, pinned).await);
        }
        Command::SetMuted { chat_id, mode, respond } => {
            let _ = respond.send(set_muted(&client, &ctx, chat_id, mode).await);
        }
        Command::SetArchived { chat_id, archived, respond } => {
            let _ = respond.send(set_archived(&client, &ctx, chat_id, archived).await);
        }
        Command::MarkUnread { chat_id, unread, respond } => {
            let _ = respond.send(mark_unread(&client, &ctx, chat_id, unread).await);
        }
        Command::DeleteChat { chat_id, respond } => {
            let _ = respond.send(delete_chat(&client, &ctx, chat_id).await);
        }
        Command::ClearHistory { chat_id, respond } => {
            let _ = respond.send(clear_history(&client, &ctx, chat_id).await);
        }
        Command::SaveDraft { chat_id, text, reply_to: _, respond } => {
            let _ = respond.send(save_draft(&client, &ctx, chat_id, &text).await);
        }
        Command::GetChatInfo { chat_id, respond } => {
            let _ = respond.send(get_chat_info(&client, &ctx, chat_id).await);
        }
        Command::GetMembers { chat_id, offset, limit, respond } => {
            let _ = respond.send(get_members(&client, &ctx, chat_id, offset, limit).await);
        }
        Command::GetSharedMedia { chat_id, kind, before_id, respond } => {
            let filter = match kind {
                SharedKind::Photos => tl::enums::MessagesFilter::InputMessagesFilterPhotos,
                SharedKind::Files => tl::enums::MessagesFilter::InputMessagesFilterDocument,
                SharedKind::Links => tl::enums::MessagesFilter::InputMessagesFilterUrl,
                SharedKind::Voice => tl::enums::MessagesFilter::InputMessagesFilterVoice,
                SharedKind::Music => tl::enums::MessagesFilter::InputMessagesFilterMusic,
            };
            let _ = respond.send(search_messages(&client, &ctx, chat_id, "", before_id, Some(filter)).await);
        }
        Command::GetContacts(tx) => {
            let _ = tx.send(get_contacts(&client, &ctx).await);
        }
        Command::OpenUser { user_id, respond } => {
            let _ = respond.send(open_user(&client, &ctx, user_id).await);
        }
        Command::CreateGroup { title, user_ids, respond } => {
            let _ = respond.send(create_group(&client, &ctx, &title, &user_ids).await);
        }
        Command::GetFolders(tx) => {
            let _ = tx.send(get_folders(&client, &ctx).await);
        }
        Command::GetStickerPacks(tx) => {
            let _ = tx.send(sticker_packs(&client, &ctx).await);
        }
        Command::GetStickers { pack_id, respond } => {
            let _ = respond.send(stickers_of(&client, &ctx, &pack_id).await);
        }
        Command::DownloadSticker { sticker_id, respond } => {
            let _ = respond.send(download_document_by_id(&client, &ctx, sticker_id, true).await);
        }
        Command::GetSavedGifs(tx) => {
            let _ = tx.send(saved_gifs(&client, &ctx).await);
        }
        Command::DownloadGif { gif_id, respond } => {
            let _ = respond.send(download_document_by_id(&client, &ctx, gif_id, false).await);
        }
        // Wave 6 commands land in the real implementation commit; until then
        // they fail cleanly instead of being mistaken for auth commands.
        cmd @ (Command::DownloadMap { .. }
        | Command::SendVote { .. }
        | Command::AddContact { .. }
        | Command::SendPoll { .. }
        | Command::SendLocation { .. }
        | Command::SendTextAt { .. }
        | Command::SendFileAt { .. }
        | Command::GetScheduled { .. }
        | Command::SendScheduledNow { .. }
        | Command::DeleteScheduled { .. }
        | Command::PressButton { .. }
        | Command::GetTopics { .. }
        | Command::CreateTopic { .. }
        | Command::SendVideoNote { .. }
        | Command::SendLiveLocation { .. }
        | Command::UpdateLiveLocation { .. }
        | Command::StopLiveLocation { .. }
        | Command::GetStoryPeers(_)
        | Command::GetStories { .. }
        | Command::DownloadStory { .. }
        | Command::MarkStoriesSeen { .. }) => reject(cmd, "not available yet"),
        other => reject(other, "auth commands are handled serially"),
    }
}

async fn get_me(client: &Client) -> Result<Me, TgError> {
    let me = client.get_me().await.map_err(|e| e.to_string())?;
    Ok(Me {
        id: me.id().bot_api_dialog_id_unchecked(),
        name: me.full_name(),
        username: me.username().unwrap_or_default().to_string(),
        phone: me.phone().unwrap_or_default().to_string(),
        has_photo: me.photo().is_some(),
    })
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

    /// Sign out, drop the client and delete the local session so the next
    /// `start()` begins a fresh login. The old sender pool is left to die
    /// with the process (a rare action; a restart is cheap).
    async fn log_out(&mut self) -> Result<AuthState, TgError> {
        if let Some(client) = self.client.take() {
            if let Err(e) = client.sign_out().await {
                eprintln!("omarchygram: sign out: {e}");
            }
        }
        self.login_token = None;
        self.password_token = None;
        self.updates_rx = None;
        self.update_loop_started = false;
        self.ctx.peers.lock().unwrap().clear();
        self.ctx.titles.lock().unwrap().clear();
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut os = self.session_path.as_os_str().to_owned();
            os.push(suffix);
            let p = PathBuf::from(os);
            if p.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
        Ok(AuthState::NeedPhone)
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
    let mut iter = client.iter_dialogs().limit(200);
    let mut out = Vec::new();
    let now = Local::now().timestamp() as i32;
    while let Some(dialog) = iter.next().await.map_err(|e| e.to_string())? {
        let chat_id = dialog.peer_id().bot_api_dialog_id_unchecked();
        let facts = describe_peer(dialog.peer());
        let title = facts
            .title_override
            .clone()
            .unwrap_or_else(|| dialog.peer().name().unwrap_or("Unknown").to_string());
        ctx.remember(chat_id, dialog.peer_ref(), Some(&title));
        if let Some(pid) = facts.photo_id {
            ctx.photos.lock().unwrap().insert(chat_id, pid);
        }
        let last = dialog.last_message.as_ref();
        // convert() registers media AND records the message in the archive,
        // so a message deleted before its chat is ever opened is recoverable.
        let last_msg = last.map(|m| convert(ctx, m, chat_id));
        let raw = match &dialog.raw {
            tl::enums::Dialog::Dialog(d) => Some(d),
            tl::enums::Dialog::Folder(_) => None,
        };
        let (unread, mentions, unread_mark, pinned, rin, rout, folder, muted, draft) = match raw {
            Some(d) => (
                d.unread_count,
                d.unread_mentions_count,
                d.unread_mark,
                d.pinned,
                d.read_inbox_max_id,
                d.read_outbox_max_id,
                d.folder_id,
                is_muted(&d.notify_settings, now),
                draft_text(d.draft.as_ref()),
            ),
            None => (0, 0, false, false, 0, 0, None, false, String::new()),
        };
        let archived = folder == Some(1);
        ctx.meta.lock().unwrap().insert(
            chat_id,
            DialogMeta { kind: facts.kind, contact: facts.contact, muted, unread: unread > 0, archived },
        );
        let last_sender = match (facts.kind, last) {
            (ChatKind::Group, Some(m)) => {
                if m.outgoing() {
                    "You".to_string()
                } else {
                    m.sender()
                        .and_then(|p| p.name())
                        .unwrap_or("")
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string()
                }
            }
            _ => String::new(),
        };
        out.push(ChatSummary {
            id: chat_id,
            title,
            kind: facts.kind,
            username: facts.username.clone(),
            last_message: last_msg.as_ref().map(preview_of).unwrap_or_default(),
            last_sender,
            last_time: last.map(|m| m.date().with_timezone(&Local)),
            last_msg_id: last.map(|m| m.id()).unwrap_or(0),
            last_outgoing: last.is_some_and(|m| m.outgoing()),
            unread,
            mentions,
            unread_mark,
            read_inbox_max_id: rin,
            read_outbox_max_id: rout,
            pinned,
            muted,
            archived,
            presence: facts.presence,
            has_photo: facts.photo_id.is_some(),
            draft,
            forum: facts.forum,
            story_ring: ctx.story_rings.lock().unwrap().get(&chat_id).copied().unwrap_or_default(),
        });
    }
    // The archive folder is not part of iter_dialogs; fetch it raw. A failure
    // there must not hide the main list.
    match get_archived_dialogs(client, ctx, now).await {
        Ok(archived) => out.extend(archived),
        Err(e) => eprintln!("omarchygram: archived dialogs unavailable: {e}"),
    }
    Ok(out)
}

/// Dialogs in Telegram's archive folder (folder_id 1), built from the raw
/// response because grammers' dialog iterator cannot select a folder.
async fn get_archived_dialogs(client: &Client, ctx: &Arc<Ctx>, now: i32) -> Result<Vec<ChatSummary>, TgError> {
    use grammers_client::session::types::PeerAuth;
    use tl::enums::messages::Dialogs as D;
    let r = client
        .invoke(&tl::functions::messages::GetDialogs {
            exclude_pinned: false,
            folder_id: Some(1),
            offset_date: 0,
            offset_id: 0,
            offset_peer: tl::enums::InputPeer::Empty,
            limit: 100,
            hash: 0,
        })
        .await
        .map_err(|e| e.to_string())?;
    let (dialogs, messages, chats, users) = match r {
        D::Dialogs(d) => (d.dialogs, d.messages, d.chats, d.users),
        D::Slice(d) => (d.dialogs, d.messages, d.chats, d.users),
        D::NotModified(_) => return Ok(vec![]),
    };
    let mut out = Vec::new();
    for dialog in dialogs {
        let tl::enums::Dialog::Dialog(d) = dialog else { continue };
        let chat_id = peer_chat_id(&d.peer);
        // Resolve the peer from the response's user/chat lists.
        let (title, kind, username, photo_id, presence, contact, peer_ref, forum) = match &d.peer {
            tl::enums::Peer::User(pu) => {
                let Some(tl::enums::User::User(u)) = users.iter().find(|u| matches!(u, tl::enums::User::User(x) if x.id == pu.user_id)) else { continue };
                let (name, username, _phone, presence, _has_photo, contact) = user_facts(u);
                let kind = if u.is_self { ChatKind::Saved } else if u.bot { ChatKind::Bot } else { ChatKind::User };
                let photo_id = match &u.photo { Some(tl::enums::UserProfilePhoto::Photo(p)) => Some(p.photo_id), _ => None };
                let title = if u.is_self { "Saved Messages".to_string() } else { name };
                let peer_ref = PeerRef { id: PeerId::user_unchecked(u.id), auth: PeerAuth::from_hash(u.access_hash.unwrap_or(0)) };
                (title, kind, username, photo_id, presence, contact, peer_ref, false)
            }
            tl::enums::Peer::Chat(pc) => {
                let Some(tl::enums::Chat::Chat(c)) = chats.iter().find(|c| matches!(c, tl::enums::Chat::Chat(x) if x.id == pc.chat_id)) else { continue };
                let photo_id = match &c.photo { tl::enums::ChatPhoto::Photo(p) => Some(p.photo_id), _ => None };
                (c.title.clone(), ChatKind::Group, String::new(), photo_id, Presence::Unknown, false, PeerId::chat_unchecked(c.id).to_ambient_ref(), false)
            }
            tl::enums::Peer::Channel(pc) => {
                let Some(tl::enums::Chat::Channel(c)) = chats.iter().find(|c| matches!(c, tl::enums::Chat::Channel(x) if x.id == pc.channel_id)) else { continue };
                let photo_id = match &c.photo { tl::enums::ChatPhoto::Photo(p) => Some(p.photo_id), _ => None };
                let kind = if c.megagroup { ChatKind::Group } else { ChatKind::Channel };
                let peer_ref = PeerRef { id: PeerId::channel_unchecked(c.id), auth: PeerAuth::from_hash(c.access_hash.unwrap_or(0)) };
                (c.title.clone(), kind, c.username.clone().unwrap_or_default(), photo_id, Presence::Unknown, false, peer_ref, c.forum)
            }
        };
        if forum {
            ctx.forums.lock().unwrap().insert(chat_id);
        }
        ctx.remember(chat_id, peer_ref, Some(&title));
        if let Some(pid) = photo_id {
            ctx.photos.lock().unwrap().insert(chat_id, pid);
        }
        let muted = is_muted(&d.notify_settings, now);
        ctx.meta.lock().unwrap().insert(
            chat_id,
            DialogMeta { kind, contact, muted, unread: d.unread_count > 0, archived: true },
        );
        // Last message: raw, preview only (the full conversion needs grammers' peer map).
        let last = messages.iter().find_map(|m| match m {
            tl::enums::Message::Message(x) if x.id == d.top_message && peer_chat_id(&x.peer_id) == chat_id => Some(x),
            _ => None,
        });
        let (last_message, last_time, last_msg_id, last_outgoing) = match last {
            Some(m) => {
                let preview = if !m.message.is_empty() {
                    m.message.clone()
                } else {
                    match &m.media {
                        Some(tl::enums::MessageMedia::Photo(_)) => "[photo]".to_string(),
                        Some(tl::enums::MessageMedia::Document(_)) => "[file]".to_string(),
                        Some(_) => "[message]".to_string(),
                        None => String::new(),
                    }
                };
                (
                    preview,
                    Local.timestamp_opt(m.date as i64, 0).single(),
                    m.id,
                    m.out,
                )
            }
            None => (String::new(), None, d.top_message, false),
        };
        out.push(ChatSummary {
            id: chat_id,
            title,
            kind,
            username,
            last_message,
            last_sender: String::new(),
            last_time,
            last_msg_id,
            last_outgoing,
            unread: d.unread_count,
            mentions: d.unread_mentions_count,
            unread_mark: d.unread_mark,
            read_inbox_max_id: d.read_inbox_max_id,
            read_outbox_max_id: d.read_outbox_max_id,
            pinned: d.pinned,
            muted,
            archived: true,
            presence,
            has_photo: photo_id.is_some(),
            draft: draft_text(d.draft.as_ref()),
            forum,
            story_ring: ctx.story_rings.lock().unwrap().get(&chat_id).copied().unwrap_or_default(),
        });
    }
    Ok(out)
}

struct PeerFacts {
    kind: ChatKind,
    presence: Presence,
    photo_id: Option<i64>,
    username: String,
    contact: bool,
    title_override: Option<String>,
    forum: bool,
}

fn describe_peer(peer: &Peer) -> PeerFacts {
    match peer {
        Peer::User(u) => PeerFacts {
            kind: if u.is_self() {
                ChatKind::Saved
            } else if u.is_bot() {
                ChatKind::Bot
            } else {
                ChatKind::User
            },
            presence: presence_from(u.status()),
            photo_id: u.photo().map(|p| p.photo_id),
            username: u.username().unwrap_or("").to_string(),
            contact: u.contact(),
            title_override: if u.is_self() { Some("Saved Messages".to_string()) } else { None },
            forum: false,
        },
        Peer::Group(g) => PeerFacts {
            kind: ChatKind::Group,
            presence: Presence::Unknown,
            photo_id: g.photo().map(|p| p.photo_id),
            username: g.username().unwrap_or("").to_string(),
            contact: false,
            title_override: None,
            forum: false,
        },
        Peer::Channel(c) => PeerFacts {
            kind: if c.raw.megagroup { ChatKind::Group } else { ChatKind::Channel },
            presence: Presence::Unknown,
            photo_id: c.photo().map(|p| p.photo_id),
            username: c.username().unwrap_or("").to_string(),
            contact: false,
            title_override: None,
            forum: c.raw.forum,
        },
    }
}

fn presence_from(status: &tl::enums::UserStatus) -> Presence {
    use tl::enums::UserStatus as S;
    match status {
        S::Empty => Presence::Unknown,
        S::Online(_) => Presence::Online,
        S::Offline(o) => Local
            .timestamp_opt(o.was_online as i64, 0)
            .single()
            .map(Presence::LastSeen)
            .unwrap_or(Presence::LongAgo),
        S::Recently(_) => Presence::Recently,
        S::LastWeek(_) => Presence::LastWeek,
        S::LastMonth(_) => Presence::LastMonth,
    }
}

fn is_muted(settings: &tl::enums::PeerNotifySettings, now: i32) -> bool {
    let tl::enums::PeerNotifySettings::Settings(s) = settings;
    s.mute_until.is_some_and(|t| t > now)
}

fn draft_text(draft: Option<&tl::enums::DraftMessage>) -> String {
    match draft {
        Some(tl::enums::DraftMessage::Message(d)) => d.message.clone(),
        _ => String::new(),
    }
}

/// Bot-API dialog id of a raw peer.
fn peer_chat_id(p: &tl::enums::Peer) -> i64 {
    match p {
        tl::enums::Peer::User(u) => u.user_id,
        tl::enums::Peer::Chat(c) => -c.chat_id,
        tl::enums::Peer::Channel(c) => -1_000_000_000_000 - c.channel_id,
    }
}

fn input_peer_chat_id(p: &tl::enums::InputPeer) -> Option<i64> {
    match p {
        tl::enums::InputPeer::User(u) => Some(u.user_id),
        tl::enums::InputPeer::Chat(c) => Some(-c.chat_id),
        tl::enums::InputPeer::Channel(c) => Some(-1_000_000_000_000 - c.channel_id),
        _ => None,
    }
}

fn preview_of(m: &Msg) -> String {
    if !m.text.is_empty() {
        return m.text.clone();
    }
    match m.media {
        Some(MediaKind::Photo) => "[photo]".into(),
        Some(MediaKind::Sticker) => format!("{} [sticker]", m.sticker_emoji.clone().unwrap_or_default()).trim().to_string(),
        Some(MediaKind::Voice) => "[voice message]".into(),
        Some(MediaKind::Document) => "[file]".into(),
        Some(MediaKind::Video) => "[video]".into(),
        Some(MediaKind::Gif) => "[GIF]".into(),
        Some(MediaKind::Audio) => "[audio]".into(),
        Some(MediaKind::VideoNote) => "[video message]".into(),
        Some(MediaKind::Location) => "[location]".into(),
        Some(MediaKind::Venue) => "[venue]".into(),
        Some(MediaKind::Contact) => "[contact]".into(),
        Some(MediaKind::Dice) => format!("{} [dice]", m.dice.as_ref().map(|d| d.emoji.as_str()).unwrap_or("")).trim().to_string(),
        Some(MediaKind::Poll) => format!("[poll] {}", m.poll.as_ref().map(|p| p.question.as_str()).unwrap_or("")).trim().to_string(),
        Some(MediaKind::Unsupported) => "[unsupported]".into(),
        None => String::new(),
    }
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
    let input = input_from_markdown(ctx, text).reply_to(reply_to);
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
        .edit_message(peer, msg_id, input_from_markdown(ctx, text))
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

fn register_media(ctx: &Ctx, m: &Message, chat_id: i64) {
    if let Some(media) = m.media() {
        ctx.media.lock().unwrap().insert((chat_id, m.id()), media);
    }
}

fn convert(ctx: &Ctx, m: &Message, chat_id: i64) -> Msg {
    let mi = media_info(m.media().as_ref(), m.date().with_timezone(&Local), m.edit_date().map(|d| d.with_timezone(&Local)));
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
                        Some(Reaction { emoji, count: rc.count, chosen: rc.chosen_order.is_some() })
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

    let text = m.text().to_string();
    let spans = spans_from_entities(&text, m.fmt_entities().map(|v| v.as_slice()).unwrap_or(&[]));
    let markdown = to_markdown(&text, &spans);
    let msg = Msg {
        id: m.id(),
        chat_id,
        chat_title,
        sender,
        sender_id: m
            .sender_id()
            .filter(|p| p.kind() == grammers_client::session::types::PeerKind::User)
            .and_then(|p| p.bot_api_dialog_id()),
        text,
        spans,
        markdown,
        ts: m.date().with_timezone(&Local),
        outgoing: m.outgoing(),
        media: mi.kind,
        doc_name: mi.doc_name,
        doc_size: mi.doc_size,
        duration: mi.duration,
        photo_size: mi.photo_size,
        sticker_emoji: mi.sticker_emoji,
        webpage: mi.webpage,
        forwarded_from: forwarded_from(ctx, m),
        views: m.view_count(),
        reply_to: m.reply_to_message_id(),
        reactions,
        edited: m.edit_date().is_some() && !m.edit_hide(),
        deleted: false,
        pinned: m.pinned(),
        location: mi.location,
        contact: mi.contact,
        dice: mi.dice,
        poll: mi.poll,
        keyboard: keyboard_of(m),
        topic_id: topic_of(ctx, m, chat_id),
        scheduled: false,
        audio_title: mi.audio_title,
        audio_performer: mi.audio_performer,
        round: mi.round,
    };
    if let Some(archive) = &ctx.archive {
        archive.record(msg.clone());
    }
    msg
}

/// Telegram entity offsets are UTF-16 code units; `Span`s use char indices.
fn spans_from_entities(text: &str, entities: &[tl::enums::MessageEntity]) -> Vec<Span> {
    use tl::enums::MessageEntity as E;
    if entities.is_empty() {
        return vec![];
    }
    // utf16 offset -> char index
    let mut map: Vec<usize> = Vec::with_capacity(text.len() + 1);
    let mut chars = 0usize;
    for c in text.chars() {
        for _ in 0..c.len_utf16() {
            map.push(chars);
        }
        chars += 1;
    }
    map.push(chars);
    let at = |u: i32| -> usize { map.get(u.max(0) as usize).copied().unwrap_or(chars) };
    let mut out = Vec::new();
    for e in entities {
        let (offset, length, kind) = match e {
            E::Bold(x) => (x.offset, x.length, SpanKind::Bold),
            E::Italic(x) => (x.offset, x.length, SpanKind::Italic),
            E::Underline(x) => (x.offset, x.length, SpanKind::Underline),
            E::Strike(x) => (x.offset, x.length, SpanKind::Strike),
            E::Code(x) => (x.offset, x.length, SpanKind::Code),
            E::Pre(x) => (x.offset, x.length, SpanKind::Pre(x.language.clone())),
            E::TextUrl(x) => (x.offset, x.length, SpanKind::Link(x.url.clone())),
            E::Url(x) => {
                let s = at(x.offset);
                let e2 = at(x.offset + x.length);
                let url: String = text.chars().skip(s).take(e2 - s).collect();
                (x.offset, x.length, SpanKind::Link(url))
            }
            E::MentionName(x) => (x.offset, x.length, SpanKind::Mention(x.user_id)),
            E::Spoiler(x) => (x.offset, x.length, SpanKind::Spoiler),
            E::Blockquote(x) => (x.offset, x.length, SpanKind::Blockquote),
            _ => continue,
        };
        let start = at(offset);
        let end = at(offset + length);
        if end > start {
            out.push(Span { start, end, kind });
        }
    }
    out.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    out
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
                if let Some(ev) = event_from_raw(&raw.raw) {
                    if events.send(ev).await.is_err() {
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


// ===================== wave 5: media facts, markdown, avatars =====================

#[derive(Default)]
struct MediaInfo {
    kind: Option<MediaKind>,
    doc_name: Option<String>,
    doc_size: Option<u64>,
    duration: Option<u32>,
    photo_size: Option<(i32, i32)>,
    sticker_emoji: Option<String>,
    webpage: Option<WebPreview>,
    // wave 6
    location: Option<LocationInfo>,
    contact: Option<ContactCard>,
    dice: Option<DiceInfo>,
    poll: Option<Poll>,
    audio_title: Option<String>,
    audio_performer: Option<String>,
    round: bool,
}

/// (voice, audio, video, round, animated) from Telegram's own attributes.
fn doc_flags(d: &Document) -> (bool, bool, bool, bool, bool) {
    let mut f = (false, false, false, false, false);
    if let Some(tl::enums::Document::Document(doc)) = d.raw.document.as_ref() {
        for attr in &doc.attributes {
            match attr {
                tl::enums::DocumentAttribute::Audio(a) => {
                    if a.voice {
                        f.0 = true
                    } else {
                        f.1 = true
                    }
                }
                tl::enums::DocumentAttribute::Video(v) => {
                    f.2 = true;
                    if v.round_message {
                        f.3 = true;
                    }
                }
                tl::enums::DocumentAttribute::Animated => f.4 = true,
                _ => {}
            }
        }
    }
    f
}

fn media_info(media: Option<&Media>, date: DateTime<Local>, edited: Option<DateTime<Local>>) -> MediaInfo {
    let mut mi = MediaInfo::default();
    match media {
        Some(Media::Geo(g)) => {
            mi.kind = Some(MediaKind::Location);
            mi.location = Some(LocationInfo { point: GeoPoint { lat: g.raw.lat, lon: g.raw.long }, ..Default::default() });
        }
        Some(Media::Venue(v)) => {
            mi.kind = Some(MediaKind::Venue);
            let point = v.geo.as_ref().map(|g| GeoPoint { lat: g.raw.lat, lon: g.raw.long }).unwrap_or_default();
            mi.location = Some(LocationInfo { point, title: v.title().to_string(), address: v.address().to_string(), live: None });
        }
        Some(Media::GeoLive(l)) => {
            mi.kind = Some(MediaKind::Location);
            let point = l.geo.as_ref().map(|g| GeoPoint { lat: g.raw.lat, lon: g.raw.long }).unwrap_or_default();
            let period = l.raw_geolive.period.max(0) as u32;
            let expires = date + chrono::Duration::seconds(period as i64);
            mi.location = Some(LocationInfo {
                point,
                title: String::new(),
                address: String::new(),
                live: Some(LiveLocation {
                    period_secs: period,
                    expires,
                    last_update: edited.unwrap_or(date),
                    heading: l.raw_geolive.heading.and_then(|h| u16::try_from(h).ok()),
                    stopped: period == 0 || Local::now() > expires,
                }),
            });
        }
        Some(Media::Contact(c)) => {
            mi.kind = Some(MediaKind::Contact);
            mi.contact = Some(ContactCard {
                first_name: c.first_name().to_string(),
                last_name: c.last_name().to_string(),
                phone: c.phone_number().to_string(),
                user_id: (c.raw.user_id != 0).then_some(c.raw.user_id),
            });
        }
        Some(Media::Dice(d)) => {
            mi.kind = Some(MediaKind::Dice);
            mi.dice = Some(DiceInfo { emoji: d.emoji().to_string(), value: d.value() });
        }
        Some(Media::Poll(p)) => {
            mi.kind = Some(MediaKind::Poll);
            mi.poll = Some(poll_of(&p.raw, &p.raw_results));
        }
        Some(Media::Photo(p)) => {
            mi.kind = Some(MediaKind::Photo);
            mi.doc_size = p.size().map(|s| s as u64);
        }
        Some(Media::Sticker(s)) => {
            mi.kind = Some(MediaKind::Sticker);
            mi.sticker_emoji = Some(s.emoji().to_string());
        }
        Some(Media::Document(d)) => {
            let (voice, audio, video, round, animated) = doc_flags(d);
            mi.kind = Some(if voice {
                MediaKind::Voice
            } else if round {
                MediaKind::VideoNote
            } else if animated {
                MediaKind::Gif
            } else if video {
                MediaKind::Video
            } else if audio {
                MediaKind::Audio
            } else {
                MediaKind::Document
            });
            if !voice {
                mi.doc_name = Some(d.name().filter(|n| !n.is_empty()).unwrap_or("file").to_string());
            }
            mi.round = round;
            if audio {
                if let Some(tl::enums::Document::Document(doc)) = d.raw.document.as_ref() {
                    for attr in &doc.attributes {
                        if let tl::enums::DocumentAttribute::Audio(a) = attr {
                            mi.audio_title = a.title.clone().filter(|t| !t.is_empty());
                            mi.audio_performer = a.performer.clone().filter(|t| !t.is_empty());
                        }
                    }
                }
            }
            mi.doc_size = d.size().map(|s| s as u64);
            mi.duration = d.duration().map(|s| s.round() as u32);
            mi.photo_size = d.resolution();
        }
        Some(Media::WebPage(w)) => {
            if let tl::enums::WebPage::Page(p) = &w.raw.webpage {
                mi.webpage = Some(WebPreview {
                    url: p.url.clone(),
                    site_name: p.site_name.clone().unwrap_or_default(),
                    title: p.title.clone().unwrap_or_default(),
                    description: p.description.clone().unwrap_or_default(),
                });
            }
        }
        Some(_) => mi.kind = Some(MediaKind::Unsupported),
        None => {}
    }
    mi
}

// ===================== wave 6: typed media helpers =====================

fn poll_of(raw: &tl::types::Poll, results: &tl::types::PollResults) -> Poll {
    let voters: Vec<&tl::types::PollAnswerVoters> = results
        .results
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|v| {
            let tl::enums::PollAnswerVoters::Voters(v) = v;
            v
        })
        .collect();
    let options: Vec<PollOption> = raw
        .answers
        .iter()
        .map(|a| {
            let (text, option) = match a {
                tl::enums::PollAnswer::Answer(a) => (text_of(&a.text), a.option.clone()),
                tl::enums::PollAnswer::InputPollAnswer(a) => (text_of(&a.text), Vec::new()),
            };
            let v = voters.iter().find(|v| v.option == option);
            PollOption {
                text,
                voters: v.and_then(|v| v.voters).unwrap_or(0),
                chosen: v.is_some_and(|v| v.chosen),
                correct: if raw.quiz && !results.min && !voters.is_empty() { v.map(|v| v.correct) } else { None },
            }
        })
        .collect();
    let voted = options.iter().any(|o| o.chosen);
    Poll {
        id: raw.id,
        question: text_of(&raw.question),
        options,
        total_voters: results.total_voters.unwrap_or(0),
        closed: raw.closed,
        public_voters: raw.public_voters,
        multiple_choice: raw.multiple_choice,
        quiz: raw.quiz,
        voted,
        solution: results.solution.clone().filter(|s| !s.is_empty()),
        close_date: raw.close_date.map(|d| Local.timestamp_opt(d as i64, 0).single().unwrap_or_else(Local::now)),
    }
}

fn keyboard_of(m: &Message) -> Option<Keyboard> {
    let tl::enums::Message::Message(raw) = &m.raw else { return None };
    let tl::enums::ReplyMarkup::ReplyInlineMarkup(markup) = raw.reply_markup.as_ref()? else { return None };
    let rows: Vec<Vec<KeyButton>> = markup
        .rows
        .iter()
        .map(|row| {
            let tl::enums::KeyboardButtonRow::Row(row) = row;
            row.buttons
                .iter()
                .map(|b| {
                    use tl::enums::KeyboardButton as B;
                    match b {
                        B::Callback(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Callback(b.data.clone()) },
                        B::Url(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Url(b.url.clone()) },
                        B::SwitchInline(b) => KeyButton {
                            text: b.text.clone(),
                            kind: ButtonKind::SwitchInline { query: b.query.clone(), same_chat: b.same_peer },
                        },
                        B::Button(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Other },
                        B::Game(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Other },
                        B::Buy(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Other },
                        B::UrlAuth(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Url(b.url.clone()) },
                        B::WebView(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Url(b.url.clone()) },
                        B::SimpleWebView(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Url(b.url.clone()) },
                        B::Copy(b) => KeyButton { text: b.text.clone(), kind: ButtonKind::Other },
                        other => KeyButton { text: button_text(other), kind: ButtonKind::Other },
                    }
                })
                .collect()
        })
        .collect();
    if rows.iter().all(|r| r.is_empty()) {
        return None;
    }
    Some(Keyboard { rows })
}

fn button_text(b: &tl::enums::KeyboardButton) -> String {
    use tl::enums::KeyboardButton as B;
    match b {
        B::RequestPhone(b) => b.text.clone(),
        B::RequestGeoLocation(b) => b.text.clone(),
        B::InputKeyboardButtonUrlAuth(b) => b.text.clone(),
        B::RequestPoll(b) => b.text.clone(),
        B::InputKeyboardButtonUserProfile(b) => b.text.clone(),
        B::UserProfile(b) => b.text.clone(),
        B::RequestPeer(b) => b.text.clone(),
        B::InputKeyboardButtonRequestPeer(b) => b.text.clone(),
        _ => String::new(),
    }
}

/// Topic of a message in a forum: the reply header's top id (or its reply
/// target when the header is flagged `forum_topic`), General (1) otherwise.
fn topic_of(ctx: &Ctx, m: &Message, chat_id: i64) -> Option<i32> {
    if !ctx.forums.lock().unwrap().contains(&chat_id) {
        return None;
    }
    let tl::enums::Message::Message(raw) = &m.raw else { return Some(1) };
    match raw.reply_to.as_ref() {
        Some(tl::enums::MessageReplyHeader::Header(h)) if h.forum_topic => h.reply_to_top_id.or(h.reply_to_msg_id).or(Some(1)),
        _ => Some(1),
    }
}

fn bot_commands_of(info: Option<&tl::enums::BotInfo>) -> Vec<BotCommand> {
    let Some(tl::enums::BotInfo::Info(info)) = info else { return vec![] };
    info.commands
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|c| {
            let tl::enums::BotCommand::Command(c) = c;
            BotCommand { command: c.command.clone(), description: c.description.clone() }
        })
        .collect()
}

fn forwarded_from(ctx: &Ctx, m: &Message) -> Option<String> {
    let tl::enums::MessageFwdHeader::Header(h) = m.forward_header()?;
    let name = h
        .from_name
        .clone()
        .or_else(|| h.from_id.as_ref().and_then(|p| ctx.titles.lock().unwrap().get(&peer_chat_id(p)).cloned()))
        .unwrap_or_else(|| "unknown".to_string());
    Some(name)
}

/// Char index -> UTF-16 offset map for `text` (Telegram entity offsets).
fn utf16_offsets(text: &str) -> Vec<i32> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut acc = 0i32;
    for c in text.chars() {
        out.push(acc);
        acc += c.len_utf16() as i32;
    }
    out.push(acc);
    out
}

fn entities_from_spans(text: &str, spans: &[Span]) -> Vec<tl::enums::MessageEntity> {
    use tl::enums::MessageEntity as E;
    let map = utf16_offsets(text);
    let at = |i: usize| map.get(i).copied().unwrap_or(*map.last().unwrap_or(&0));
    spans
        .iter()
        .filter_map(|s| {
            let offset = at(s.start);
            let length = at(s.end) - offset;
            if length <= 0 {
                return None;
            }
            Some(match &s.kind {
                SpanKind::Bold => E::Bold(tl::types::MessageEntityBold { offset, length }),
                SpanKind::Italic => E::Italic(tl::types::MessageEntityItalic { offset, length }),
                SpanKind::Underline => E::Underline(tl::types::MessageEntityUnderline { offset, length }),
                SpanKind::Strike => E::Strike(tl::types::MessageEntityStrike { offset, length }),
                SpanKind::Code => E::Code(tl::types::MessageEntityCode { offset, length }),
                SpanKind::Pre(lang) => E::Pre(tl::types::MessageEntityPre { offset, length, language: lang.clone() }),
                SpanKind::Link(url) => E::TextUrl(tl::types::MessageEntityTextUrl { offset, length, url: url.clone() }),
                SpanKind::Spoiler => E::Spoiler(tl::types::MessageEntitySpoiler { offset, length }),
                SpanKind::Blockquote => E::Blockquote(tl::types::MessageEntityBlockquote { collapsed: false, offset, length }),
                SpanKind::Mention(_) => return None,
            })
        })
        .collect()
}

/// Composer text -> InputMessage, parsing Telegram-style markers unless the
/// user turned markdown off.
fn input_from_markdown(ctx: &Ctx, text: &str) -> InputMessage {
    if !ctx.flags.lock().unwrap().markdown_send {
        return InputMessage::new().text(text);
    }
    let (plain, spans) = parse_markdown(text);
    if spans.is_empty() {
        InputMessage::new().text(text)
    } else {
        let entities = entities_from_spans(&plain, &spans);
        InputMessage::new().text(plain).fmt_entities(entities)
    }
}

fn input_peer(ctx: &Ctx, chat_id: i64) -> Result<tl::enums::InputPeer, TgError> {
    Ok(ctx.peer(chat_id)?.into())
}

fn private_dir(dir: &std::path::Path) -> Result<(), TgError> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    Ok(())
}

async fn download_avatar(client: &Client, ctx: &Arc<Ctx>, chat_id: i64) -> Result<Option<PathBuf>, TgError> {
    let Some(photo_id) = ctx.photos.lock().unwrap().get(&chat_id).copied() else {
        return Ok(None);
    };
    let dir = paths::avatar_dir();
    private_dir(&dir)?;
    let path = dir.join(format!("{chat_id}_{photo_id}.jpg"));
    if path.exists() {
        return Ok(Some(path));
    }
    let peer = input_peer(ctx, chat_id)?;
    let location = ChatPhoto {
        raw: tl::enums::InputFileLocation::InputPeerPhotoFileLocation(tl::types::InputPeerPhotoFileLocation {
            big: false,
            peer,
            photo_id,
        }),
    };
    client
        .download_media(&location, &path)
        .await
        .map_err(|e| format!("avatar download failed: {e}"))?;
    chmod_600(&path);
    Ok(Some(path))
}

/// Decode a webp (stickers) into a png next to it; GdkPixbuf has no webp loader here.
fn webp_to_png(path: &std::path::Path, png: &std::path::Path) -> Result<(), TgError> {
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| format!("sticker decode failed: {e}"))?;
    img.save(png).map_err(|e| e.to_string())?;
    chmod_600(png);
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// Stickers/gifs from the cached raw documents (see `remember_documents`).
async fn download_document_by_id(client: &Client, ctx: &Arc<Ctx>, id: i64, sticker: bool) -> Result<Option<PathBuf>, TgError> {
    let Some(doc) = ctx.documents.lock().unwrap().get(&id).cloned() else {
        return Err("unknown sticker or gif — open the picker again".into());
    };
    let dir = paths::media_dir();
    private_dir(&dir)?;
    let mime = doc.mime_type.as_str();
    if sticker && mime != "image/webp" && mime != "image/png" {
        // .tgs / .webm stickers cannot be rendered here.
        return Ok(None);
    }
    let ext = match mime {
        "image/webp" => "webp",
        "image/png" => "png",
        "video/mp4" => "mp4",
        "image/gif" => "gif",
        _ => "bin",
    };
    let stem = format!("doc_{id}");
    let final_path = dir.join(format!("{stem}.{}", if ext == "webp" { "png" } else { ext }));
    if final_path.exists() {
        return Ok(Some(final_path));
    }
    let path = dir.join(format!("{stem}.{ext}"));
    let location = ChatPhoto {
        raw: tl::enums::InputFileLocation::InputDocumentFileLocation(tl::types::InputDocumentFileLocation {
            id: doc.id,
            access_hash: doc.access_hash,
            file_reference: doc.file_reference.clone(),
            thumb_size: String::new(),
        }),
    };
    client
        .download_media(&location, &path)
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    chmod_600(&path);
    if ext == "webp" {
        webp_to_png(&path, &final_path)?;
    }
    Ok(Some(final_path))
}

fn remember_documents(ctx: &Ctx, docs: &[tl::enums::Document]) -> Vec<tl::types::Document> {
    let mut out = Vec::new();
    let mut cache = ctx.documents.lock().unwrap();
    for d in docs {
        if let tl::enums::Document::Document(d) = d {
            cache.insert(d.id, d.clone());
            out.push(d.clone());
        }
    }
    out
}

fn sticker_from_doc(d: &tl::types::Document) -> Sticker {
    let emoji = d
        .attributes
        .iter()
        .find_map(|a| match a {
            tl::enums::DocumentAttribute::Sticker(s) => Some(s.alt.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let animated = d.mime_type != "image/webp" && d.mime_type != "image/png";
    Sticker { id: d.id, emoji, animated, video: d.mime_type == "video/webm" }
}

// ===================== wave 5: messages =====================

async fn get_messages(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, ids: &[i32]) -> Result<Vec<Msg>, TgError> {
    let peer = ctx.peer(chat_id)?;
    let fetched = client.get_messages_by_id(peer, ids).await.map_err(|e| e.to_string())?;
    Ok(fetched.into_iter().flatten().map(|m| convert(ctx, &m, chat_id)).collect())
}

async fn delete_messages(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, ids: &[i32]) -> Result<(), TgError> {
    let peer = ctx.peer(chat_id)?;
    client
        .delete_messages(peer, ids)
        .await
        .map(|_| ())
        .map_err(|e| format!("delete failed: {e}"))
}

async fn forward_messages(client: &Client, ctx: &Arc<Ctx>, from_chat: i64, ids: &[i32], to_chat: i64) -> Result<Vec<Msg>, TgError> {
    let from = ctx.peer(from_chat)?;
    let to = ctx.peer(to_chat)?;
    let sent = client
        .forward_messages(to, ids, from)
        .await
        .map_err(|e| format!("forward failed: {e}"))?;
    Ok(sent.into_iter().flatten().map(|m| convert(ctx, &m, to_chat)).collect())
}

async fn search_messages(
    client: &Client,
    ctx: &Arc<Ctx>,
    chat_id: i64,
    query: &str,
    before_id: Option<i32>,
    filter: Option<tl::enums::MessagesFilter>,
) -> Result<Vec<Msg>, TgError> {
    let peer = ctx.peer(chat_id)?;
    let mut iter = client.search_messages(peer).query(query).limit(50);
    if let Some(f) = filter {
        iter = iter.filter(f);
    }
    if let Some(b) = before_id {
        iter = iter.offset_id(b);
    }
    let mut out = Vec::new();
    while let Some(m) = iter.next().await.map_err(|e| e.to_string())? {
        out.push(convert(ctx, &m, chat_id));
        if out.len() >= 50 {
            break;
        }
    }
    Ok(out)
}

async fn search_global(client: &Client, ctx: &Arc<Ctx>, query: &str) -> Result<Vec<Msg>, TgError> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut iter = client.search_all_messages().query(query).limit(50);
    let mut out = Vec::new();
    while let Some(m) = iter.next().await.map_err(|e| e.to_string())? {
        let chat_id = m.peer_id().bot_api_dialog_id_unchecked();
        remember_from_message(ctx, &m, chat_id).await;
        out.push(convert(ctx, &m, chat_id));
        if out.len() >= 50 {
            break;
        }
    }
    Ok(out)
}

/// Public usernames and contacts not in the dialog list.
async fn search_chats(client: &Client, ctx: &Arc<Ctx>, query: &str) -> Result<Vec<ChatSummary>, TgError> {
    let q = query.trim().trim_start_matches('@');
    if q.len() < 3 {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    // Contacts by name (cheap, cached by Telegram).
    if let Ok(contacts) = get_contacts(client, ctx).await {
        let ql = q.to_lowercase();
        for c in contacts {
            let known = ctx.meta.lock().unwrap().contains_key(&c.user_id);
            if !known && (c.name.to_lowercase().contains(&ql) || c.username.to_lowercase().contains(&ql)) {
                out.push(ChatSummary {
                    id: c.user_id,
                    title: c.name,
                    kind: ChatKind::User,
                    username: c.username,
                    presence: c.presence,
                    has_photo: c.has_photo,
                    ..ChatSummary::default()
                });
            }
        }
    }
    // Exact public username.
    if !q.contains(char::is_whitespace) {
        if let Ok(Some(peer)) = client.resolve_username(q).await {
            let id = peer.id().bot_api_dialog_id_unchecked();
            if !out.iter().any(|c| c.id == id) {
                let facts = describe_peer(&peer);
                let title = facts.title_override.clone().unwrap_or_else(|| peer.name().unwrap_or(q).to_string());
                if let Ok(Some(r)) = peer_ref_of(&peer).await {
                    ctx.remember(id, r, Some(&title));
                }
                if let Some(pid) = facts.photo_id {
                    ctx.photos.lock().unwrap().insert(id, pid);
                }
                out.push(ChatSummary {
                    id,
                    title,
                    kind: facts.kind,
                    username: facts.username,
                    presence: facts.presence,
                    has_photo: facts.photo_id.is_some(),
                    ..ChatSummary::default()
                });
            }
        }
    }
    Ok(out)
}

async fn peer_ref_of(peer: &Peer) -> Result<Option<PeerRef>, TgError> {
    match peer {
        Peer::User(u) => u.to_ref().await.map_err(|e| e.to_string()),
        Peer::Group(g) => g.to_ref().await.map_err(|e| e.to_string()),
        Peer::Channel(c) => c.to_ref().await.map_err(|e| e.to_string()),
    }
}

async fn get_pinned_message(client: &Client, ctx: &Arc<Ctx>, chat_id: i64) -> Result<Option<Msg>, TgError> {
    let peer = ctx.peer(chat_id)?;
    let m = client.get_pinned_message(peer).await.map_err(|e| e.to_string())?;
    Ok(m.map(|m| convert(ctx, &m, chat_id)))
}

async fn pin_message(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, msg_id: i32, pinned: bool) -> Result<(), TgError> {
    let peer = ctx.peer(chat_id)?;
    let r = if pinned {
        client.pin_message(peer, msg_id).await
    } else {
        client.unpin_message(peer, msg_id).await
    };
    r.map_err(|e| format!("pin failed: {e}"))
}

async fn send_reaction(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, msg_id: i32, emoji: Option<String>) -> Result<(), TgError> {
    use grammers_client::message::InputReactions;
    let peer = ctx.peer(chat_id)?;
    let reactions = match emoji {
        Some(e) => InputReactions::emoticon(e),
        None => InputReactions::remove(),
    };
    client
        .send_reactions(peer, msg_id, reactions)
        .await
        .map_err(|e| format!("reaction failed: {e}"))
}

async fn available_reactions(client: &Client) -> Result<Vec<String>, TgError> {
    let r = client
        .invoke(&tl::functions::messages::GetAvailableReactions { hash: 0 })
        .await
        .map_err(|e| e.to_string())?;
    Ok(match r {
        tl::enums::messages::AvailableReactions::Reactions(r) => r
            .reactions
            .into_iter()
            .filter_map(|a| {
                let tl::enums::AvailableReaction::Reaction(a) = a;
                (!a.inactive && !a.premium).then_some(a.reaction)
            })
            .collect(),
        _ => vec![],
    })
}

async fn send_voice(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, path: &std::path::Path, duration: u32) -> Result<Msg, TgError> {
    use grammers_client::media::Attribute;
    let peer = ctx.peer(chat_id)?;
    let uploaded = client.upload_file(path).await.map_err(|e| format!("upload failed: {e}"))?;
    let input = InputMessage::new()
        .document(uploaded)
        .mime_type("audio/ogg")
        .attribute(Attribute::Voice { duration: std::time::Duration::from_secs(duration as u64), waveform: None });
    let sent = client.send_message(peer, input).await.map_err(|e| format!("send failed: {e}"))?;
    Ok(convert(ctx, &sent, chat_id))
}

async fn send_document_by_id(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, id: i64) -> Result<Msg, TgError> {
    let Some(doc) = ctx.documents.lock().unwrap().get(&id).cloned() else {
        return Err("unknown sticker or gif — open the picker again".into());
    };
    let peer = ctx.peer(chat_id)?;
    let media = tl::enums::InputMedia::Document(tl::types::InputMediaDocument {
        spoiler: false,
        id: tl::enums::InputDocument::Document(tl::types::InputDocument {
            id: doc.id,
            access_hash: doc.access_hash,
            file_reference: doc.file_reference.clone(),
        }),
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
        query: None,
    });
    let sent = client
        .send_message(peer, InputMessage::new().media(media))
        .await
        .map_err(|e| format!("send failed: {e}"))?;
    Ok(convert(ctx, &sent, chat_id))
}

// ===================== wave 5: chat actions =====================

fn dialog_peer(ctx: &Ctx, chat_id: i64) -> Result<tl::enums::InputDialogPeer, TgError> {
    Ok(tl::enums::InputDialogPeer::Peer(tl::types::InputDialogPeer { peer: input_peer(ctx, chat_id)? }))
}

async fn set_pinned(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, pinned: bool) -> Result<(), TgError> {
    client
        .invoke(&tl::functions::messages::ToggleDialogPin { pinned, peer: dialog_peer(ctx, chat_id)? })
        .await
        .map(|_| ())
        .map_err(|e| format!("pin failed: {e}"))
}

async fn set_muted(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, mode: MuteMode) -> Result<(), TgError> {
    let until = match mode {
        MuteMode::Unmute => 0,
        MuteMode::Forever => i32::MAX,
        MuteMode::Hours(h) => (Local::now().timestamp() as i32).saturating_add((h as i32).saturating_mul(3600)),
    };
    client
        .invoke(&tl::functions::account::UpdateNotifySettings {
            peer: tl::enums::InputNotifyPeer::Peer(tl::types::InputNotifyPeer { peer: input_peer(ctx, chat_id)? }),
            settings: tl::enums::InputPeerNotifySettings::Settings(tl::types::InputPeerNotifySettings {
                show_previews: None,
                silent: None,
                mute_until: Some(until),
                sound: None,
                stories_muted: None,
                stories_hide_sender: None,
                stories_sound: None,
            }),
        })
        .await
        .map(|_| ())
        .map_err(|e| format!("mute failed: {e}"))?;
    if let Some(m) = ctx.meta.lock().unwrap().get_mut(&chat_id) {
        m.muted = mode != MuteMode::Unmute;
    }
    Ok(())
}

async fn set_archived(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, archived: bool) -> Result<(), TgError> {
    client
        .invoke(&tl::functions::folders::EditPeerFolders {
            folder_peers: vec![tl::enums::InputFolderPeer::Peer(tl::types::InputFolderPeer {
                peer: input_peer(ctx, chat_id)?,
                folder_id: if archived { 1 } else { 0 },
            })],
        })
        .await
        .map(|_| ())
        .map_err(|e| format!("archive failed: {e}"))?;
    if let Some(m) = ctx.meta.lock().unwrap().get_mut(&chat_id) {
        m.archived = archived;
    }
    Ok(())
}

async fn mark_unread(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, unread: bool) -> Result<(), TgError> {
    client
        .invoke(&tl::functions::messages::MarkDialogUnread { unread, parent_peer: None, peer: dialog_peer(ctx, chat_id)? })
        .await
        .map(|_| ())
        .map_err(|e| format!("mark unread failed: {e}"))
}

async fn delete_chat(client: &Client, ctx: &Arc<Ctx>, chat_id: i64) -> Result<(), TgError> {
    let peer = ctx.peer(chat_id)?;
    client.delete_dialog(peer).await.map_err(|e| format!("delete chat failed: {e}"))
}

async fn clear_history(client: &Client, ctx: &Arc<Ctx>, chat_id: i64) -> Result<(), TgError> {
    let peer = ctx.peer(chat_id)?;
    if peer.id.kind() == grammers_client::session::types::PeerKind::Channel {
        client
            .invoke(&tl::functions::channels::DeleteHistory { for_everyone: false, channel: peer.into(), max_id: 0 })
            .await
            .map(|_| ())
            .map_err(|e| format!("clear history failed: {e}"))
    } else {
        client
            .invoke(&tl::functions::messages::DeleteHistory {
                just_clear: true,
                revoke: false,
                peer: peer.into(),
                max_id: 0,
                min_date: None,
                max_date: None,
            })
            .await
            .map(|_| ())
            .map_err(|e| format!("clear history failed: {e}"))
    }
}

async fn save_draft(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, text: &str) -> Result<(), TgError> {
    client
        .invoke(&tl::functions::messages::SaveDraft {
            no_webpage: false,
            invert_media: false,
            reply_to: None,
            peer: input_peer(ctx, chat_id)?,
            message: text.to_string(),
            entities: None,
            media: None,
            effect: None,
            suggested_post: None,
            rich_message: None,
        })
        .await
        .map(|_| ())
        .map_err(|e| format!("draft not saved: {e}"))
}

// ===================== wave 5: info, contacts, groups, folders =====================

fn user_facts(u: &tl::types::User) -> (String, String, String, Presence, bool, bool) {
    let name = format!("{} {}", u.first_name.clone().unwrap_or_default(), u.last_name.clone().unwrap_or_default())
        .trim()
        .to_string();
    let presence = u.status.as_ref().map(presence_from).unwrap_or_default();
    let has_photo = matches!(u.photo, Some(tl::enums::UserProfilePhoto::Photo(_)));
    (
        name,
        u.username.clone().unwrap_or_default(),
        u.phone.clone().unwrap_or_default(),
        presence,
        has_photo,
        u.contact,
    )
}

async fn get_chat_info(client: &Client, ctx: &Arc<Ctx>, chat_id: i64) -> Result<ChatInfo, TgError> {
    use grammers_client::session::types::PeerKind;
    let peer = ctx.peer(chat_id)?;
    let title = ctx.titles.lock().unwrap().get(&chat_id).cloned().unwrap_or_default();
    let meta = ctx.meta.lock().unwrap().get(&chat_id).copied();
    let now = Local::now().timestamp() as i32;
    match peer.id.kind() {
        PeerKind::User => {
            let r = client
                .invoke(&tl::functions::users::GetFullUser { id: peer.into() })
                .await
                .map_err(|e| e.to_string())?;
            let tl::enums::users::UserFull::Full(full) = r;
            let tl::enums::UserFull::Full(f) = full.full_user;
            let user = full.users.into_iter().find_map(|u| match u {
                tl::enums::User::User(u) if u.id == chat_id => Some(u),
                _ => None,
            });
            let (name, username, phone, presence, has_photo, contact) =
                user.as_ref().map(user_facts).unwrap_or((title.clone(), String::new(), String::new(), Presence::Unknown, false, false));
            Ok(ChatInfo {
                id: chat_id,
                title: if name.is_empty() { title } else { name },
                kind: meta.map(|m| m.kind).unwrap_or(ChatKind::User),
                username,
                phone,
                about: f.about.unwrap_or_default(),
                members: None,
                presence,
                has_photo,
                muted: is_muted(&f.notify_settings, now),
                is_contact: contact,
                bot_commands: bot_commands_of(f.bot_info.as_ref()),
                forum: false,
            })
        }
        PeerKind::Chat => {
            let r = client
                .invoke(&tl::functions::messages::GetFullChat { chat_id: peer.id.bare_id_unchecked() })
                .await
                .map_err(|e| e.to_string())?;
            let tl::enums::messages::ChatFull::Full(full) = r;
            let (about, members, muted) = match full.full_chat {
                tl::enums::ChatFull::Full(f) => (
                    f.about,
                    match &f.participants {
                        tl::enums::ChatParticipants::Participants(p) => Some(p.participants.len() as i32),
                        _ => None,
                    },
                    is_muted(&f.notify_settings, now),
                ),
                tl::enums::ChatFull::ChannelFull(f) => (f.about, f.participants_count, is_muted(&f.notify_settings, now)),
            };
            Ok(ChatInfo {
                id: chat_id,
                title,
                kind: ChatKind::Group,
                about,
                members,
                muted,
                ..ChatInfo::default()
            })
        }
        PeerKind::Channel => {
            let r = client
                .invoke(&tl::functions::channels::GetFullChannel { channel: peer.into() })
                .await
                .map_err(|e| e.to_string())?;
            let tl::enums::messages::ChatFull::Full(full) = r;
            let (about, members, muted) = match full.full_chat {
                tl::enums::ChatFull::ChannelFull(f) => (f.about, f.participants_count, is_muted(&f.notify_settings, now)),
                tl::enums::ChatFull::Full(f) => (f.about, None, is_muted(&f.notify_settings, now)),
            };
            let username = full
                .chats
                .iter()
                .find_map(|c| match c {
                    tl::enums::Chat::Channel(c) => c.username.clone(),
                    _ => None,
                })
                .unwrap_or_default();
            Ok(ChatInfo {
                id: chat_id,
                title,
                kind: meta.map(|m| m.kind).unwrap_or(ChatKind::Channel),
                username,
                about,
                members,
                muted,
                forum: ctx.forums.lock().unwrap().contains(&chat_id),
                ..ChatInfo::default()
            })
        }
    }
}

async fn get_members(client: &Client, ctx: &Arc<Ctx>, chat_id: i64, offset: i32, limit: i32) -> Result<Vec<Member>, TgError> {
    let peer = ctx.peer(chat_id)?;
    let mut iter = client.iter_participants(peer);
    let mut seen = 0;
    let mut out = Vec::new();
    while let Some(p) = iter.next().await.map_err(|e| e.to_string())? {
        seen += 1;
        if seen <= offset {
            continue;
        }
        let user_id = p.user.id().bot_api_dialog_id_unchecked();
        if let Ok(Some(r)) = p.user.to_ref().await {
            ctx.remember(user_id, r, Some(&p.user.full_name()));
        }
        if let Some(photo) = p.user.photo() {
            ctx.photos.lock().unwrap().insert(user_id, photo.photo_id);
        }
        out.push(Member {
            user_id,
            name: p.user.full_name(),
            username: p.user.username().unwrap_or("").to_string(),
            presence: presence_from(p.user.status()),
            role: match p.role {
                Role::Creator(_) => MemberRole::Creator,
                Role::Admin(_) => MemberRole::Admin,
                _ => MemberRole::Member,
            },
        });
        if out.len() as i32 >= limit.max(1) {
            break;
        }
    }
    Ok(out)
}

async fn get_contacts(client: &Client, ctx: &Arc<Ctx>) -> Result<Vec<Contact>, TgError> {
    let r = client
        .invoke(&tl::functions::contacts::GetContacts { hash: 0 })
        .await
        .map_err(|e| e.to_string())?;
    let tl::enums::contacts::Contacts::Contacts(c) = r else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for u in c.users {
        let tl::enums::User::User(raw) = u else { continue };
        let (name, username, phone, presence, has_photo, _) = user_facts(&raw);
        let id = raw.id;
        if let Some(tl::enums::UserProfilePhoto::Photo(p)) = &raw.photo {
            ctx.photos.lock().unwrap().insert(id, p.photo_id);
        }
        let user = grammers_client::peer::User::from_raw(client, tl::enums::User::User(raw));
        if let Ok(Some(r)) = user.to_ref().await {
            ctx.remember(id, r, Some(&name));
        }
        out.push(Contact { user_id: id, name, username, phone, presence, has_photo, story_ring: StoryRing::None });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

async fn open_user(client: &Client, ctx: &Arc<Ctx>, user_id: i64) -> Result<ChatSummary, TgError> {
    if ctx.peer(user_id).is_err() {
        // Saved Messages or a user we have not cached yet.
        let me = client.get_me().await.map_err(|e| e.to_string())?;
        if me.id().bot_api_dialog_id_unchecked() == user_id {
            if let Ok(Some(r)) = me.to_ref().await {
                ctx.remember(user_id, r, Some("Saved Messages"));
            }
        } else {
            // Contacts fill the peer cache.
            let _ = get_contacts(client, ctx).await;
        }
    }
    ctx.peer(user_id)?;
    let title = ctx.titles.lock().unwrap().get(&user_id).cloned().unwrap_or_default();
    let meta = ctx.meta.lock().unwrap().get(&user_id).copied();
    let has_photo = ctx.photos.lock().unwrap().contains_key(&user_id);
    let kind = meta.map(|m| m.kind).unwrap_or(if title == "Saved Messages" { ChatKind::Saved } else { ChatKind::User });
    Ok(ChatSummary {
        id: user_id,
        title: if title.is_empty() { "Chat".to_string() } else { title },
        kind,
        has_photo,
        ..ChatSummary::default()
    })
}

async fn create_group(client: &Client, ctx: &Arc<Ctx>, title: &str, user_ids: &[i64]) -> Result<ChatSummary, TgError> {
    if title.is_empty() {
        return Err("the group needs a title".into());
    }
    if user_ids.is_empty() {
        return Err("pick at least one member".into());
    }
    let mut users = Vec::new();
    for id in user_ids {
        users.push(tl::enums::InputUser::from(ctx.peer(*id)?));
    }
    let r = client
        .invoke(&tl::functions::messages::CreateChat { users, title: title.to_string(), ttl_period: None })
        .await
        .map_err(|e| format!("could not create the group: {e}"))?;
    let tl::enums::messages::InvitedUsers::Users(iu) = r;
    let chats = match iu.updates {
        tl::enums::Updates::Updates(u) => u.chats,
        tl::enums::Updates::Combined(u) => u.chats,
        _ => vec![],
    };
    for chat in chats {
        if let tl::enums::Chat::Chat(c) = &chat {
            let chat_id = -c.id;
            let group = grammers_client::peer::Group::from_raw(client, chat.clone());
            if let Ok(Some(r)) = group.to_ref().await {
                ctx.remember(chat_id, r, Some(title));
                ctx.meta.lock().unwrap().insert(
                    chat_id,
                    DialogMeta { kind: ChatKind::Group, contact: false, muted: false, unread: false, archived: false },
                );
                return Ok(ChatSummary { id: chat_id, title: title.to_string(), kind: ChatKind::Group, ..ChatSummary::default() });
            }
        }
    }
    Err("the group was created but could not be opened — reload the chat list".into())
}

async fn get_folders(client: &Client, ctx: &Arc<Ctx>) -> Result<Vec<Folder>, TgError> {
    let r = client
        .invoke(&tl::functions::messages::GetDialogFilters {})
        .await
        .map_err(|e| e.to_string())?;
    let tl::enums::messages::DialogFilters::Filters(f) = r;
    let meta = ctx.meta.lock().unwrap().clone();
    let mut out = Vec::new();
    for filter in f.filters {
        let (id, title, pinned, include, exclude, flags) = match filter {
            tl::enums::DialogFilter::Filter(d) => {
                let flags = Some((d.contacts, d.non_contacts, d.groups, d.broadcasts, d.bots, d.exclude_muted, d.exclude_read, d.exclude_archived));
                (d.id, text_of(&d.title), d.pinned_peers, d.include_peers, d.exclude_peers, flags)
            }
            tl::enums::DialogFilter::Chatlist(d) => (d.id, text_of(&d.title), d.pinned_peers, d.include_peers, vec![], None),
            tl::enums::DialogFilter::Default => continue,
        };
        let mut chats: Vec<i64> = pinned.iter().chain(include.iter()).filter_map(input_peer_chat_id).collect();
        let excluded: Vec<i64> = exclude.iter().filter_map(input_peer_chat_id).collect();
        if let Some((contacts, non_contacts, groups, broadcasts, bots, ex_muted, ex_read, ex_archived)) = flags {
            for (&chat_id, m) in &meta {
                if chats.contains(&chat_id) || excluded.contains(&chat_id) {
                    continue;
                }
                let by_kind = match m.kind {
                    ChatKind::User | ChatKind::Saved => (contacts && m.contact) || (non_contacts && !m.contact),
                    ChatKind::Bot => bots,
                    ChatKind::Group => groups,
                    ChatKind::Channel => broadcasts,
                };
                if by_kind && !(ex_muted && m.muted) && !(ex_read && !m.unread) && !(ex_archived && m.archived) {
                    chats.push(chat_id);
                }
            }
        }
        chats.sort_unstable();
        chats.dedup();
        out.push(Folder { id, title, chats });
    }
    Ok(out)
}

fn text_of(t: &tl::enums::TextWithEntities) -> String {
    let tl::enums::TextWithEntities::Entities(t) = t;
    t.text.clone()
}

// ===================== wave 5: stickers & gifs =====================

async fn sticker_packs(client: &Client, ctx: &Arc<Ctx>) -> Result<Vec<StickerPack>, TgError> {
    let mut out = Vec::new();
    if let Ok(tl::enums::messages::RecentStickers::Stickers(r)) = client
        .invoke(&tl::functions::messages::GetRecentStickers { attached: false, hash: 0 })
        .await
    {
        let docs = remember_documents(ctx, &r.stickers);
        if !docs.is_empty() {
            out.push(StickerPack { id: "recent".into(), title: "Recent".into(), count: docs.len() as i32 });
        }
    }
    if let Ok(tl::enums::messages::FavedStickers::Stickers(f)) =
        client.invoke(&tl::functions::messages::GetFavedStickers { hash: 0 }).await
    {
        let docs = remember_documents(ctx, &f.stickers);
        if !docs.is_empty() {
            out.push(StickerPack { id: "favorites".into(), title: "Favorites".into(), count: docs.len() as i32 });
        }
    }
    match client.invoke(&tl::functions::messages::GetAllStickers { hash: 0 }).await {
        Ok(tl::enums::messages::AllStickers::Stickers(s)) => {
            for set in s.sets {
                let tl::enums::StickerSet::Set(set) = set;
                if set.masks || set.emojis {
                    continue;
                }
                ctx.sticker_sets.lock().unwrap().insert(set.id, set.access_hash);
                out.push(StickerPack { id: set.id.to_string(), title: set.title, count: set.count });
            }
        }
        Ok(_) => {}
        Err(e) => return Err(format!("could not load sticker packs: {e}")),
    }
    Ok(out)
}

async fn stickers_of(client: &Client, ctx: &Arc<Ctx>, pack_id: &str) -> Result<Vec<Sticker>, TgError> {
    let docs = match pack_id {
        "recent" => match client
            .invoke(&tl::functions::messages::GetRecentStickers { attached: false, hash: 0 })
            .await
            .map_err(|e| e.to_string())?
        {
            tl::enums::messages::RecentStickers::Stickers(r) => remember_documents(ctx, &r.stickers),
            _ => vec![],
        },
        "favorites" => match client
            .invoke(&tl::functions::messages::GetFavedStickers { hash: 0 })
            .await
            .map_err(|e| e.to_string())?
        {
            tl::enums::messages::FavedStickers::Stickers(f) => remember_documents(ctx, &f.stickers),
            _ => vec![],
        },
        set_id => {
            let id: i64 = set_id.parse().map_err(|_| format!("unknown sticker pack {set_id}"))?;
            let access_hash = ctx
                .sticker_sets
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                .ok_or_else(|| "unknown sticker pack — reopen the picker".to_string())?;
            match client
                .invoke(&tl::functions::messages::GetStickerSet {
                    stickerset: tl::enums::InputStickerSet::Id(tl::types::InputStickerSetId { id, access_hash }),
                    hash: 0,
                })
                .await
                .map_err(|e| e.to_string())?
            {
                tl::enums::messages::StickerSet::Set(s) => remember_documents(ctx, &s.documents),
                _ => vec![],
            }
        }
    };
    Ok(docs.iter().map(sticker_from_doc).collect())
}

async fn saved_gifs(client: &Client, ctx: &Arc<Ctx>) -> Result<Vec<Gif>, TgError> {
    let r = client
        .invoke(&tl::functions::messages::GetSavedGifs { hash: 0 })
        .await
        .map_err(|e| e.to_string())?;
    let tl::enums::messages::SavedGifs::Gifs(g) = r else {
        return Ok(vec![]);
    };
    let docs = remember_documents(ctx, &g.gifs);
    Ok(docs
        .iter()
        .map(|d| {
            let (w, h) = d
                .attributes
                .iter()
                .find_map(|a| match a {
                    tl::enums::DocumentAttribute::Video(v) => Some((v.w, v.h)),
                    _ => None,
                })
                .unwrap_or((0, 0));
            Gif { id: d.id, width: w, height: h }
        })
        .collect())
}

// ===================== wave 5: raw updates =====================

fn event_from_raw(u: &tl::enums::Update) -> Option<Event> {
    use tl::enums::Update as U;
    Some(match u {
        U::ReadHistoryOutbox(x) => Event::ReadOutbox { chat_id: peer_chat_id(&x.peer), max_id: x.max_id },
        U::ReadChannelOutbox(x) => Event::ReadOutbox { chat_id: -1_000_000_000_000 - x.channel_id, max_id: x.max_id },
        U::ReadHistoryInbox(x) => Event::ReadInbox { chat_id: peer_chat_id(&x.peer), max_id: x.max_id },
        U::ReadChannelInbox(x) => Event::ReadInbox { chat_id: -1_000_000_000_000 - x.channel_id, max_id: x.max_id },
        U::UserStatus(x) => Event::Presence { user_id: x.user_id, presence: presence_from(&x.status) },
        U::DialogPinned(_)
        | U::PinnedDialogs(_)
        | U::NotifySettings(_)
        | U::FolderPeers(_)
        | U::DialogUnreadMark(_)
        | U::DraftMessage(_)
        | U::DialogFilters
        | U::DialogFilter(_) => Event::DialogsChanged,
        U::PinnedMessages(x) => Event::PinnedChanged { chat_id: peer_chat_id(&x.peer) },
        _ => return None,
    })
}
