# Wave 6 — Telegram media parity

Decided 2026-09-03 (orchestrator). Scope: 6A in-app playback, 6B remaining
media kinds (display), 6C sending (polls, location, scheduled), 6D animated
stickers, 6E bots and forums, 6F capture and live features, 6G calls verdict.
Secret chats are out.

This spec is the design authority. Workers implement it verbatim; where it is
silent, match Telegram Desktop's behavior; when unsure, plainer wins. The
wave-5 design direction (specs/spec-wave5.md §0) still applies: Telegram
Desktop layout, Omarchy skin, theme tokens only, Nerd Font glyphs from
`src/ui/icons.rs` only (never emoji in chrome — emoji are allowed only where
they ARE content: dice, sticker emoji, topic icons, reactions), 8px grid, 6px
radius, "JetBrainsMono Nerd Font".

Rules for every package (binding):

- Touch only the files your package lists. Backend (`src/tg/`, `src/local/`,
  `src/main.rs`, `src/lib.rs`, `Cargo.toml`) is orchestrator-owned; if the
  contract in §1 is missing something you need, STOP and report it — never
  work around it.
- New code goes in the new module(s) your package names. Edits to shared
  files (`src/ui/messages.rs`, `src/ui/shell.rs`, `src/ui/chatlist.rs`,
  `src/theme/style.css`, `src/ui/mod.rs`) are limited to the hook points the
  package lists, kept as small as possible (other packages edit the same
  files in parallel; merges are the orchestrator's problem, but keep them
  small). CSS additions go at the END of `src/theme/style.css` inside one
  block delimited by `/* ---- wave 6X ---- */` … `/* ---- end 6X ---- */`.
- Only `var(--…)` tokens for colors. No new fonts. No shadows, gradients,
  emoji glyphs in chrome. Icons: add missing glyphs to `src/ui/icons.rs` only
  if §1.9 does not already provide them.
- The mock backend (`--smoke`) carries fixtures for every feature (§1.10);
  the `--probe` traversal (`src/ui/shell.rs::run_probe`) MUST be extended
  with steps that exercise your package on those fixtures (§1.11), and the
  HARD acceptance gate in CLAUDE.md must pass (`G_DEBUG=fatal-criticals`
  probe ×6, `OMG_MOCK_AUTH=1` ×3, `OMG_MOCK_LATENCY_MS=400` ×1, plus the two
  uistate seeds), all through `bin/headless`.
- No panics on missing system pieces: a missing GStreamer plugin, a missing
  ffmpeg, a missing camera, a failed network fetch → an inline error label
  (`omg-error`) in the card/dialog, never a crash, never a blocked UI.
- Do not commit.

---

## 1. Backend contract (orchestrator implements; UI compiles against this)

All types live in `src/tg/mod.rs`. Everything below EXISTS on `main` when
the UI packages start (commit "Wave 6 contract"); mock fixtures and the real
implementation are the orchestrator's.

### 1.1 MediaKind

```rust
pub enum MediaKind {
    Photo, Sticker, Voice, Document, Video, Gif, Audio, VideoNote,
    Location,   // Msg.location (live == None)
    Venue,      // Msg.location with title/address
    Contact,    // Msg.contact
    Dice,       // Msg.dice
    Poll,       // Msg.poll
    Unsupported,
}
```

`MediaKind` stays `Copy`. Typed payloads are new `Msg` fields (all
`None`/`false` by default):

```rust
pub struct GeoPoint { pub lat: f64, pub lon: f64 }

pub struct LiveLocation {
    pub period_secs: u32,
    pub expires: DateTime<Local>,
    pub last_update: DateTime<Local>,
    pub heading: Option<u16>,   // degrees, when the sender's client reports one
    pub stopped: bool,          // sharing ended (expired or stopped)
}

pub struct LocationInfo {
    pub point: GeoPoint,
    pub title: String,          // venue only
    pub address: String,        // venue only
    pub live: Option<LiveLocation>,
}

pub struct ContactCard {
    pub first_name: String,
    pub last_name: String,
    pub phone: String,
    pub user_id: Option<i64>,   // Bot-API user id when the contact is a Telegram user
}

pub struct DiceInfo { pub emoji: String, pub value: i32 }   // value 0 = still rolling

pub struct PollOption {
    pub text: String,
    pub voters: i32,
    pub chosen: bool,           // I voted for this option
    pub correct: Option<bool>,  // quiz, known once voted/closed
}

pub struct Poll {
    pub id: i64,
    pub question: String,
    pub options: Vec<PollOption>,
    pub total_voters: i32,
    pub closed: bool,
    pub public_voters: bool,    // false = anonymous
    pub multiple_choice: bool,
    pub quiz: bool,
    pub voted: bool,            // any option chosen
    pub solution: Option<String>,   // quiz explanation, present once voted/closed
    pub close_date: Option<DateTime<Local>>,
}

pub struct PollDraft {
    pub question: String,
    pub options: Vec<String>,   // 2..=10
    pub anonymous: bool,
    pub multiple_choice: bool,
    pub quiz: bool,
    pub correct_option: Option<usize>,  // quiz only
    pub solution: Option<String>,       // quiz only
}

pub enum ButtonKind {
    Callback(Vec<u8>),
    Url(String),
    SwitchInline { query: String, same_chat: bool },
    Other,                      // rendered disabled
}
pub struct KeyButton { pub text: String, pub kind: ButtonKind }
pub struct Keyboard { pub rows: Vec<Vec<KeyButton>> }

pub struct BotCommand { pub command: String, pub description: String }  // command without "/"

pub struct Topic {
    pub id: i32,                // Telegram topic id (== id of the topic's first message; 1 = General)
    pub chat_id: i64,           // synthetic chat id to open this topic (see 1.4)
    pub forum_id: i64,          // the forum supergroup's chat id
    pub title: String,
    pub icon_emoji: String,     // "" when the topic has none
    pub unread: i32,
    pub last_message: String,
    pub last_time: Option<DateTime<Local>>,
    pub pinned: bool,
    pub closed: bool,
}

pub enum StoryRing { None, Unread, Read }

pub struct StoryPeer { pub chat_id: i64, pub name: String, pub unread: bool, pub has_photo: bool }

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
```

### 1.2 Msg / ChatSummary / ChatInfo / Contact / Sticker additions

```rust
// Msg
pub location: Option<LocationInfo>,
pub contact: Option<ContactCard>,
pub dice: Option<DiceInfo>,
pub poll: Option<Poll>,
pub keyboard: Option<Keyboard>,   // bot inline keyboard under the message
pub topic_id: Option<i32>,        // forum topic this message belongs to (None in non-forums; General = Some(1))
pub scheduled: bool,              // from get_scheduled: ts == scheduled send time
pub audio_title: Option<String>,      // Audio: track title
pub audio_performer: Option<String>,  // Audio: artist
pub round: bool,                  // VideoNote: always true (kept for symmetry)

// ChatSummary
pub forum: bool,                  // supergroup with topics: opening it shows the topic list (6E)
pub story_ring: StoryRing,        // 6F

// ChatInfo
pub bot_commands: Vec<BotCommand>,
pub forum: bool,

// Contact
pub story_ring: StoryRing,

// Sticker
pub video: bool,                  // .webm video sticker (animated == true as well)
```

### 1.3 Tg methods (new)

```rust
// 6B — display
/// PNG of an OpenStreetMap map centered on `point`, `width`×`height` px, cached.
/// Mock: draws a synthetic tile. None when offline. Never errors for a bad
/// network — that is `Ok(None)`.
pub async fn download_map(&self, point: GeoPoint, zoom: u8, width: u32, height: u32) -> Result<Option<PathBuf>, TgError>;
/// Empty `options` retracts. The new state arrives as Event::PollChanged.
pub async fn send_vote(&self, chat_id: i64, msg_id: i32, options: Vec<usize>) -> Result<(), TgError>;
pub async fn add_contact(&self, user_id: i64, first_name: &str, last_name: &str, phone: &str) -> Result<(), TgError>;

// 6C — sending
pub async fn send_poll(&self, chat_id: i64, draft: PollDraft) -> Result<Msg, TgError>;
pub async fn send_location(&self, chat_id: i64, point: GeoPoint) -> Result<Msg, TgError>;
/// Scheduled sends return nothing: the message shows up in get_scheduled and
/// Event::ScheduledChanged fires.
pub async fn send_text_at(&self, chat_id: i64, text: &str, reply_to: Option<i32>, at: DateTime<Local>) -> Result<(), TgError>;
pub async fn send_file_at(&self, chat_id: i64, path: PathBuf, caption: &str, at: DateTime<Local>) -> Result<(), TgError>;
/// Soonest first; Msg.scheduled == true, Msg.ts == send time, Msg.id is the
/// scheduled id (only valid for the two calls below).
pub async fn get_scheduled(&self, chat_id: i64) -> Result<Vec<Msg>, TgError>;
pub async fn send_scheduled_now(&self, chat_id: i64, ids: Vec<i32>) -> Result<(), TgError>;
pub async fn delete_scheduled(&self, chat_id: i64, ids: Vec<i32>) -> Result<(), TgError>;

// 6E — bots and forums
/// Presses a Callback button. Some(text) = the bot answered with a toast /
/// alert text to show; None = silent ack. Url buttons are opened by the UI.
pub async fn press_button(&self, chat_id: i64, msg_id: i32, data: Vec<u8>) -> Result<Option<String>, TgError>;
/// Topics of a forum supergroup, pinned first then newest activity first.
pub async fn get_topics(&self, forum_id: i64) -> Result<Vec<Topic>, TgError>;
pub async fn create_topic(&self, forum_id: i64, title: &str) -> Result<Topic, TgError>;

// 6F — capture and live features
/// MP4 (h264, square `size` px) video circle.
pub async fn send_video_note(&self, chat_id: i64, path: PathBuf, duration: u32, size: u32) -> Result<Msg, TgError>;
pub async fn send_live_location(&self, chat_id: i64, point: GeoPoint, period_secs: u32) -> Result<Msg, TgError>;
pub async fn update_live_location(&self, chat_id: i64, msg_id: i32, point: GeoPoint) -> Result<(), TgError>;
pub async fn stop_live_location(&self, chat_id: i64, msg_id: i32) -> Result<(), TgError>;
/// Peers with active stories, unread first.
pub async fn get_story_peers(&self) -> Result<Vec<StoryPeer>, TgError>;
/// Oldest first.
pub async fn get_stories(&self, chat_id: i64) -> Result<Vec<Story>, TgError>;
/// JPG or MP4 path, cached. None when unavailable.
pub async fn download_story(&self, chat_id: i64, story_id: i32) -> Result<Option<PathBuf>, TgError>;
pub async fn mark_stories_seen(&self, chat_id: i64, up_to_id: i32) -> Result<(), TgError>;
```

Changed behavior of existing methods:

- `download_sticker` and `download_media` (for `MediaKind::Sticker`) now
  return the raw file for animated stickers: a `.tgs` path (gzipped Lottie
  JSON) or a `.webm` path (video sticker). The UI decides by extension: `.png`
  → image, `.tgs` → 6D Lottie widget, `.webm` → 6A video player (muted loop).
- `download_media` for `Location`/`Venue` returns the map PNG (`download_map`
  at zoom 15, 320×180) — so the standard row media pipeline shows it. For
  `Contact`/`Dice`/`Poll` it returns `Ok(None)`.
- Every method accepts topic synthetic chat ids (§1.4).
- `get_history` in a forum chat id (not a topic) returns the whole forum
  history as before; the UI never shows that for forums (6E), only topics.

### 1.4 Forum topics as synthetic chat ids

```rust
pub const TOPIC_CHAT_ID_BASE: i64 = -(1 << 62);
/// Synthetic chat id for (forum, topic). Everything that takes a chat id
/// accepts it: history, send_*, mark_read, pins, search, drafts, downloads.
pub fn topic_chat_id(forum_id: i64, topic_id: i32) -> i64;
/// Some((forum_id, topic_id)) when `chat_id` is a topic id.
pub fn split_topic_chat_id(chat_id: i64) -> Option<(i64, i32)>;
/// Does `msg` (from an Event or a history page) belong to the chat the UI
/// has open? True for plain ids when `msg.chat_id == open_chat_id`; for a
/// topic id when `msg.chat_id == forum_id && msg.topic_id == Some(topic_id)`
/// (General: topic 1 matches `topic_id == Some(1)` and `None`).
pub fn msg_in_chat(msg: &Msg, open_chat_id: i64) -> bool;
```

Events for topic messages carry the FORUM chat id and `topic_id`; the UI
routes with `msg_in_chat`. `Event::DialogsChanged` still means "reload
dialogs"; a topic's unread count changes arrive as `Event::TopicsChanged`.

### 1.5 Events (new)

```rust
/// A poll's votes/closed state changed (also after my own send_vote).
PollChanged { poll_id: i64, poll: Poll },
/// Scheduled list of this chat changed: refetch get_scheduled.
ScheduledChanged { chat_id: i64 },
/// Topic list of this forum changed: refetch get_topics.
TopicsChanged { forum_id: i64 },
/// Story rings changed: refetch get_story_peers (and dialogs' rings).
StoriesChanged,
```

`MessageChanged` also fires for live-location updates (the message's
`location.live.last_update` moves) and for `dice.value` becoming final.

### 1.6 Local services (orchestrator, `src/local/`, used by 6F)

```rust
// src/local/record.rs (existing voice recorder pattern)
/// Starts recording a video circle with ffmpeg: camera → square MP4 (h264,
/// `size` px, ≤ 60 s) + a live preview stream. Returns a handle.
pub fn record_video_note_start(size: u32) -> Result<VideoRecorder, String>;
pub struct VideoRecorder { … }
impl VideoRecorder {
    /// Preview frames: RGBA `size`×`size`, ~10 fps, arrive on the GLib main
    /// context via this receiver.
    pub fn frames(&self) -> async_channel::Receiver<Vec<u8>>;
    /// Stops and returns (mp4 path, duration secs).
    pub async fn stop(self) -> Result<(PathBuf, u32), String>;
    pub fn cancel(self);
}
/// Where the camera comes from: `OMG_CAMERA` (a /dev/videoN path) else the
/// first /dev/video*, else — only in --smoke — ffmpeg's `testsrc` pattern.
/// No camera and not smoke → Err("no camera found").
```

### 1.7 Playback prerequisites (system)

GTK's media backend and the 6A audio player both use GStreamer. Installed
today: base plugins (ogg/opus/vorbis → voice notes play now), `pipewiresink`.
Missing (the orchestrator asks the user to install them; UI must degrade with
an inline error, not crash): `gst-plugins-good` (mp4/mkv demux, mp3,
autoaudiosink, v4l2), `gst-libav` (h264/aac decode). Error text to show when
a pipeline fails with a missing-plugin/decoder error:
`"can't play this: install gst-plugins-good gst-libav"`.

### 1.8 Settings (orchestrator adds to `src/settings.rs`, all hot-reloaded)

```toml
[media]
autoplay_gifs = true          # 6A: GIFs and video stickers loop when visible
autoplay_video_notes = true   # 6A: circles autoplay muted when visible
voice_speed = 1.0             # 6A: remembered speed toggle (1.0 / 1.5 / 2.0)
animated_stickers = true      # 6D: false → first frame only
map_tiles = true              # 6B: false → no network tile fetch, coordinates only
```

`settings.media.*` are read through `SettingsStore::get()` like every other
group. `animations` master toggle (`Effects::animations_enabled()`) ALSO
pauses animated stickers and gif autoplay when off.

### 1.9 Icons added to `src/ui/icons.rs` by the contract commit

| const        | glyph | codepoint | use |
|--------------|-------|-----------|-----|
| PLAY         |      | U+F04B | players |
| PAUSE        |      | U+F04C | players |
| VOLUME       |      | U+F028 | player mute toggle |
| VOLUME_OFF   |      | U+F026 | player muted |
| FULLSCREEN   |      | U+F065 | video fullscreen |
| LOCATION     |      | U+F041 | location/venue cards, attach menu |
| PHONE        |      | U+F095 | contact card, calls |
| POLL         |      | U+F080 | poll card, attach menu |
| DICE         |      | U+F522 | dice card fallback |
| CALENDAR     |      | U+F073 | scheduled |
| SCHEDULE     |      | U+F017 | send-later (same as CLOCK) |
| TOPIC        |      | U+F292 | forum topics |
| ROBOT        |      | U+F06A9 | bot chats / Start |
| CAMERA       |      | U+F030 | video note recorder |
| STORY        |      | U+F111 | stories strip fallback |
| LIVE         |      | U+F1EB | live location |
| ADD          |      | U+F067 | new topic / add contact |
| EXTERNAL     |      | U+F08E | open in browser |
| SPEED        |      | U+F0E7 | speed toggle |

### 1.10 Mock fixtures (all in `src/tg/mock.rs`; names are stable — probes use them)

- Chat **"Media Lab"** (group, id fixed, 14 messages, in this order): a voice
  note (3 s, ogg/opus), a music track (`audio_title` "Night Drive",
  `audio_performer` "Marta", 3 s), a video (320×240, 3 s), a video note
  (240 px, 3 s), a GIF (mp4, 2 s), a location (52.5200, 13.4050), a venue
  ("Café Einstein", "Kurfürstenstraße 58, Berlin"), a live location (period
  3600, not stopped), a contact ("Marta Koenig", "+49 30 1234567",
  user_id = Marta's id), a contact without user id ("Unknown Caller"), a dice
  (🎲, value 4), a dart (🎯, value 6), a rolling dice (🎲, value 0), and an
  animated sticker (.tgs, `sticker_emoji` "🔥"). Mock media files are
  generated on first download with ffmpeg into the media cache
  (`mock-voice.ogg`, `mock-music.ogg`, `mock-video.mp4`, `mock-note.mp4`,
  `mock-gif.mp4`); the `.tgs` is a bundled minimal Lottie
  (`src/tg/fixtures/fire.tgs`). If ffmpeg is missing, `download_media`
  returns `Ok(None)`.
- Chat **"Polls"** (group): an open regular poll (4 options, 12 voters, not
  voted), a multiple-choice poll (not voted), a quiz (not voted, solution
  present), a closed poll (results), a public-voters poll I already voted in.
  `send_vote` updates counts and emits `PollChanged`; retract works.
- Chat **"Omarchy Bot"** (existing bot chat): gets `bot_commands` (`start`,
  `help`, `theme`, `screenshot`), a message with an inline keyboard (rows:
  [Callback "Next theme", Callback "Lock"], [Url "Docs" → https://omarchy.org],
  [SwitchInline "Search" query "omarchy"]). `press_button` with the "Lock"
  data returns `Some("Locked (mock)")`; "Next theme" returns `None` and
  emits a `MessageChanged` with the keyboard's first button text changed to
  "Next theme ✓".
- Chat **"Omarchy Forum"** (channel-kind supergroup, `forum: true`) with
  topics: "General" (1), "Themes" (icon 🎨, 3 unread), "Bugs" (icon 🐛,
  pinned), "Off-topic" (closed). Each topic has 5–8 messages;
  `create_topic` appends and emits `TopicsChanged`. New messages posted to a
  topic via the mock's live feed carry `topic_id`.
- Scheduled: chat **"Marta"** has 2 scheduled messages (tomorrow 09:00, +3 d
  18:30). `send_text_at`/`send_file_at` append; `send_scheduled_now` moves
  the message into the history (NewMessage) and emits `ScheduledChanged`.
- Stories: contacts "Marta" (unread story: photo + video) and "Work" group
  is not a story peer; "Alex" (read story: 1 photo). `mark_stories_seen`
  flips the ring and emits `StoriesChanged`.
- Live location: `send_live_location` returns a message with `live`;
  `update_live_location` emits `MessageChanged`; `stop_live_location` sets
  `stopped`.
- `send_video_note`, `send_poll`, `send_location` append messages like the
  other senders (`OMG_MOCK_FAIL_ONCE` names: `SendPoll`, `SendLocation`,
  `SendVideoNote`, `SendVote`, `PressButton`, `GetTopics`, `CreateTopic`,
  `GetScheduled`, `SendTextAt`, `DownloadMap`, `GetStories`, `DownloadStory`).
- Env hooks: `OMG_MOCK_NO_FFMPEG=1` makes the mock behave as if ffmpeg were
  missing (media → None); `OMG_MOCK_CAMERA=none` makes the video recorder
  fail with "no camera found" even in smoke.

### 1.11 Probe steps each package adds (names are the `probe_step` strings)

- 6A: `player voice play`, `player voice seek`, `player speed`, `player
  music`, `player video`, `player video fullscreen`, `player video note`,
  `player gif autoplay`, `player single active`, `player stops on chat
  switch`, `player scroll pause`.
- 6B: `card location`, `card venue`, `card live location`, `card contact`,
  `card contact unknown`, `card dice`, `card dice rolling`, `poll vote`,
  `poll retract`, `poll multiple`, `poll quiz`, `poll closed`, `poll live
  update`.
- 6C: `attach menu`, `poll dialog open`, `poll dialog validate`, `poll
  dialog send`, `location dialog send`, `location dialog pick`, `send later
  open`, `send later send`, `scheduled strip`, `scheduled send now`,
  `scheduled delete`.
- 6D: `lottie sticker renders`, `lottie sticker offscreen pause`, `lottie
  picker hover`, `lottie master toggle`.
- 6E: `bot keyboard callback`, `bot keyboard alert`, `bot keyboard url`,
  `bot keyboard switch inline`, `bot command autocomplete`, `bot start`,
  `forum topics list`, `forum open topic`, `forum topic header`, `forum
  create topic`, `forum topic new message`.
- 6F: `video note record`, `video note cancel`, `video note send`, `live
  location send`, `live location update`, `live location stop`, `stories
  strip`, `stories viewer`, `stories viewer advance`, `stories seen`.

Under the probe: `Shell.probe == true` — players use `fakesink` audio (no
sound leaves the machine), external launches are counted not performed,
and the video recorder uses ffmpeg `testsrc`.

---

## 2. Package 6A — in-app playback

Files: NEW `src/ui/player.rs`; hooks in `src/ui/messages.rs` (media match
arms for Voice/Audio/Video/VideoNote/Gif and a `.webm` sticker branch in
`finish_media_path`; `reset_chat` → `player::stop_all()`; scroll handler →
`player::visibility_tick`), `src/ui/shell.rs` (`media_action`: never
`launch_media` for these kinds any more; fullscreen overlay), `src/ui/mod.rs`
(`pub mod player;`), `src/theme/style.css`, `Cargo.toml` is NOT yours — the
contract commit adds `gstreamer = "0.25"` (feature-less) already.

### 2.1 Audio (voice, music): GStreamer `playbin3`

- One `Player` per row, created lazily on first play. Pipeline:
  `playbin3` with `audio-sink` = `autoaudiosink` if that factory exists,
  else `pipewiresink`, else `fakesink`; under `Shell.probe` always
  `fakesink sync=true`. `video-sink` = `fakesink` (audio only). Bus watched
  via `bus.add_watch_local` on the main context; `Error` → inline error
  label (missing-plugin/decoder → the §1.7 text), `Eos` → back to start,
  paused; `StateChanged`/duration → refresh.
- Speed: `set_rate(r)` = flushing segment seek at the current position with
  `rate = r`; 1.0 / 1.5 / 2.0 cycle on the SPEED pill; remembered in
  `settings.media.voice_speed` (the shell writes it via `SettingsStore`).
- Progress: a 100 ms `glib::timeout` while playing queries position.
- Card layout (voice, replaces the `omg-doc-pill` button): 40 px round
  play/pause button (`omg-player-btn`, accent bg, PLAY/PAUSE glyph) ·
  progress bar (`gtk::Scale` without value, `omg-player-bar`, click = seek,
  fills with accent) · `0:07 / 0:31` time label (`omg-small`) · SPEED pill
  (`omg-player-speed`, shows "1x"/"1.5x"/"2x") · the existing "transcribe"
  button stays below. Music: same bar plus a title line (`audio_title`,
  bold) and performer line (`omg-muted`) above the bar; falls back to
  `doc_name`. Width: fill the bubble, min 240 px.
- Download: clicking play on an undownloaded row triggers the existing
  download (`start_media_download(msg_id, false)`), shows the asciiload
  placeholder in the button, plays automatically when `on_media_ready`
  fires. Duration before download = `Msg.duration`.

### 2.2 Video, video notes, GIFs, video stickers: `gtk::MediaFile`

- `gtk::MediaFile::for_filename(path)` inside a `gtk::Picture`
  (content-fit Cover) in a `gtk::Overlay`; the paintable is the media
  stream itself (`Picture::set_paintable(Some(&media))`).
- Video bubble: poster = first frame (paint the stream paused at 0 — `pause`
  after `play` at load, `set_playing(false)`); a centered PLAY button
  overlay; click → play, click again → pause; controls row (play/pause,
  progress `gtk::Scale`, time, VOLUME/VOLUME_OFF mute toggle, FULLSCREEN)
  revealed on hover (`omg-player-controls`). Double-click or FULLSCREEN →
  fullscreen: a shell overlay (`omg-viewer` style, like the image viewer:
  backdrop, Esc closes, the same MediaFile re-parented, playing continues).
  Size: bubble width, aspect from `photo_size`, max 360 px tall.
- Video note: 240 px circle (`Picture` in a box with `omg-round`:
  `border-radius: 50%` + `overflow: hidden`), autoplay muted when
  `settings.media.autoplay_video_notes` and the row is visible; click →
  unmute + play from start (with sound); click again → pause. A thin
  circular progress ring is NOT required; show remaining seconds in a
  small pill (`omg-round-time`) at the bottom.
- GIF and `.webm` video stickers: loop (`set_loop(true)`), muted, autoplay
  when `settings.media.autoplay_gifs` && `Effects::animations_enabled()`
  && visible; otherwise poster + PLAY overlay. Max 280 px wide.
- Errors (`MediaStream::error`) → inline `omg-error` label replacing the
  picture, §1.7 text for `GST_CORE_ERROR_MISSING_PLUGIN`/decoder errors,
  the stream's message otherwise.

### 2.3 Single active player, viewport, chat switch

- `player.rs` keeps a thread-local registry of live players keyed by
  `(msg_id, kind)`. `activate(player)` pauses every other player with
  sound (autoplaying muted gifs/notes are NOT paused by that; they pause
  only when they leave the viewport).
- `visibility_tick(is_visible: impl Fn(msg_id) -> bool)` is called from the
  messages scroll `value-changed` handler (throttled to 100 ms) and after a
  history page lands: players whose row is not visible pause (sound players
  remember `resume_on_visible = false` — voice does NOT auto-resume; muted
  loops resume when visible again). `MessagesView` exposes
  `pub fn row_visible(&self, msg_id: i32) -> bool` (row bounds vs the
  scrolled window's visible rectangle, computed via
  `compute_bounds(&scroll)`).
- `stop_all()` on `reset_chat`, on chat switch, on window close. Players
  hold no `Rc<Shell>`.

### 2.4 Acceptance

- `cargo build` clean (no new warnings), `cargo test` green.
- Gate (§0) passes; probe steps of §1.11 (6A) present and passing: the
  voice step asserts the label reaches "playing" state or an error label
  within 2 s (under `fakesink`); `player single active` asserts starting
  the music pauses the voice; `player stops on chat switch` asserts the
  registry is empty after `open_chat` of another chat.
- Screenshot `bin/shot 6a-media "Media Lab" 4` shows the cards.

---

## 3. Package 6B — location, venue, contact, dice, polls (display + voting)

Files: NEW `src/ui/cards.rs` (location/venue/live/contact/dice), NEW
`src/ui/poll.rs`; hooks in `src/ui/messages.rs` (media match arms;
`finish_image` routes Location/Venue textures into the card's map slot;
`update_existing` → `cards::update`/`poll::update`; new `MessageAction`
variants), `src/ui/shell.rs` (action handlers: `Vote`, `RetractVote`,
`AddContact`, `OpenInBrowser`, `Event::PollChanged` → `messages.update_poll`),
`src/ui/mod.rs`, `src/theme/style.css`.

### 3.1 Location / venue / live

- Card (`omg-geo-card`, bubble width, max 320 px): map slot 320×180
  (placeholder "loading map…" with the image_loading effect, exactly like
  photos; auto-download like photos through the existing pipeline —
  `MediaKind::Location|Venue` join the Photo branch for placeholder +
  auto-start; when `settings.media.map_tiles` is false the slot shows the
  LOCATION glyph on `omg-bg-darker` and no download happens) · below:
  venue title (bold) + address (`omg-muted`) for venues; coordinates line
  `52.52000, 13.40500` (`omg-small omg-muted`, selectable) · a text button
  "Open in browser" (EXTERNAL glyph) → `MessageAction::OpenInBrowser(url)`
  with `https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=16/{lat}/{lon}`
  (shell: `gio::AppInfo::launch_default_for_uri`, counted under probe).
- Live location: a `LIVE` glyph + "Live · updated 2 min ago" line
  (`omg-accent`) refreshed each minute via `glib::timeout_add_seconds_local`
  (drop the source on row removal — store it in the row's
  `animation_sources`), "Sharing ended" (`omg-muted`) when `stopped` or
  past `expires`. Own live messages additionally get a "Stop sharing"
  button → `MessageAction::StopLive(msg_id)` (shell → `tg.stop_live_location`).
- `MessageChanged` on a live message re-renders the line and re-downloads
  the map when the point moved (the shell restarts the media download by
  resetting the media state to `NotStarted`).

### 3.2 Contact

- Card (`omg-contact-card`): avatar circle with initials (reuse
  `src/ui/avatar.rs` initials rendering; the download of the real photo is
  NOT required) · name (bold) · phone (`omg-muted`, selectable) · buttons:
  "Open chat" (when `user_id` is Some → `MessageAction::OpenMention(user_id)`)
  and "Add to contacts" (→ `MessageAction::AddContact(msg_id)`; shell calls
  `tg.add_contact`, then the button label flips to "Added"; on error the
  card shows the error inline and the button re-enables).

### 3.3 Dice

- Big emoji (`omg-dice`, 48 px font) + value line "Rolled 4" / "Rolling…"
  (value 0, `omg-muted`). No animation (Telegram's dice animation is a
  sticker set we do not ship). `MessageChanged` updates the value.

### 3.4 Polls

- Card (`omg-poll`, bubble width): question (bold, wraps) · kind line
  (`omg-small omg-muted`): "Anonymous poll" / "Poll" / "Quiz" / "Anonymous
  quiz", plus " · Multiple answers" and " · Closed" when applicable ·
  options list · footer "12 votes" / "No votes yet" · quiz solution line
  after voting (`omg-msg-quote`).
- Option row, not voted, open: a `gtk::CheckButton` (radio-style for
  single-choice via a shared group; check boxes for multiple) with the
  option text; multiple-choice gets a "Vote" button (enabled once ≥ 1 is
  checked) → `MessageAction::Vote { msg_id, options }`; single-choice votes
  on click.
- Option row, voted or closed: option text · percentage right-aligned ·
  a `gtk::LevelBar`-free bar: a `gtk::Box` with `omg-poll-bar` fill width
  proportional (use a `gtk::ProgressBar` with `omg-poll-bar` class, fraction
  = voters/total) · chosen option marked with CHECK glyph; quiz: correct
  option in `omg-poll-correct` (accent), my wrong pick in `omg-poll-wrong`
  (red).
- "Retract vote" text button when `voted && !closed && !quiz` →
  `MessageAction::RetractVote(msg_id)`.
- While a vote is in flight the card is insensitive; on error it re-enables
  and shows the error inline. `Event::PollChanged` → `messages.update_poll
  (poll_id, poll)` (find rows via a `poll_id → msg_id` index kept in the
  store) → `poll::update(widget, &poll)` rebuilds the option rows in place.

### 3.5 Acceptance

Build/test clean; gate passes; §1.11 (6B) probe steps present and passing on
the "Media Lab"/"Polls" fixtures (vote → counts change; retract → back to
choice state; closed → no check buttons); `bin/shot 6b-cards "Media Lab" 3`
and `bin/shot 6b-polls Polls 3`.

---

## 4. Package 6C — sending: polls, location, scheduled

Files: NEW `src/ui/polldialog.rs`, NEW `src/ui/locationdialog.rs`, NEW
`src/ui/scheduled.rs` (date-time picker + scheduled strip/list); hooks in
`src/ui/messages.rs` (attach button opens a popover menu; header strip slot
below the pinned bar; send button secondary-click/long-press menu),
`src/ui/shell.rs` (dialog wiring; `Event::ScheduledChanged`; `open_chat`
refetches `get_scheduled`), `src/ui/mod.rs`, `src/theme/style.css`.

### 4.1 Attach menu

The ATTACH button opens a `gtk::Popover` menu (`omg-attach-menu`, same look
as the main menu): "File…" (FILE, existing file chooser flow), "Poll…"
(POLL), "Location…" (LOCATION). Keyboard: arrows/Enter/Esc. The old direct
file-chooser action stays reachable as the first item.

### 4.2 Poll dialog (`PollDialog`, pattern of `newgroup.rs`: `new()`,
`widget`, `set_action`)

- Question entry (max 255 chars, counter) · options list: 2 rows initially,
  "Add option" up to 10, each row an entry (max 100) with a CLOSE remove
  button (min 2) · toggles: "Anonymous voting" (on), "Multiple answers",
  "Quiz mode" (exclusive with multiple answers) · quiz: a radio per option
  marks the correct answer + "Explanation" entry (optional, max 200) ·
  buttons: Cancel, Create (primary; disabled until question + ≥ 2 non-empty
  options, and a correct answer in quiz mode). Enter in the last option
  entry adds a row. Emits `PollDialogAction::Create(PollDraft)`; the shell
  calls `tg.send_poll`, closes on Ok, shows the error inline on Err (dialog
  stays).

### 4.3 Location dialog (`LocationDialog`)

- Latitude / longitude entries (validated: −90..90 / −180..180; error
  label) prefilled with the last used point (remember in uistate:
  `last_location`) or 52.52/13.405 · a 3×3 map grid (each cell 96×96 from
  `tg.download_map(center_of_cell, zoom, 96, 96)`) centered on the current
  point at `zoom` (default 12; "+"/"−" buttons 3..17); clicking a cell
  moves the point to that cell's center and refetches; `map_tiles` off →
  grid hidden · "Share live for" dropdown (Off / 15 min / 1 h / 8 h) →
  `send_live_location` (6F wires the later updates; 6C only sends) · Send
  (primary) / Cancel. Emits `LocationDialogAction::Send { point, live_secs:
  Option<u32> }`.

### 4.4 Send later

- Secondary click (right button) or long-press on the SEND button, and
  `Ctrl+Shift+Enter` in the composer, open the `SendLaterPopover`
  (`omg-send-later`): a `gtk::Calendar` + hour/minute spin buttons (default
  = next full hour), quick chips "Tomorrow 09:00", "In 1 hour", "Tonight
  20:00", a "Schedule" button. Past times are refused inline. On Schedule:
  text → `tg.send_text_at` (with the current reply target), a pending
  file (caption dialog gets a "Send later…" secondary button) →
  `tg.send_file_at`. The composer clears like a normal send.
- Scheduled strip (`omg-scheduled-bar`, sits under the pinned bar,
  hidden when the list is empty): CALENDAR glyph + "3 scheduled messages";
  click toggles the scheduled panel: a list inside the messages pane's
  overlay (`omg-scheduled-panel`, like the pinned/search overlays) with one
  row per scheduled message: send time (`Tomorrow 09:00`, `Fri 18:30`) ·
  text/caption (ellipsized, or "[file] name") · "Send now" · TRASH.
  `Send now` → `tg.send_scheduled_now`, TRASH → confirm-free delete →
  `tg.delete_scheduled`; the panel refetches on `Event::ScheduledChanged`
  and on `open_chat`. Esc closes the panel.

### 4.5 Acceptance

Build/test clean; gate passes; §1.11 (6C) probe steps present: `poll
dialog validate` asserts Create stays disabled with one option; `poll
dialog send` asserts a `MediaKind::Poll` row appears; `location dialog
send` asserts a Location row; `send later send` asserts the strip appears
with count 3 in "Marta"; `scheduled send now` asserts the count drops and
a NewMessage row appears; `bin/shot 6c-poll-dialog Marta 3` (use the
`OMG_SMOKE_MENU`-style hook `OMG_SMOKE_ATTACH=poll|location|later` you add
to `src/ui/shell.rs` to open the dialog for the screenshot).

---

## 5. Package 6D — animated stickers (Lottie)

Files: NEW `src/ui/lottie.rs`; hooks in `src/ui/messages.rs` (`.tgs` path in
`finish_media_path`/download completion → `lottie::Sticker` widget instead
of the "loading image…" placeholder — the shell's Sticker branch decodes a
texture today; route `.tgs` around that), `src/ui/stickers.rs` (picker cells
for `animated && !video` stickers: first frame static, animate on hover;
sendable), `src/ui/shell.rs` (download branch), `src/ui/mod.rs`,
`src/theme/style.css`. Renderer crate: fixed by the contract commit in
`Cargo.toml` — see §5.1; you do not edit `Cargo.toml`.

### 5.1 Renderer (decided 2026-09-03 after the spike)

**ThorVG** via the `thorvg` crate (`=0.5.1`, vendored C++ built by `cc`,
software rasterizer only; verified on this machine with real Telegram
stickers at 0.2–1 ms/frame @256px). Fallback if it ever fails: `rlottie`
with `vendor-telegram` (needs cmake + network at build time — not used).
The contract commit wires the crate and ships `src/ui/lottie_backend.rs`,
which you use and do not modify:

```rust
pub fn decode_tgs(bytes: &[u8]) -> Result<Vec<u8>, String>;   // gzip or plain JSON
pub struct Engine;                      // !Send: one per render thread
impl Engine {
    pub fn new() -> Result<Engine, String>;
    pub fn load(&self, tgs: &[u8], size: u32) -> Result<Animation<'_>, String>;
}
pub struct Animation<'e>;               // borrows the Engine; !Send
impl Animation<'_> {
    pub fn frame_count(&self) -> usize;
    pub fn fps(&self) -> f64;
    pub fn duration_secs(&self) -> f64;
    pub fn size(&self) -> u32;
    /// RGBA8 premultiplied (`gdk::MemoryFormat::R8g8b8a8Premultiplied`),
    /// `size*size*4` bytes, valid until the next `render`.
    pub fn render(&mut self, index: usize) -> Result<&[u8], String>;
}
```

Consequence for the design below: **one render thread for all stickers**
(it owns the `Engine` and every `Animation`); the UI thread never touches
ThorVG.

### 5.2 Widget (`lottie::Sticker`)

- A `gtk::Picture` (size 256 in bubbles, 96 in picker cells; DPI scale
  aware — render at `size * scale_factor`) whose paintable is replaced
  with a fresh `gdk::MemoryTexture` per frame.
- Rendering off the UI thread: ONE `std::thread` ("lottie-render", started
  lazily, owns the `Engine`) serves every sticker. The UI sends it commands
  over a `std::sync::mpsc` channel (`Load { id, bytes, size }`, `Play(id)`,
  `Pause(id)`, `Drop(id)`); the thread keeps `HashMap<id, Animation>` plus
  per-sticker playhead/fps, and on each tick (sleep until the earliest next
  frame is due, cap 60 Hz) renders the due frames and sends
  `(id, frame_index, Vec<u8>)` back through an `async_channel` the UI side
  drains with `spawn_local` on the main context, turning each into a
  `gdk::MemoryTexture` (`R8g8b8a8Premultiplied`, `size*4` stride). Frames
  the UI has not consumed yet are dropped, never queued (bounded channel of
  2 per sticker or a coalescing map) — the UI must never fall behind.
  Load errors come back as `(id, Err(text))`.
- Global cap: at most 6 stickers animate at once (registry in
  `lottie.rs`); others show their first frame and start when a slot frees
  (oldest-first). Off-screen (via the 6A `row_visible` hook — if 6A is not
  merged yet in your worktree, add your own `row_visible` in messages.rs
  with the same signature; the orchestrator dedupes) → pause; back on
  screen → resume. `settings.media.animated_stickers == false` or
  `Effects::animations_enabled() == false` → first frame only (react to
  settings changes: a `SettingsStore` subscription like other UI code).
- Loading happens off-thread (`gio::spawn_blocking`); the row shows the
  existing image placeholder meanwhile; a parse error → the old "image
  unavailable" label.
- Picker (`src/ui/stickers.rs`): animated cells render frame 0 (cache the
  first-frame texture per sticker id); hover (`gtk::EventControllerMotion`
  enter/leave) plays/pauses; click sends (`animated` stickers become
  sendable — remove the "not sendable" guard for `.tgs`; `.webm` stays as
  is unless 6A is present, in which case webm cells also send).

### 5.3 Acceptance

Build/test clean; gate passes; §1.11 (6D) steps: `lottie sticker renders`
asserts the "Media Lab" sticker row's picture has a paintable within 2 s and
the frame index advanced after 300 ms; `offscreen pause` asserts the
thread count/registry state after scrolling away; `master toggle` asserts
frame index stops advancing after `animations` is turned off in settings;
`bin/shot 6d-sticker "Media Lab" 3`.

---

## 6. Package 6E — bots and forums

Files: NEW `src/ui/bots.rs` (keyboard, command autocomplete, Start), NEW
`src/ui/topics.rs` (topic list + header + new-topic dialog); hooks in
`src/ui/messages.rs` (keyboard slot under the bubble; composer `/`
autocomplete popover anchor; Start button in the composer slot; header
breadcrumb for topics), `src/ui/shell.rs` (`open_chat` branches on
`ChatSummary.forum` → topic list; routing with `tg::msg_in_chat`;
`Event::TopicsChanged`; actions), `src/ui/chatlist.rs` (forum rows get the
TOPIC glyph before the title), `src/ui/mod.rs`, `src/theme/style.css`.

### 6.1 Inline keyboards

- Rendered under the bubble text (`omg-keyboard`): one `gtk::Box` row per
  keyboard row, buttons `omg-keyboard-btn` (flat, `omg-bg-lighter`, full
  row width split equally, 32 px tall, text ellipsized). `Url` → EXTERNAL
  glyph suffix, click → `MessageAction::OpenLink(url)` (exists).
  `Callback` → `MessageAction::PressButton { msg_id, data }`: the button
  shows a spinner-free "…" suffix while in flight; the shell calls
  `tg.press_button`; `Some(text)` → toast via `messages.show_error`-style
  info bar (`messages.show_info(text)` — add it next to `show_error`, same
  bar, `omg-info` class instead of `omg-error`); `None` → nothing.
  `SwitchInline` → put `@{bot_username} {query}` into the composer of the
  current chat (`same_chat`) or open the switcher (Ctrl+K) with the text
  pending for the picked chat. `Other` → insensitive.
- `MessageChanged` re-renders the keyboard (`bots::update_keyboard`).

### 6.2 Commands

- In a `ChatKind::Bot` chat (or a group whose `ChatInfo.bot_commands` is
  non-empty), typing `/` at the start of the composer opens the
  `omg-command-popover` above the composer: rows `/theme — Switch the
  Omarchy theme`, filtered by prefix as you type, Up/Down/Enter/Tab select
  (fills `/command ` into the composer), Esc closes. The list comes from
  `ChatInfo.bot_commands` (the shell already fetches `get_chat_info` on
  open — pass the commands to `MessagesView::set_bot_commands`).
- Start: when a bot chat's history is empty, the composer is replaced by a
  full-width "Start" button (`omg-start-btn`, ROBOT glyph) → sends
  `/start` via `tg.send_text`; the composer returns once the first message
  exists.

### 6.3 Forum topics

- Opening a chat with `forum == true` shows the **topic list** in the
  messages pane instead of history: header = forum title + "N topics";
  rows (`omg-topic-row`, 56 px): icon emoji (or TOPIC glyph) · title (bold)
  · last message preview (`omg-muted`, ellipsized) · time · unread badge
  (reuse the chat-list badge style); pinned first; closed topics get a
  MUTE-style "closed" tag. Click → `shell.open_chat(topic.chat_id)`.
  "New topic" button (ADD) in the header → a small dialog (title entry,
  Create/Cancel, `newgroup.rs` pattern) → `tg.create_topic` → open it.
- A topic chat: header title "Forum title › Topic title" with a LEFT
  back button returning to the topic list; the chat-list selection stays on
  the forum row; Alt+Up/Down still navigates the chat list. Sending,
  replies, pins, drafts, reads all work through the synthetic id (backend).
- Routing: everywhere the shell compares `msg.chat_id == open_chat`, use
  `tg::msg_in_chat(&msg, open_chat)`; `Event::NewMessage` for a topic that
  is not open bumps the topic's unread in the list (refetch on
  `TopicsChanged`, which the backend also emits for new topic messages).
- The sidebar preview for a forum shows the last message across topics
  (unchanged); the unread badge is the forum's total (unchanged).

### 6.4 Acceptance

Build/test clean; gate passes; §1.11 (6E) steps on "Omarchy Bot" and
"Omarchy Forum" (`bot keyboard alert` asserts the info bar shows "Locked
(mock)"; `bot keyboard callback` asserts the first button's label changes
to "Next theme ✓"; `forum open topic` asserts the header breadcrumb;
`forum topic new message` injects a mock live message into "Themes" and
asserts it shows only when that topic is open); `bin/shot 6e-topics
"Omarchy Forum" 3`, `bin/shot 6e-bot "Omarchy Bot" 3`.

---

## 7. Package 6F — capture and live features (after 6A–6E are merged)

Files: NEW `src/ui/videonote.rs` (recorder UI), NEW `src/ui/stories.rs`
(strip + viewer), live-location wiring in `src/ui/locationdialog.rs` and
`src/ui/cards.rs`; hooks in `src/ui/messages.rs` (mic button long-press /
secondary click → video note; strip slot above the chat list is in
`src/ui/chatlist.rs`), `src/ui/shell.rs`, `src/ui/mod.rs`,
`src/theme/style.css`.

### 7.1 Video notes

- Secondary click on the MIC button (or `Ctrl+Shift+V`) starts a video
  note: the composer is replaced by the recorder bar (like the voice
  recorder bar): a 240 px live circular preview (frames from
  `VideoRecorder::frames()` → `gdk::MemoryTexture` in a `gtk::Picture`
  inside `omg-round`), a red dot + timer, Cancel / Send. 60 s auto-stop.
  Send → `tg.send_video_note(chat, path, duration, 240)`; the resulting
  message renders through the 6A circle player. Errors ("no camera found",
  ffmpeg failures) show in the bar with a Close button.

### 7.2 Live location

- After `send_live_location`, the own message card shows "Live · sharing
  for 58 min" + "Update position" (opens the location dialog prefilled;
  Send → `tg.update_live_location`) + "Stop sharing". The shell keeps a
  list of own live messages `(chat_id, msg_id, expires)`; when `expires`
  passes, the card flips to "Sharing ended" locally.

### 7.3 Stories

- Strip (`omg-stories-strip`, 72 px, scrolls horizontally, above the chat
  list under the search box, hidden when `get_story_peers` is empty):
  40 px avatars (reuse `avatar.rs`) with a 2 px ring (`omg-story-unread`
  accent, `omg-story-read` muted) and the first name below (`omg-small`).
  Chat-list avatars get the same ring from `ChatSummary.story_ring`.
- Viewer (overlay like the image viewer, `omg-story-viewer`): 9:16 stage
  max 720 px tall, progress segments at the top (one per story, the
  current one filling over 5 s for photos / the video duration), header
  avatar + name + relative time, caption at the bottom, Left/Right and
  click on the left/right third to navigate, Space pauses, Esc closes;
  auto-advances to the next story then the next peer; `mark_stories_seen`
  is called when a story becomes current. Media via `download_story`;
  video via the 6A `gtk::MediaFile` path with sound.

### 7.4 Acceptance

Build/test clean; gate passes; §1.11 (6F) steps (recorder under probe uses
`testsrc`; `video note send` asserts a `VideoNote` row appears; `stories
viewer advance` asserts the second segment becomes current; `stories seen`
asserts Marta's ring flips to Read); `bin/shot 6f-stories Marta 3`.

---

## 8. Package 6G — calls

Research spike result and verdict: recorded in FEATURES.md by the
orchestrator after the spike; no implementation is part of wave 6 unless the
verdict says "build".
