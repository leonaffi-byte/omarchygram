//! Telegram backend bridge. Orchestrator-owned: do not modify in delegations.
//!
//! The backend (grammers + tokio) runs on its own thread; the UI talks to it
//! through this module only, in the plain types below — never in grammers
//! types. All `Tg` methods are async and safe to await on the GLib main
//! context (`glib::MainContext::spawn_local`); `Event`s are read from
//! `Tg::events` the same way. Both mock and real backends behave identically.
//!
//! Wave 5 (specs/spec-wave5.md §1) is the contract for the fields and
//! methods below; the UI must compile against nothing else.

mod archive;
mod history_cache;
mod calls;
mod markdown;
mod mock;
mod real;
mod places;
pub use places::Place;

use std::path::PathBuf;

use chrono::{DateTime, Local};
use tokio::sync::{mpsc, oneshot};

pub use markdown::{parse_markdown, to_markdown};

pub mod paths {
    use std::path::PathBuf;

    // A missing XDG base dir must fail loudly: falling back to a relative
    // path would drop the session auth key into the current directory.

    pub fn config_file() -> PathBuf {
        dirs::config_dir()
            .expect("cannot determine XDG config dir — is HOME set?")
            .join("omarchygram/config.toml")
    }

    pub fn session_file() -> PathBuf {
        dirs::data_dir()
            .expect("cannot determine XDG data dir — is HOME set?")
            .join("omarchygram/omarchygram.session")
    }

    pub fn media_dir() -> PathBuf {
        dirs::cache_dir()
            .expect("cannot determine XDG cache dir — is HOME set?")
            .join("omarchygram/media")
    }

    pub fn avatar_dir() -> PathBuf {
        dirs::cache_dir()
            .expect("cannot determine XDG cache dir — is HOME set?")
            .join("omarchygram/avatars")
    }
}

pub const SETUP_HELP: &str = "Omarchygram needs Telegram API credentials (one-time setup):

  1. Log in at https://my.telegram.org/apps with your Telegram account
  2. Create an application (any name, platform \"Desktop\")
  3. Enter the api_id and api_hash below.
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthState {
    /// config.toml missing/invalid — the UI shows the credentials form.
    NeedCredentials,
    NeedPhone,
    NeedCode,
    /// 2FA
    NeedPassword,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatKind {
    #[default]
    User,
    Bot,
    Group,
    Channel,
    /// The user's own "Saved Messages" chat.
    Saved,
}

/// Online state of a user. Groups/channels/bots are always `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Presence {
    #[default]
    Unknown,
    Online,
    LastSeen(DateTime<Local>),
    Recently,
    LastWeek,
    LastMonth,
    LongAgo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteMode {
    Unmute,
    Forever,
    Hours(u32),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatSummary {
    pub id: i64,
    pub title: String,
    pub kind: ChatKind,
    /// Public @username without the "@"; empty when none.
    pub username: String,
    /// Preview text of the last message (media → "[photo]" etc.).
    pub last_message: String,
    /// "" for 1:1 chats and channels, "You" or the sender's first name in groups.
    pub last_sender: String,
    pub last_time: Option<DateTime<Local>>,
    pub last_msg_id: i32,
    pub last_outgoing: bool,
    pub unread: i32,
    /// Unread messages mentioning me.
    pub mentions: i32,
    /// Manually marked unread (no unread messages, but shows a badge).
    pub unread_mark: bool,
    pub read_inbox_max_id: i32,
    /// The other side has read my messages up to this id (→ ✓✓).
    pub read_outbox_max_id: i32,
    pub pinned: bool,
    pub muted: bool,
    pub archived: bool,
    pub presence: Presence,
    /// `download_avatar` may return a photo. False → initials only.
    pub has_photo: bool,
    /// Server-side draft text (Telegram syncs it between devices).
    pub draft: String,
    /// Supergroup with topics: opening it shows the topic list (wave 6E).
    pub forum: bool,
    /// Story ring around the avatar (wave 6F).
    pub story_ring: StoryRing,
}

impl ChatSummary {
    pub fn identity(&self) -> String {
        let kind = match self.kind {
            ChatKind::User => "Person", ChatKind::Bot => "Bot", ChatKind::Group => "Group",
            ChatKind::Channel => "Channel", ChatKind::Saved => "Saved Messages",
        };
        if self.username.is_empty() { format!("{kind} · {}", self.id) }
        else { format!("@{} · {kind}", self.username) }
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MediaKind {
    Photo,
    Sticker,
    Voice,
    Document,
    Video,
    Gif,
    Audio,
    VideoNote,
    /// `Msg::location` (`live` is None for a plain point).
    Location,
    /// `Msg::location` with title/address.
    Venue,
    /// `Msg::contact`.
    Contact,
    /// `Msg::dice`.
    Dice,
    /// `Msg::poll`.
    Poll,
    /// Games, invoices, stories shared in chats… rendered as a "[unsupported]" card.
    Unsupported,
}

// ===================== wave 6: typed media payloads =====================

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LiveLocation {
    pub period_secs: u32,
    pub expires: DateTime<Local>,
    pub last_update: DateTime<Local>,
    /// Degrees, when the sender's client reports one.
    pub heading: Option<u16>,
    /// Sharing ended (expired or stopped).
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct LocationInfo {
    pub point: GeoPoint,
    /// Venue only.
    pub title: String,
    /// Venue only.
    pub address: String,
    pub live: Option<LiveLocation>,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ContactCard {
    pub first_name: String,
    pub last_name: String,
    pub phone: String,
    /// Bot-API user id when the contact is a Telegram user.
    pub user_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct DiceInfo {
    pub emoji: String,
    /// 0 = still rolling (the final value arrives as MessageChanged).
    pub value: i32,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct PollOption {
    pub text: String,
    pub voters: i32,
    /// I voted for this option.
    pub chosen: bool,
    /// Quiz: known once voted or closed.
    pub correct: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Poll {
    pub id: i64,
    pub question: String,
    pub options: Vec<PollOption>,
    pub total_voters: i32,
    pub closed: bool,
    /// false = anonymous.
    pub public_voters: bool,
    pub multiple_choice: bool,
    pub quiz: bool,
    /// Any option chosen by me.
    pub voted: bool,
    /// Quiz explanation, present once voted or closed.
    pub solution: Option<String>,
    pub close_date: Option<DateTime<Local>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PollDraft {
    pub question: String,
    /// 2..=10 entries.
    pub options: Vec<String>,
    pub anonymous: bool,
    pub multiple_choice: bool,
    pub quiz: bool,
    /// Quiz only.
    pub correct_option: Option<usize>,
    /// Quiz only.
    pub solution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ButtonKind {
    Callback(Vec<u8>),
    Url(String),
    SwitchInline { query: String, same_chat: bool },
    /// Rendered insensitive.
    Other,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KeyButton {
    pub text: String,
    pub kind: ButtonKind,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Keyboard {
    pub rows: Vec<Vec<KeyButton>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BotCommand {
    /// Without the leading "/".
    pub command: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Topic {
    /// Telegram topic id (== id of the topic's first message; 1 = General).
    pub id: i32,
    /// Synthetic chat id that opens this topic (see `topic_chat_id`).
    pub chat_id: i64,
    /// The forum supergroup's chat id.
    pub forum_id: i64,
    pub title: String,
    /// "" when the topic has none.
    pub icon_emoji: String,
    pub unread: i32,
    pub last_message: String,
    pub last_time: Option<DateTime<Local>>,
    pub pinned: bool,
    pub closed: bool,
    pub muted: bool,
    pub draft: String,
    pub draft_reply_to: Option<i32>,
    pub read_outbox_max_id: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoryRing {
    #[default]
    None,
    Unread,
    Read,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoryPeer {
    pub chat_id: i64,
    pub name: String,
    pub unread: bool,
    pub has_photo: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Story {
    pub id: i32,
    pub chat_id: i64,
    pub ts: DateTime<Local>,
    pub expires: DateTime<Local>,
    pub video: bool,
    pub duration: Option<u32>,
    pub caption: String,
    pub seen: bool,
}

/// Any chat id at or below this is a forum topic (see `topic_chat_id`).
pub const TOPIC_CHAT_ID_BASE: i64 = -(1 << 62);
const TOPIC_SHIFT: i64 = 1 << 28;

/// Synthetic chat id for (forum, topic). Everything that takes a chat id
/// accepts it: history, send_*, mark_read, pins, search, drafts, downloads.
/// Limits: forum bare id < 2^34, topic id < 2^28 (both far beyond today's).
pub fn topic_chat_id(forum_id: i64, topic_id: i32) -> i64 {
    let bare = channel_bare_id(forum_id).max(0);
    TOPIC_CHAT_ID_BASE - (bare * TOPIC_SHIFT + topic_id as i64)
}

/// Some((forum_id, topic_id)) when `chat_id` is a topic id.
pub fn split_topic_chat_id(chat_id: i64) -> Option<(i64, i32)> {
    if chat_id > TOPIC_CHAT_ID_BASE {
        return None;
    }
    let offset = TOPIC_CHAT_ID_BASE - chat_id;
    let bare = offset / TOPIC_SHIFT;
    let topic = (offset % TOPIC_SHIFT) as i32;
    Some((-1_000_000_000_000 - bare, topic))
}

/// Bot-API channel/supergroup id → bare id (-100xxxx → xxxx).
fn channel_bare_id(chat_id: i64) -> i64 {
    -chat_id - 1_000_000_000_000
}

/// Does `msg` (from an Event or a history page) belong to the chat the UI
/// has open? Plain ids: `msg.chat_id == open_chat_id`. Topic ids:
/// `msg.chat_id == forum_id && msg.topic_id == Some(topic_id)` (General,
/// topic 1, also matches `topic_id == None`).
pub fn msg_in_chat(msg: &Msg, open_chat_id: i64) -> bool {
    match split_topic_chat_id(open_chat_id) {
        None => msg.chat_id == open_chat_id,
        Some((forum_id, topic_id)) => {
            msg.chat_id == forum_id
                && (msg.topic_id == Some(topic_id) || (topic_id == 1 && msg.topic_id.is_none()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: i32,
    /// I reacted with this emoji.
    pub chosen: bool,
}

/// Text formatting. Offsets are CHAR indices into `Msg::text`, half-open.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SpanKind {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    /// Code block with an optional language.
    Pre(String),
    /// Link with the target url (explicit urls in the text also get one).
    Link(String),
    /// @mention of a user (Bot-API user id).
    Mention(i64),
    Spoiler,
    Blockquote,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: SpanKind,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebPreview {
    pub url: String,
    pub site_name: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Msg {
    pub id: i32,
    pub chat_id: i64,
    /// Title of the chat this message belongs to (may be empty when unknown).
    /// Lets the UI create a sidebar row for a chat outside the loaded dialogs.
    pub chat_title: String,
    /// Display name; empty when unknown, "You" for own messages.
    pub sender: String,
    /// Bot-API id of the sender (a user id), when known.
    pub sender_id: Option<i64>,
    pub text: String,
    pub ts: DateTime<Local>,
    pub outgoing: bool,
    pub media: Option<MediaKind>,
    /// Filename for document-like media.
    pub doc_name: Option<String>,
    /// Id of the replied-to message in the same chat.
    pub reply_to: Option<i32>,
    pub reactions: Vec<Reaction>,
    pub edited: bool,
    /// Deleted on Telegram but kept by the local archive (anti-delete).
    pub deleted: bool,
    /// Formatting of `text`.
    pub spans: Vec<Span>,
    /// `text` with Telegram-style markers (**bold** etc.) — for the edit buffer.
    pub markdown: String,
    pub webpage: Option<WebPreview>,
    /// "Forwarded from" display name.
    pub forwarded_from: Option<String>,
    /// View count (channels).
    pub views: Option<i32>,
    /// Seconds, for voice/audio/video/video notes.
    pub duration: Option<u32>,
    pub doc_size: Option<u64>,
    /// Photo/video dimensions, for pre-sizing.
    pub photo_size: Option<(i32, i32)>,
    pub sticker_emoji: Option<String>,
    /// Pinned in its chat.
    pub pinned: bool,
    // ----- wave 6 -----
    pub location: Option<LocationInfo>,
    pub contact: Option<ContactCard>,
    pub dice: Option<DiceInfo>,
    pub poll: Option<Poll>,
    /// Bot inline keyboard under the message.
    pub keyboard: Option<Keyboard>,
    /// Forum topic this message belongs to (None outside forums; General = Some(1)).
    pub topic_id: Option<i32>,
    /// From `get_scheduled`: `ts` is the scheduled send time, `id` the scheduled id.
    pub scheduled: bool,
    /// Audio: track title.
    pub audio_title: Option<String>,
    /// Audio: artist.
    pub audio_performer: Option<String>,
    /// VideoNote: always true (kept for symmetry with video stickers).
    pub round: bool,
}

impl Default for Msg {
    fn default() -> Self {
        Msg {
            id: 0,
            chat_id: 0,
            chat_title: String::new(),
            sender: String::new(),
            sender_id: None,
            text: String::new(),
            ts: Local::now(),
            outgoing: false,
            media: None,
            doc_name: None,
            reply_to: None,
            reactions: Vec::new(),
            edited: false,
            deleted: false,
            spans: Vec::new(),
            markdown: String::new(),
            webpage: None,
            forwarded_from: None,
            views: None,
            duration: None,
            doc_size: None,
            photo_size: None,
            sticker_emoji: None,
            pinned: false,
            location: None,
            contact: None,
            dice: None,
            poll: None,
            keyboard: None,
            topic_id: None,
            scheduled: false,
            audio_title: None,
            audio_performer: None,
            round: false,
        }
    }
}

/// A previous text of an edited message (edit history).
#[derive(Debug, Clone)]
pub struct MsgVersion {
    pub text: String,
    pub replaced_at: DateTime<Local>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatInfo {
    pub id: i64,
    pub title: String,
    pub kind: ChatKind,
    pub username: String,
    pub phone: String,
    /// Bio (users) or description (groups/channels).
    pub about: String,
    pub members: Option<i32>,
    pub presence: Presence,
    pub has_photo: bool,
    pub muted: bool,
    pub is_contact: bool,
    /// Bots (and groups with bots): the "/" autocomplete list (wave 6E).
    pub bot_commands: Vec<BotCommand>,
    pub forum: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemberRole {
    Creator,
    Admin,
    #[default]
    Member,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Member {
    pub user_id: i64,
    pub name: String,
    pub username: String,
    pub presence: Presence,
    pub role: MemberRole,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contact {
    pub user_id: i64,
    pub name: String,
    pub username: String,
    pub phone: String,
    pub presence: Presence,
    pub has_photo: bool,
    pub story_ring: StoryRing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedKind {
    Photos,
    Files,
    Links,
    Voice,
    Music,
}

/// A Telegram chat folder with its membership already resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Folder {
    pub id: i32,
    pub title: String,
    pub chats: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StickerPack {
    /// "recent", "favorites", or a numeric set id.
    pub id: String,
    pub title: String,
    pub count: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sticker {
    pub id: i64,
    pub emoji: String,
    /// .tgs (Lottie, wave 6D) or .webm (video, wave 6A); `download_sticker`
    /// returns the raw file for these.
    pub animated: bool,
    /// .webm video sticker (`animated` is true as well).
    pub video: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gif {
    pub id: i64,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Me {
    pub id: i64,
    pub name: String,
    pub username: String,
    pub phone: String,
    pub has_photo: bool,
}

/// Behavior flags the UI forwards from settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendFlags {
    /// No read receipts, no online status.
    pub ghost_mode: bool,
    /// get_history merges archived deleted messages (struck through).
    pub anti_delete: bool,
    /// Parse **bold** etc. on send/edit (false = send literally).
    pub markdown_send: bool,
}

impl Default for BackendFlags {
    fn default() -> Self {
        BackendFlags { ghost_mode: false, anti_delete: false, markdown_send: true }
    }
}


// ===================== wave 7: voice calls =====================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    /// Outgoing: request sent, ringing on the other side.
    Requesting,
    /// Incoming: the other side is calling; we are ringing, not yet accepted.
    Incoming,
    /// Keys are being exchanged after accept/confirm.
    Exchanging,
    /// connect_p2p is running; media not yet flowing.
    Connecting,
    /// Connected: audio is flowing.
    Active,
    /// Over; see `CallInfo::end_reason`.
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEndReason {
    /// One side hung up.
    Hangup,
    /// Never answered in time.
    Missed,
    /// The callee was busy or declined.
    Declined,
    /// A transport or crypto failure (logged, never shown raw).
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallInfo {
    /// Telegram phone-call id; 0 before the server assigns one.
    pub id: i64,
    /// The other party (Bot-API user id).
    pub peer_id: i64,
    pub peer_name: String,
    /// True when we placed the call.
    pub outgoing: bool,
    pub phase: CallPhase,
    pub muted: bool,
    /// The 4-emoji key verification (e.g. "🐴🍎🚗🌍"); empty until `Active`.
    pub emojis: String,
    /// When `Active` began (for the timer). None before that.
    pub connected_at: Option<DateTime<Local>>,
    /// Set only in `Ended`.
    pub end_reason: Option<CallEndReason>,
    /// Sanitized actionable failure detail; never carries key material.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallDevice {
    /// Opaque id passed back in settings verbatim.
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallDevices {
    /// Microphones; first entry is the system default.
    pub input: Vec<CallDevice>,
    /// Speakers; first entry is the system default.
    pub output: Vec<CallDevice>,
}

/// Pushed by the backend; read via `Tg::events`. Arrive on whatever context
/// awaits them — in this app, the GLib main context, so widgets may be touched
/// directly in the receive loop.
#[derive(Debug, Clone)]
pub enum Event {
    /// Includes OUTGOING messages sent from the user's other devices/clients
    /// (`msg.outgoing == true`) — the UI merges by id and must not notify or
    /// count unread for those.
    NewMessage(Msg),
    /// Edits and reaction updates to already-displayed messages.
    MessageChanged(Msg),
    /// name may be empty. UI owns the "X is typing" timeout (suggest 5s).
    Typing { chat_id: i64, name: String },
    /// Messages deleted on Telegram. With anti-delete the UI keeps the rows
    /// struck through (msg.deleted); otherwise it removes them.
    MessageDeleted { chat_id: i64, msg_ids: Vec<i32> },
    /// The other side read my messages up to `max_id` (→ ✓✓).
    ReadOutbox { chat_id: i64, max_id: i32 },
    /// I read up to `max_id` on another device (→ drop the unread badge).
    ReadInbox { chat_id: i64, max_id: i32 },
    Presence { user_id: i64, presence: Presence },
    /// Pin/mute/archive/draft/new dialog: reload `get_dialogs` (coalesce 300ms).
    DialogsChanged,
    /// Refetch `get_pinned_message` for this chat.
    PinnedChanged { chat_id: i64 },
    /// A poll's votes/closed state changed (also after my own `send_vote`).
    PollChanged { poll_id: i64, poll: Poll },
    /// The scheduled list of this chat changed: refetch `get_scheduled`.
    ScheduledChanged { chat_id: i64 },
    /// The topic list of this forum changed: refetch `get_topics`.
    TopicsChanged { forum_id: i64 },
    /// Story rings changed: refetch `get_story_peers` (and dialogs' rings).
    StoriesChanged,
    /// The one active/ringing voice call changed (new incoming, phase advanced,
    /// ended). The UI keeps a single call surface; on `Ended` it shows the end
    /// state briefly then clears. No further events for a call after `Ended`.
    CallChanged(CallInfo),
}

/// User-facing error text; show it, don't parse it.
pub type TgError = String;

type Reply<T> = oneshot::Sender<Result<T, TgError>>;

enum Command {
    Shutdown,
    Start(Reply<AuthState>),
    SubmitCredentials { api_id: i32, api_hash: String, respond: Reply<AuthState> },
    SubmitPhone(String, Reply<AuthState>),
    SubmitCode(String, Reply<AuthState>),
    SubmitPassword(String, Reply<AuthState>),
    LogOut(Reply<AuthState>),
    GetMe(Reply<Me>),
    GetDialogs(Reply<Vec<ChatSummary>>),
    GetHistory { chat_id: i64, before_id: Option<i32>, respond: Reply<Vec<Msg>> },
    GetCachedHistory { chat_id: i64, respond: Reply<Vec<Msg>> },
    GetMessages { chat_id: i64, ids: Vec<i32>, respond: Reply<Vec<Msg>> },
    DownloadMedia { chat_id: i64, msg_id: i32, redownload: bool, respond: Reply<Option<PathBuf>> },
    DownloadAvatar { chat_id: i64, big: bool, refresh: bool, respond: Reply<Option<PathBuf>> },
    SendText { chat_id: i64, text: String, reply_to: Option<i32>, respond: Reply<Msg> },
    SendFile { chat_id: i64, path: PathBuf, caption: String, respond: Reply<Msg> },
    SendVoice { chat_id: i64, path: PathBuf, duration: u32, respond: Reply<Msg> },
    SendSticker { chat_id: i64, sticker_id: i64, respond: Reply<Msg> },
    SendGif { chat_id: i64, gif_id: i64, respond: Reply<Msg> },
    EditText { chat_id: i64, msg_id: i32, text: String, respond: Reply<Msg> },
    DeleteMessages { chat_id: i64, ids: Vec<i32>, respond: Reply<()> },
    ForwardMessages { from_chat: i64, ids: Vec<i32>, to_chat: i64, respond: Reply<Vec<Msg>> },
    MarkRead {
        chat_id: i64,
        /// Highest message id the UI has actually shown — nothing newer is
        /// marked read, so a message racing the request stays unread.
        up_to: i32,
        respond: Reply<()>,
    },
    SetFlags(BackendFlags, Reply<()>),
    SetOnline(bool, Reply<()>),
    GetHistoryAtDate { chat_id: i64, date: DateTime<Local>, respond: Reply<Vec<Msg>> },
    GetEditHistory { chat_id: i64, msg_id: i32, respond: Reply<Vec<MsgVersion>> },
    SearchMessages { chat_id: i64, query: String, before_id: Option<i32>, respond: Reply<Vec<Msg>> },
    SearchGlobal { query: String, respond: Reply<Vec<Msg>> },
    SearchChats { query: String, respond: Reply<Vec<ChatSummary>> },
    GetPinnedMessage { chat_id: i64, respond: Reply<Option<Msg>> },
    PinMessage { chat_id: i64, msg_id: i32, pinned: bool, respond: Reply<()> },
    SendReaction { chat_id: i64, msg_id: i32, emoji: Option<String>, respond: Reply<()> },
    GetAvailableReactions(Reply<Vec<String>>),
    SetPinned { chat_id: i64, pinned: bool, respond: Reply<()> },
    SetMuted { chat_id: i64, mode: MuteMode, respond: Reply<()> },
    SetArchived { chat_id: i64, archived: bool, respond: Reply<()> },
    MarkUnread { chat_id: i64, unread: bool, respond: Reply<()> },
    DeleteChat { chat_id: i64, respond: Reply<()> },
    ClearHistory { chat_id: i64, respond: Reply<()> },
    SaveDraft { chat_id: i64, text: String, reply_to: Option<i32>, respond: Reply<()> },
    GetChatInfo { chat_id: i64, respond: Reply<ChatInfo> },
    GetUserProfile { user_id: i64, source: Option<(i64, i32)>, respond: Reply<ChatInfo> },
    GetMembers { chat_id: i64, offset: i32, limit: i32, respond: Reply<Vec<Member>> },
    GetSharedMedia { chat_id: i64, kind: SharedKind, before_id: Option<i32>, respond: Reply<Vec<Msg>> },
    GetContacts(Reply<Vec<Contact>>),
    OpenUser { user_id: i64, respond: Reply<ChatSummary> },
    CreateGroup { title: String, user_ids: Vec<i64>, respond: Reply<ChatSummary> },
    GetFolders(Reply<Vec<Folder>>),
    GetStickerPacks(Reply<Vec<StickerPack>>),
    GetStickers { pack_id: String, respond: Reply<Vec<Sticker>> },
    DownloadSticker { sticker_id: i64, respond: Reply<Option<PathBuf>> },
    GetSavedGifs(Reply<Vec<Gif>>),
    DownloadGif { gif_id: i64, respond: Reply<Option<PathBuf>> },
    // ----- wave 6 -----
    DownloadMap { point: GeoPoint, zoom: u8, width: u32, height: u32, marker: bool, respond: Reply<Option<PathBuf>> },
    SendVote { chat_id: i64, msg_id: i32, options: Vec<usize>, respond: Reply<()> },
    AddContact { user_id: i64, first_name: String, last_name: String, phone: String, respond: Reply<()> },
    SendPoll { chat_id: i64, draft: PollDraft, respond: Reply<Msg> },
    SendLocation { chat_id: i64, point: GeoPoint, respond: Reply<Msg> },
    SendTextAt { chat_id: i64, text: String, reply_to: Option<i32>, at: DateTime<Local>, respond: Reply<()> },
    SendFileAt { chat_id: i64, path: PathBuf, caption: String, at: DateTime<Local>, respond: Reply<()> },
    GetScheduled { chat_id: i64, respond: Reply<Vec<Msg>> },
    SendScheduledNow { chat_id: i64, ids: Vec<i32>, respond: Reply<()> },
    DeleteScheduled { chat_id: i64, ids: Vec<i32>, respond: Reply<()> },
    PressButton { chat_id: i64, msg_id: i32, data: Vec<u8>, respond: Reply<Option<String>> },
    GetTopics { forum_id: i64, respond: Reply<Vec<Topic>> },
    CreateTopic { forum_id: i64, title: String, respond: Reply<Topic> },
    SendVideoNote { chat_id: i64, path: PathBuf, duration: u32, size: u32, respond: Reply<Msg> },
    SendLiveLocation { chat_id: i64, point: GeoPoint, period_secs: u32, respond: Reply<Msg> },
    UpdateLiveLocation { chat_id: i64, msg_id: i32, point: GeoPoint, respond: Reply<()> },
    StopLiveLocation { chat_id: i64, msg_id: i32, respond: Reply<()> },
    GetStoryPeers(Reply<Vec<StoryPeer>>),
    GetStories { chat_id: i64, respond: Reply<Vec<Story>> },
    DownloadStory { chat_id: i64, story_id: i32, respond: Reply<Option<PathBuf>> },
    MarkStoriesSeen { chat_id: i64, up_to_id: i32, respond: Reply<()> },
    // ----- wave 7: voice calls -----
    CallStart { user_id: i64, respond: Reply<()> },
    CallAccept(Reply<()>),
    CallHangUp(Reply<()>),
    CallSetMuted { muted: bool, respond: Reply<()> },
    CallDevicesList(Reply<CallDevices>),
    SearchPlaces { query: String, respond: Reply<Vec<Place>> },
    CallSetDevices { call_id: i64, input: String, output: String, respond: Reply<()> },
    ImportLegacyArchive { account_id: i64, respond: Reply<u64> },
}

impl Command {
    /// Variant name, for the mock's OMG_MOCK_SLOW / OMG_MOCK_FAIL_ONCE hooks.
    fn name(&self) -> &'static str {
        match self {
            Command::Shutdown => "Shutdown",
            Command::Start(_) => "Start",
            Command::SubmitCredentials { .. } => "SubmitCredentials",
            Command::SubmitPhone(..) => "SubmitPhone",
            Command::SubmitCode(..) => "SubmitCode",
            Command::SubmitPassword(..) => "SubmitPassword",
            Command::LogOut(_) => "LogOut",
            Command::GetMe(_) => "GetMe",
            Command::GetDialogs(_) => "GetDialogs",
            Command::GetHistory { .. } => "GetHistory",
            Command::GetCachedHistory { .. } => "GetCachedHistory",
            Command::GetMessages { .. } => "GetMessages",
            Command::DownloadMedia { .. } => "DownloadMedia",
            Command::DownloadAvatar { .. } => "DownloadAvatar",
            Command::SendText { .. } => "SendText",
            Command::SendFile { .. } => "SendFile",
            Command::SendVoice { .. } => "SendVoice",
            Command::SendSticker { .. } => "SendSticker",
            Command::SendGif { .. } => "SendGif",
            Command::EditText { .. } => "EditText",
            Command::DeleteMessages { .. } => "DeleteMessages",
            Command::ForwardMessages { .. } => "ForwardMessages",
            Command::MarkRead { .. } => "MarkRead",
            Command::SetFlags(..) => "SetFlags",
            Command::SetOnline(..) => "SetOnline",
            Command::GetHistoryAtDate { .. } => "GetHistoryAtDate",
            Command::GetEditHistory { .. } => "GetEditHistory",
            Command::SearchMessages { .. } => "SearchMessages",
            Command::SearchGlobal { .. } => "SearchGlobal",
            Command::SearchChats { .. } => "SearchChats",
            Command::GetPinnedMessage { .. } => "GetPinnedMessage",
            Command::PinMessage { .. } => "PinMessage",
            Command::SendReaction { .. } => "SendReaction",
            Command::GetAvailableReactions(_) => "GetAvailableReactions",
            Command::SetPinned { .. } => "SetPinned",
            Command::SetMuted { .. } => "SetMuted",
            Command::SetArchived { .. } => "SetArchived",
            Command::MarkUnread { .. } => "MarkUnread",
            Command::DeleteChat { .. } => "DeleteChat",
            Command::ClearHistory { .. } => "ClearHistory",
            Command::SaveDraft { .. } => "SaveDraft",
            Command::GetChatInfo { .. } => "GetChatInfo",
            Command::GetUserProfile { .. } => "GetUserProfile",
            Command::GetMembers { .. } => "GetMembers",
            Command::GetSharedMedia { .. } => "GetSharedMedia",
            Command::GetContacts(_) => "GetContacts",
            Command::OpenUser { .. } => "OpenUser",
            Command::CreateGroup { .. } => "CreateGroup",
            Command::GetFolders(_) => "GetFolders",
            Command::GetStickerPacks(_) => "GetStickerPacks",
            Command::GetStickers { .. } => "GetStickers",
            Command::DownloadSticker { .. } => "DownloadSticker",
            Command::GetSavedGifs(_) => "GetSavedGifs",
            Command::DownloadGif { .. } => "DownloadGif",
            Command::DownloadMap { .. } => "DownloadMap",
            Command::SendVote { .. } => "SendVote",
            Command::AddContact { .. } => "AddContact",
            Command::SendPoll { .. } => "SendPoll",
            Command::SendLocation { .. } => "SendLocation",
            Command::SendTextAt { .. } => "SendTextAt",
            Command::SendFileAt { .. } => "SendFileAt",
            Command::GetScheduled { .. } => "GetScheduled",
            Command::SendScheduledNow { .. } => "SendScheduledNow",
            Command::DeleteScheduled { .. } => "DeleteScheduled",
            Command::PressButton { .. } => "PressButton",
            Command::GetTopics { .. } => "GetTopics",
            Command::CreateTopic { .. } => "CreateTopic",
            Command::SendVideoNote { .. } => "SendVideoNote",
            Command::SendLiveLocation { .. } => "SendLiveLocation",
            Command::UpdateLiveLocation { .. } => "UpdateLiveLocation",
            Command::StopLiveLocation { .. } => "StopLiveLocation",
            Command::GetStoryPeers(_) => "GetStoryPeers",
            Command::GetStories { .. } => "GetStories",
            Command::DownloadStory { .. } => "DownloadStory",
            Command::MarkStoriesSeen { .. } => "MarkStoriesSeen",
            Command::CallStart { .. } => "CallStart",
            Command::CallAccept(_) => "CallAccept",
            Command::CallHangUp(_) => "CallHangUp",
            Command::CallSetMuted { .. } => "CallSetMuted",
            Command::CallDevicesList(_) => "CallDevicesList",
            Command::SearchPlaces { .. } => "SearchPlaces",
            Command::CallSetDevices { .. } => "CallSetDevices",
            Command::ImportLegacyArchive { .. } => "ImportLegacyArchive",
        }
    }
}

/// Reply to any command with an error (used before connect, for
/// unimplemented commands, and by the mock's failure injection).
fn reject(cmd: Command, e: &str) {
    let e = e.to_string();
    match cmd {
        Command::Shutdown => {},
        Command::Start(tx) | Command::SubmitPhone(_, tx) | Command::SubmitCode(_, tx) | Command::SubmitPassword(_, tx) | Command::LogOut(tx) => {
            drop(tx.send(Err(e)))
        }
        Command::SubmitCredentials { respond, .. } => drop(respond.send(Err(e))),
        Command::GetMe(tx) => drop(tx.send(Err(e))),
        Command::GetDialogs(tx) => drop(tx.send(Err(e))),
        Command::GetHistory { respond, .. }
        | Command::GetCachedHistory { respond, .. }
        | Command::GetMessages { respond, .. }
        | Command::GetHistoryAtDate { respond, .. }
        | Command::SearchMessages { respond, .. }
        | Command::SearchGlobal { respond, .. }
        | Command::ForwardMessages { respond, .. }
        | Command::GetSharedMedia { respond, .. } => drop(respond.send(Err(e))),
        Command::DownloadMedia { respond, .. }
        | Command::DownloadAvatar { respond, .. }
        | Command::DownloadSticker { respond, .. }
        | Command::DownloadGif { respond, .. } => drop(respond.send(Err(e))),
        Command::SendText { respond, .. }
        | Command::SendFile { respond, .. }
        | Command::SendVoice { respond, .. }
        | Command::SendSticker { respond, .. }
        | Command::SendGif { respond, .. }
        | Command::EditText { respond, .. } => drop(respond.send(Err(e))),
        Command::DeleteMessages { respond, .. }
        | Command::MarkRead { respond, .. }
        | Command::PinMessage { respond, .. }
        | Command::SendReaction { respond, .. }
        | Command::SetPinned { respond, .. }
        | Command::SetMuted { respond, .. }
        | Command::SetArchived { respond, .. }
        | Command::MarkUnread { respond, .. }
        | Command::DeleteChat { respond, .. }
        | Command::ClearHistory { respond, .. }
        | Command::SaveDraft { respond, .. } => drop(respond.send(Err(e))),
        Command::SetFlags(_, tx) | Command::SetOnline(_, tx) => drop(tx.send(Err(e))),
        Command::GetEditHistory { respond, .. } => drop(respond.send(Err(e))),
        Command::GetPinnedMessage { respond, .. } => drop(respond.send(Err(e))),
        Command::GetAvailableReactions(tx) => drop(tx.send(Err(e))),
        Command::SearchChats { respond, .. } => drop(respond.send(Err(e))),
        Command::GetChatInfo { respond, .. } | Command::GetUserProfile { respond, .. } => drop(respond.send(Err(e))),
        Command::GetMembers { respond, .. } => drop(respond.send(Err(e))),
        Command::GetContacts(tx) => drop(tx.send(Err(e))),
        Command::OpenUser { respond, .. } | Command::CreateGroup { respond, .. } => drop(respond.send(Err(e))),
        Command::GetFolders(tx) => drop(tx.send(Err(e))),
        Command::GetStickerPacks(tx) => drop(tx.send(Err(e))),
        Command::GetStickers { respond, .. } => drop(respond.send(Err(e))),
        Command::GetSavedGifs(tx) => drop(tx.send(Err(e))),
        Command::DownloadMap { respond, .. } | Command::DownloadStory { respond, .. } => drop(respond.send(Err(e))),
        Command::SendVote { respond, .. }
        | Command::AddContact { respond, .. }
        | Command::SendTextAt { respond, .. }
        | Command::SendFileAt { respond, .. }
        | Command::SendScheduledNow { respond, .. }
        | Command::DeleteScheduled { respond, .. }
        | Command::UpdateLiveLocation { respond, .. }
        | Command::StopLiveLocation { respond, .. }
        | Command::MarkStoriesSeen { respond, .. }
        | Command::CallStart { respond, .. }
        | Command::CallSetMuted { respond, .. } => drop(respond.send(Err(e))),
        Command::CallAccept(tx) | Command::CallHangUp(tx) => drop(tx.send(Err(e))),
        Command::CallDevicesList(tx) => drop(tx.send(Err(e))),
        Command::SearchPlaces { respond, .. } => drop(respond.send(Err(e))),
        Command::CallSetDevices { respond, .. } => drop(respond.send(Err(e))),
        Command::ImportLegacyArchive { respond, .. } => drop(respond.send(Err(e))),
        Command::SendPoll { respond, .. }
        | Command::SendLocation { respond, .. }
        | Command::SendVideoNote { respond, .. }
        | Command::SendLiveLocation { respond, .. } => drop(respond.send(Err(e))),
        Command::GetScheduled { respond, .. } => drop(respond.send(Err(e))),
        Command::PressButton { respond, .. } => drop(respond.send(Err(e))),
        Command::GetTopics { respond, .. } => drop(respond.send(Err(e))),
        Command::CreateTopic { respond, .. } => drop(respond.send(Err(e))),
        Command::GetStoryPeers(tx) => drop(tx.send(Err(e))),
        Command::GetStories { respond, .. } => drop(respond.send(Err(e))),
    }
}

/// UI-side handle to the backend thread. Cheap to clone.
#[derive(Clone)]
pub struct Tg {
    cmds: mpsc::UnboundedSender<Command>,
    stopped: async_channel::Receiver<()>,
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
        let (stopped_tx, stopped_rx) = async_channel::bounded::<()>(1);
        std::thread::spawn(move || {
            // Close only after every runtime task and native service is gone.
            // No values are sent: all cloned receivers observe the same close.
            let _stopped = stopped_tx;
            // Wave 8B: a personal 1:1 client is network-bound, not CPU-bound —
            // 2 workers instead of one-per-core (was 16 here) saves ~14 idle
            // threads with no throughput loss (data commands still spawn freely).
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            if mock {
                rt.block_on(mock::run(cmd_rx, event_tx));
            } else {
                rt.block_on(real::run(cmd_rx, event_tx));
            }
        });
        Tg {
            cmds: cmd_tx,
            stopped: stopped_rx,
            events: event_rx,
            is_mock: mock,
        }
    }

    // ----- auth -----

    /// Stop the backend and wait for its runtime and native call service to
    /// finish. Preserves the authorized session; safe to call from clones.
    pub async fn shutdown(&self) {
        let _ = self.cmds.send(Command::Shutdown);
        let _ = self.stopped.recv().await;
    }

    pub async fn start(&self) -> Result<AuthState, TgError> {
        roundtrip!(self, Command::Start)
    }

    /// Stores the Telegram API credentials (config.toml, 0600) and continues
    /// the login. Values are never logged.
    pub async fn submit_credentials(&self, api_id: i32, api_hash: &str) -> Result<AuthState, TgError> {
        let api_hash = api_hash.trim().to_string();
        roundtrip!(self, |tx| Command::SubmitCredentials { api_id, api_hash, respond: tx })
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

    /// Signs out and deletes the local session; returns `NeedPhone`.
    pub async fn log_out(&self) -> Result<AuthState, TgError> {
        roundtrip!(self, Command::LogOut)
    }

    pub async fn get_me(&self) -> Result<Me, TgError> {
        roundtrip!(self, Command::GetMe)
    }

    // ----- dialogs & history -----

    /// All dialogs, pinned first then newest first; archived ones are
    /// included and flagged.
    pub async fn get_dialogs(&self) -> Result<Vec<ChatSummary>, TgError> {
        roundtrip!(self, Command::GetDialogs)
    }

    /// Newest last (display order). `before_id` pages older messages; an empty
    /// page means there is nothing older.
    pub async fn get_history(&self, chat_id: i64, before_id: Option<i32>) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |tx| Command::GetHistory { chat_id, before_id, respond: tx })
    }

    /// A bounded local snapshot for immediate display while get_history
    /// refreshes. Empty on a miss; never performs a Telegram request.
    pub async fn get_cached_history(&self, chat_id: i64) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |respond| Command::GetCachedHistory { chat_id, respond })
    }

    /// Specific messages by id (reply quotes, jump targets). Missing ids are
    /// simply absent from the result.
    pub async fn get_messages(&self, chat_id: i64, ids: Vec<i32>) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |tx| Command::GetMessages { chat_id, ids, respond: tx })
    }

    /// Up to 50 messages at or before `date`, newest last (jump-to-date).
    /// Empty when the chat has nothing that old.
    pub async fn get_history_at_date(&self, chat_id: i64, date: DateTime<Local>) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |tx| Command::GetHistoryAtDate { chat_id, date, respond: tx })
    }

    /// Previous texts of an edited message, oldest first. Empty when the
    /// archive never saw an earlier version.
    pub async fn get_edit_history(&self, chat_id: i64, msg_id: i32) -> Result<Vec<MsgVersion>, TgError> {
        roundtrip!(self, |tx| Command::GetEditHistory { chat_id, msg_id, respond: tx })
    }

    // ----- media -----

    /// Downloads to the media cache and returns a GTK-renderable path
    /// (stickers are converted webp -> png). Cached across calls.
    /// None when the message has no media or it cannot be rendered.
    pub async fn download_media(&self, chat_id: i64, msg_id: i32) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadMedia { chat_id, msg_id, redownload: false, respond: tx })
    }

    /// Explicit recovery from a corrupt cached file or failed playback.
    pub async fn retry_media(&self, chat_id: i64, msg_id: i32) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadMedia { chat_id, msg_id, redownload: true, respond: tx })
    }

    /// Small profile photo of a chat or user (Bot-API id), cached. None when
    /// there is no photo — render initials.
    pub async fn download_avatar(&self, chat_id: i64) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadAvatar { chat_id, big: false, refresh: false, respond: tx })
    }

    /// Full-size profile photo, stored separately from list thumbnails.
    pub async fn download_profile_photo(&self, chat_id: i64) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadAvatar { chat_id, big: true, refresh: false, respond: tx })
    }

    pub async fn retry_profile_photo(&self, chat_id: i64) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |respond| Command::DownloadAvatar { chat_id, big: true, refresh: true, respond })
    }

    // ----- sending -----

    /// `text` may contain Telegram-style markers: **bold**, __italic__,
    /// ~~strike~~, `code`, ```pre```, ||spoiler||, [text](url). Plain text
    /// without markers is sent unchanged.
    pub async fn send_text(&self, chat_id: i64, text: &str, reply_to: Option<i32>) -> Result<Msg, TgError> {
        let text = text.to_string();
        roundtrip!(self, |tx| Command::SendText { chat_id, text, reply_to, respond: tx })
    }

    pub async fn send_file(&self, chat_id: i64, path: PathBuf, caption: &str) -> Result<Msg, TgError> {
        let caption = caption.to_string();
        roundtrip!(self, |tx| Command::SendFile { chat_id, path, caption, respond: tx })
    }

    /// OGG/Opus voice note (see `Local::record_*`).
    pub async fn send_voice(&self, chat_id: i64, path: PathBuf, duration: u32) -> Result<Msg, TgError> {
        roundtrip!(self, |tx| Command::SendVoice { chat_id, path, duration, respond: tx })
    }

    pub async fn send_sticker(&self, chat_id: i64, sticker_id: i64) -> Result<Msg, TgError> {
        roundtrip!(self, |tx| Command::SendSticker { chat_id, sticker_id, respond: tx })
    }

    pub async fn send_gif(&self, chat_id: i64, gif_id: i64) -> Result<Msg, TgError> {
        roundtrip!(self, |tx| Command::SendGif { chat_id, gif_id, respond: tx })
    }

    /// Same markdown rules as `send_text`.
    pub async fn edit_text(&self, chat_id: i64, msg_id: i32, text: &str) -> Result<Msg, TgError> {
        let text = text.to_string();
        roundtrip!(self, |tx| Command::EditText { chat_id, msg_id, text, respond: tx })
    }

    pub async fn delete_message(&self, chat_id: i64, msg_id: i32) -> Result<(), TgError> {
        self.delete_messages(chat_id, vec![msg_id]).await
    }

    pub async fn delete_messages(&self, chat_id: i64, ids: Vec<i32>) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::DeleteMessages { chat_id, ids, respond: tx })
    }

    /// Returns the forwarded copies as they appear in `to_chat`.
    pub async fn forward_messages(&self, from_chat: i64, ids: Vec<i32>, to_chat: i64) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |tx| Command::ForwardMessages { from_chat, ids, to_chat, respond: tx })
    }

    /// Marks messages up to and including `up_to` as read (never newer ones).
    pub async fn mark_read(&self, chat_id: i64, up_to: i32) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::MarkRead { chat_id, up_to, respond: tx })
    }

    /// Forward ghost-mode / anti-delete from settings. Call at startup and
    /// whenever settings change; idempotent.
    pub async fn set_flags(&self, flags: BackendFlags) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SetFlags(flags, tx))
    }

    /// App activity controls presence independently of the network connection.
    /// Ghost mode always overrides an online request.
    pub async fn set_online(&self, online: bool) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SetOnline(online, tx))
    }

    // ----- search -----

    /// Newest first, 50 per page; `before_id` pages older hits.
    pub async fn search_messages(&self, chat_id: i64, query: &str, before_id: Option<i32>) -> Result<Vec<Msg>, TgError> {
        let query = query.to_string();
        roundtrip!(self, |tx| Command::SearchMessages { chat_id, query, before_id, respond: tx })
    }

    /// Newest first, up to 50, across all chats.
    pub async fn search_global(&self, query: &str) -> Result<Vec<Msg>, TgError> {
        let query = query.to_string();
        roundtrip!(self, |tx| Command::SearchGlobal { query, respond: tx })
    }

    /// Chats/users matching a name or @username that are NOT necessarily in
    /// the dialog list (contacts, public usernames). The UI filters loaded
    /// dialogs itself.
    pub async fn search_chats(&self, query: &str) -> Result<Vec<ChatSummary>, TgError> {
        let query = query.to_string();
        roundtrip!(self, |tx| Command::SearchChats { query, respond: tx })
    }

    // ----- pins & reactions -----

    pub async fn get_pinned_message(&self, chat_id: i64) -> Result<Option<Msg>, TgError> {
        roundtrip!(self, |tx| Command::GetPinnedMessage { chat_id, respond: tx })
    }

    pub async fn pin_message(&self, chat_id: i64, msg_id: i32, pinned: bool) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::PinMessage { chat_id, msg_id, pinned, respond: tx })
    }

    /// `None` removes my reaction. The updated message arrives as `MessageChanged`.
    pub async fn send_reaction(&self, chat_id: i64, msg_id: i32, emoji: Option<String>) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SendReaction { chat_id, msg_id, emoji, respond: tx })
    }

    /// Emoji the account may react with, most common first.
    pub async fn get_available_reactions(&self) -> Result<Vec<String>, TgError> {
        roundtrip!(self, Command::GetAvailableReactions)
    }

    // ----- chat actions (each is followed by Event::DialogsChanged) -----

    pub async fn set_pinned(&self, chat_id: i64, pinned: bool) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SetPinned { chat_id, pinned, respond: tx })
    }

    pub async fn set_muted(&self, chat_id: i64, mode: MuteMode) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SetMuted { chat_id, mode, respond: tx })
    }

    pub async fn set_archived(&self, chat_id: i64, archived: bool) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SetArchived { chat_id, archived, respond: tx })
    }

    pub async fn mark_unread(&self, chat_id: i64, unread: bool) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::MarkUnread { chat_id, unread, respond: tx })
    }

    /// Deletes the dialog (leaves groups/channels).
    pub async fn delete_chat(&self, chat_id: i64) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::DeleteChat { chat_id, respond: tx })
    }

    pub async fn clear_history(&self, chat_id: i64) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::ClearHistory { chat_id, respond: tx })
    }

    /// Empty text clears the draft. The UI debounces (rule D4).
    pub async fn save_draft(&self, chat_id: i64, text: &str, reply_to: Option<i32>) -> Result<(), TgError> {
        let text = text.to_string();
        roundtrip!(self, |tx| Command::SaveDraft { chat_id, text, reply_to, respond: tx })
    }

    // ----- info -----

    pub async fn get_user_profile(&self, user_id: i64, source: Option<(i64, i32)>) -> Result<ChatInfo, TgError> {
        roundtrip!(self, |respond| Command::GetUserProfile { user_id, source, respond })
    }

    pub async fn get_chat_info(&self, chat_id: i64) -> Result<ChatInfo, TgError> {
        roundtrip!(self, |tx| Command::GetChatInfo { chat_id, respond: tx })
    }

    pub async fn get_members(&self, chat_id: i64, offset: i32, limit: i32) -> Result<Vec<Member>, TgError> {
        roundtrip!(self, |tx| Command::GetMembers { chat_id, offset, limit, respond: tx })
    }

    /// Newest first, 50 per page.
    pub async fn get_shared_media(&self, chat_id: i64, kind: SharedKind, before_id: Option<i32>) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |tx| Command::GetSharedMedia { chat_id, kind, before_id, respond: tx })
    }

    pub async fn get_contacts(&self) -> Result<Vec<Contact>, TgError> {
        roundtrip!(self, Command::GetContacts)
    }

    /// Opens (or creates) the 1:1 chat with a user; `me.id` opens Saved Messages.
    pub async fn open_user(&self, user_id: i64) -> Result<ChatSummary, TgError> {
        roundtrip!(self, |tx| Command::OpenUser { user_id, respond: tx })
    }

    pub async fn create_group(&self, title: &str, user_ids: Vec<i64>) -> Result<ChatSummary, TgError> {
        let title = title.trim().to_string();
        roundtrip!(self, |tx| Command::CreateGroup { title, user_ids, respond: tx })
    }

    pub async fn get_folders(&self) -> Result<Vec<Folder>, TgError> {
        roundtrip!(self, Command::GetFolders)
    }

    // ----- stickers & gifs -----

    pub async fn get_sticker_packs(&self) -> Result<Vec<StickerPack>, TgError> {
        roundtrip!(self, Command::GetStickerPacks)
    }

    pub async fn get_stickers(&self, pack_id: &str) -> Result<Vec<Sticker>, TgError> {
        let pack_id = pack_id.to_string();
        roundtrip!(self, |tx| Command::GetStickers { pack_id, respond: tx })
    }

    /// PNG path (webp converted), cached. None for animated stickers.
    pub async fn download_sticker(&self, sticker_id: i64) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadSticker { sticker_id, respond: tx })
    }

    pub async fn get_saved_gifs(&self) -> Result<Vec<Gif>, TgError> {
        roundtrip!(self, Command::GetSavedGifs)
    }

    /// MP4 path, cached. None when unavailable.
    pub async fn download_gif(&self, gif_id: i64) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadGif { gif_id, respond: tx })
    }

    // ----- wave 6B: display -----

    /// PNG of an OpenStreetMap map centered on `point`, `width`×`height` px,
    /// cached. Mock: a synthetic tile. `Ok(None)` when offline or the fetch
    /// fails — never an error for a bad network.
    pub async fn download_map(&self, point: GeoPoint, zoom: u8, width: u32, height: u32) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadMap { point, zoom, width, height, marker: true, respond: tx })
    }

    /// Same map without the center marker — for grids of tiles (the location
    /// dialog draws one marker itself).
    pub async fn download_map_tile(&self, point: GeoPoint, zoom: u8, width: u32, height: u32) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadMap { point, zoom, width, height, marker: false, respond: tx })
    }

    /// Empty `options` retracts. The new state arrives as `Event::PollChanged`.
    pub async fn send_vote(&self, chat_id: i64, msg_id: i32, options: Vec<usize>) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SendVote { chat_id, msg_id, options, respond: tx })
    }

    pub async fn add_contact(&self, user_id: i64, first_name: &str, last_name: &str, phone: &str) -> Result<(), TgError> {
        let first_name = first_name.trim().to_string();
        let last_name = last_name.trim().to_string();
        let phone = phone.trim().to_string();
        roundtrip!(self, |tx| Command::AddContact { user_id, first_name, last_name, phone, respond: tx })
    }

    // ----- wave 6C: sending -----

    pub async fn send_poll(&self, chat_id: i64, draft: PollDraft) -> Result<Msg, TgError> {
        roundtrip!(self, |tx| Command::SendPoll { chat_id, draft, respond: tx })
    }

    pub async fn send_location(&self, chat_id: i64, point: GeoPoint) -> Result<Msg, TgError> {
        roundtrip!(self, |tx| Command::SendLocation { chat_id, point, respond: tx })
    }

    /// Scheduled sends return nothing: the message shows up in
    /// `get_scheduled` and `Event::ScheduledChanged` fires.
    pub async fn send_text_at(&self, chat_id: i64, text: &str, reply_to: Option<i32>, at: DateTime<Local>) -> Result<(), TgError> {
        let text = text.to_string();
        roundtrip!(self, |tx| Command::SendTextAt { chat_id, text, reply_to, at, respond: tx })
    }

    pub async fn send_file_at(&self, chat_id: i64, path: PathBuf, caption: &str, at: DateTime<Local>) -> Result<(), TgError> {
        let caption = caption.to_string();
        roundtrip!(self, |tx| Command::SendFileAt { chat_id, path, caption, at, respond: tx })
    }

    /// Soonest first. `Msg::scheduled == true`, `ts` = send time, `id` = the
    /// scheduled id (valid only for the two calls below).
    pub async fn get_scheduled(&self, chat_id: i64) -> Result<Vec<Msg>, TgError> {
        roundtrip!(self, |tx| Command::GetScheduled { chat_id, respond: tx })
    }

    pub async fn send_scheduled_now(&self, chat_id: i64, ids: Vec<i32>) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::SendScheduledNow { chat_id, ids, respond: tx })
    }

    pub async fn delete_scheduled(&self, chat_id: i64, ids: Vec<i32>) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::DeleteScheduled { chat_id, ids, respond: tx })
    }

    // ----- wave 6E: bots and forums -----

    /// Presses a Callback button. `Some(text)` = the bot answered with a
    /// toast/alert text to show; `None` = silent ack. Url buttons are opened
    /// by the UI.
    pub async fn press_button(&self, chat_id: i64, msg_id: i32, data: Vec<u8>) -> Result<Option<String>, TgError> {
        roundtrip!(self, |tx| Command::PressButton { chat_id, msg_id, data, respond: tx })
    }

    /// Topics of a forum supergroup, pinned first then newest activity first.
    pub async fn get_topics(&self, forum_id: i64) -> Result<Vec<Topic>, TgError> {
        roundtrip!(self, |tx| Command::GetTopics { forum_id, respond: tx })
    }

    pub async fn create_topic(&self, forum_id: i64, title: &str) -> Result<Topic, TgError> {
        let title = title.trim().to_string();
        roundtrip!(self, |tx| Command::CreateTopic { forum_id, title, respond: tx })
    }

    // ----- wave 6F: capture and live features -----

    /// MP4 (h264, square `size` px) video circle.
    pub async fn send_video_note(&self, chat_id: i64, path: PathBuf, duration: u32, size: u32) -> Result<Msg, TgError> {
        roundtrip!(self, |tx| Command::SendVideoNote { chat_id, path, duration, size, respond: tx })
    }

    pub async fn send_live_location(&self, chat_id: i64, point: GeoPoint, period_secs: u32) -> Result<Msg, TgError> {
        roundtrip!(self, |tx| Command::SendLiveLocation { chat_id, point, period_secs, respond: tx })
    }

    pub async fn update_live_location(&self, chat_id: i64, msg_id: i32, point: GeoPoint) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::UpdateLiveLocation { chat_id, msg_id, point, respond: tx })
    }

    pub async fn stop_live_location(&self, chat_id: i64, msg_id: i32) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::StopLiveLocation { chat_id, msg_id, respond: tx })
    }

    /// Peers with active stories, unread first.
    pub async fn get_story_peers(&self) -> Result<Vec<StoryPeer>, TgError> {
        roundtrip!(self, Command::GetStoryPeers)
    }

    /// Oldest first.
    pub async fn get_stories(&self, chat_id: i64) -> Result<Vec<Story>, TgError> {
        roundtrip!(self, |tx| Command::GetStories { chat_id, respond: tx })
    }

    /// JPG or MP4 path, cached. None when unavailable.
    pub async fn download_story(&self, chat_id: i64, story_id: i32) -> Result<Option<PathBuf>, TgError> {
        roundtrip!(self, |tx| Command::DownloadStory { chat_id, story_id, respond: tx })
    }

    pub async fn mark_stories_seen(&self, chat_id: i64, up_to_id: i32) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::MarkStoriesSeen { chat_id, up_to_id, respond: tx })
    }

    // ----- wave 7: voice calls -----

    /// Place a 1:1 voice call to a user (Bot-API id). Progress arrives as
    /// `Event::CallChanged`. Fails if a call is active, the peer is not a user,
    /// or calls are not built in.
    pub async fn call_start(&self, user_id: i64) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::CallStart { user_id, respond: tx })
    }

    /// Accept the current incoming call.
    pub async fn call_accept(&self) -> Result<(), TgError> {
        roundtrip!(self, Command::CallAccept)
    }

    /// Decline an incoming call, or hang up an outgoing/active one. Idempotent.
    pub async fn call_hang_up(&self) -> Result<(), TgError> {
        roundtrip!(self, Command::CallHangUp)
    }

    pub async fn call_set_muted(&self, muted: bool) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::CallSetMuted { muted, respond: tx })
    }

    /// Audio devices for the settings page; the first entry of each list is
    /// the system default.
    pub async fn import_legacy_archive(&self, account_id: i64) -> Result<u64, TgError> {
        roundtrip!(self, |tx| Command::ImportLegacyArchive { account_id, respond: tx })
    }

    pub async fn call_set_devices(&self, call_id: i64, input: String, output: String) -> Result<(), TgError> {
        roundtrip!(self, |tx| Command::CallSetDevices { call_id, input, output, respond: tx })
    }

    pub async fn search_places(&self, query: &str) -> Result<Vec<Place>, TgError> {
        roundtrip!(self, |tx| Command::SearchPlaces { query: query.into(), respond: tx })
    }

    pub async fn call_devices(&self) -> Result<CallDevices, TgError> {
        roundtrip!(self, Command::CallDevicesList)
    }
}

#[cfg(test)]
mod topic_id_tests {
    use super::*;

    #[test]
    fn topic_ids_round_trip() {
        let forum = -1_002_345_678_901;
        for topic in [1, 7, 4_000_000] {
            let id = topic_chat_id(forum, topic);
            assert!(id <= TOPIC_CHAT_ID_BASE);
            assert_eq!(split_topic_chat_id(id), Some((forum, topic)));
        }
        assert_eq!(split_topic_chat_id(forum), None);
        assert_eq!(split_topic_chat_id(42), None);
    }

    #[test]
    fn msg_in_chat_routes_topics() {
        let forum = -1_001_000_000_001;
        let open = topic_chat_id(forum, 5);
        let mut m = Msg { chat_id: forum, topic_id: Some(5), ..Msg::default() };
        assert!(msg_in_chat(&m, open));
        m.topic_id = Some(6);
        assert!(!msg_in_chat(&m, open));
        m.topic_id = None;
        assert!(msg_in_chat(&m, topic_chat_id(forum, 1)));
        assert!(!msg_in_chat(&m, open));
        assert!(msg_in_chat(&m, forum));
        m.chat_id = 9;
        assert!(!msg_in_chat(&m, forum));
    }
}
