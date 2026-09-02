# Wave 5 — Telegram parity: layout, in-app configuration, everyday functions

Decided 2026-09-02 (orchestrator, after the user's first real look at the app).
User direction, verbatim: "telegram layout but make it more full and
sophisticated". Scope: 5A layout/chrome, 5B in-app configuration, 5C
daily-use functions, 5D regular-use functions. All four.

This spec is the design authority. Workers implement it verbatim; when it is
silent, match Telegram Desktop's behavior; when unsure, plainer wins.

---

## 0. Design direction

- **Layout, spacing, behaviors: Telegram Desktop.** Three columns — chat list
  (resizable), messages, optional info panel (resizable). Search box + main
  menu above the chat list. Chat header with avatar, title, status line and
  action buttons. Bubbles with inline meta. Day separators. Composer with
  attach / emoji / text / mic-or-send.
- **Skin: Omarchy.** Colors ONLY from theme tokens (`var(--…)` in
  `src/theme/style.css`, `omg-*` classes). Font stays "JetBrainsMono Nerd
  Font" everywhere. Flat surfaces, no shadows, no gradients. Radius: 6px on
  bubbles, avatars, pills, menus (this raises the old 4px cap — orchestrator
  decision). 8px spacing grid. Type scale: 10.5pt body (current), 9.5pt
  secondary (previews, meta), 12pt titles, 9pt badges — set via existing
  classes `omg-muted`, `omg-small`, `omg-title` (add if missing).
- **Icons: Nerd Font glyphs**, never emoji, never icon-theme images. All
  glyphs live in one new module `src/ui/icons.rs` as `pub const` &str and are
  referenced only from there:

  | const           | glyph | codepoint |
  |-----------------|-------|-----------|
  | MENU            |      | U+F0C9    |
  | SEARCH          |      | U+F002    |
  | MORE (⋮)        |      | U+F142    |
  | INFO            |      | U+F129    |
  | ATTACH          |      | U+F0C6    |
  | EMOJI           |      | U+F118    |
  | MIC             |      | U+F130    |
  | SEND            |      | U+F1D8    |
  | CHECK           |      | U+F00C    |
  | CHECK_DOUBLE    |      | U+F560    |
  | CLOCK (sending) |      | U+F017    |
  | PIN             |      | U+F08D    |
  | MUTE            |      | U+F1F6    |
  | ARCHIVE         |      | U+F187    |
  | FORWARD         |      | U+F064    |
  | REPLY           |      | U+F112    |
  | TRASH           |      | U+F1F8    |
  | CLOSE           |      | U+F00D    |
  | DOWN            |      | U+F063    |
  | LEFT / RIGHT    |  /  | U+F053 / U+F054 |
  | IMAGE           |      | U+F03E    |
  | FILE            |      | U+F15B    |
  | LINK            |      | U+F0C1    |
  | USERS           |      | U+F0C0    |
  | USER            |      | U+F007    |
  | EYE (views)     |      | U+F06E    |
  | EDIT            |      | U+F044    |
  | STICKER         |      | U+F1B2    |
  | GIF             | GIF  | text      |
  | COPY            |      | U+F0C5    |
  | SAVE            |      | U+F0C7    |
  | STOP            |      | U+F04D    |
  | GHOST           |      | U+F6E2    |

- **Name colors** (sender names in groups, avatar backgrounds): 7 theme
  tokens by `user_id % 7`: accent, green, cyan, blue, magenta, yellow, orange.
  Expose them as CSS classes `omg-c0`…`omg-c6` (text color) and
  `omg-avatar-c0`…`omg-avatar-c6` (background, with `--bg-darker` text) in
  style.css — the theme module already substitutes every key in
  `colors.toml`; orchestrator adds the `$green` … `$orange` placeholders.
- **Avatars**: real profile photo when available (`download_avatar`), else
  two-letter initials on a name-colored square, 6px radius. Sizes: 44px in
  chat rows, 36px in the header, 28px in the collapsed sidebar, 96px in the
  info panel. Never round.
- **Adaptive width.** The window is often a 700px tile on a 2x display. Rules:
  sidebar default 300px, min 220px, max 45% of window; collapsed mode = 64px
  avatar strip. Below 640px window width the sidebar auto-collapses (and
  restores when wider again — remember the user's explicit choice
  separately). Info panel 320px, min 280px; below 1000px it opens as an
  overlay sheet on the right instead of a third column.
- **Every list has four states**: loading (spinner row), empty (one muted
  sentence, e.g. "No results", "No contacts"), error (message + Retry),
  populated. No lorem ipsum, no filler.
- **Keyboard-first stays**: every action reachable by keyboard; focus rings
  visible (existing `omg-focus` rules); menus and dialogs close on Esc.

---

## 1. Backend API (orchestrator implements; workers compile against it)

`src/tg/mod.rs` is the contract. Types below are final. Real and mock behave
identically; the mock ships the fixtures in §7.

```rust
pub enum ChatKind { User, Bot, Group, Channel, Saved }
pub enum Presence { Unknown, Online, LastSeen(DateTime<Local>), Recently, LastWeek, LastMonth, LongAgo }
pub enum MuteMode { Unmute, Forever, Hours(u32) }
pub enum MediaKind { Photo, Sticker, Voice, Document, Video, Gif, Audio, VideoNote, Unsupported }
pub enum SpanKind { Bold, Italic, Underline, Strike, Code, Pre(String), Link(String), Mention(i64), Spoiler, Blockquote }
pub struct Span { pub start: usize, pub end: usize, pub kind: SpanKind }   // char offsets into Msg.text, half-open
pub struct WebPreview { pub url: String, pub site_name: String, pub title: String, pub description: String }
pub struct Reaction { pub emoji: String, pub count: i32, pub chosen: bool }

pub struct ChatSummary {
    pub id: i64, pub title: String, pub kind: ChatKind, pub username: String,
    pub last_message: String, pub last_sender: String,       // last_sender: "" (1:1/channel), "You", or first name
    pub last_time: Option<DateTime<Local>>, pub last_msg_id: i32, pub last_outgoing: bool,
    pub unread: i32, pub mentions: i32, pub unread_mark: bool,
    pub read_inbox_max_id: i32, pub read_outbox_max_id: i32,
    pub pinned: bool, pub muted: bool, pub archived: bool,
    pub presence: Presence, pub has_photo: bool, pub draft: String,
}
pub struct Msg {
    // existing: id, chat_id, chat_title, sender, sender_id, text, ts, outgoing, media, doc_name, reply_to, reactions, edited, deleted
    pub spans: Vec<Span>, pub markdown: String,             // markdown = text with Telegram-style markers, for the edit buffer
    pub webpage: Option<WebPreview>, pub forwarded_from: Option<String>, pub views: Option<i32>,
    pub duration: Option<u32>, pub doc_size: Option<u64>, pub photo_size: Option<(i32, i32)>,
    pub sticker_emoji: Option<String>, pub pinned: bool,
}
pub struct ChatInfo { pub id: i64, pub title: String, pub kind: ChatKind, pub username: String, pub phone: String,
    pub about: String, pub members: Option<i32>, pub presence: Presence, pub has_photo: bool, pub muted: bool, pub is_contact: bool }
pub enum MemberRole { Creator, Admin, Member }
pub struct Member { pub user_id: i64, pub name: String, pub username: String, pub presence: Presence, pub role: MemberRole }
pub struct Contact { pub user_id: i64, pub name: String, pub username: String, pub phone: String, pub presence: Presence, pub has_photo: bool }
pub enum SharedKind { Photos, Files, Links, Voice, Music }
pub struct Folder { pub id: i32, pub title: String, pub chats: Vec<i64> }   // membership computed by the backend
pub struct StickerPack { pub id: String, pub title: String, pub count: i32 } // "recent", "favorites", or a set id
pub struct Sticker { pub id: i64, pub emoji: String, pub animated: bool }
pub struct Gif { pub id: i64, pub width: i32, pub height: i32 }
pub struct Me { pub id: i64, pub name: String, pub username: String, pub phone: String, pub has_photo: bool }

pub enum Event {
    NewMessage(Msg), MessageChanged(Msg), Typing { chat_id, name }, MessageDeleted { chat_id, msg_ids },
    ReadOutbox { chat_id: i64, max_id: i32 },   // the other side read up to max_id → ✓✓
    ReadInbox  { chat_id: i64, max_id: i32 },   // read on another device → drop unread
    Presence   { user_id: i64, presence: Presence },
    DialogsChanged,                               // pin/mute/archive/draft/new dialog: reload get_dialogs (coalesce 300ms)
    PinnedChanged { chat_id: i64 },              // refetch get_pinned_message
}
```

`Tg` methods (all async, `Result<_, TgError>`):

| method | notes |
|---|---|
| `submit_credentials(api_id: i32, api_hash: &str) -> AuthState` | writes config.toml 0600, continues to NeedPhone |
| `get_dialogs() -> Vec<ChatSummary>` | up to 200, archived included (flagged), pinned first then by time |
| `get_messages(chat_id, ids: Vec<i32>) -> Vec<Msg>` | reply quotes, jump targets |
| `download_avatar(chat_id_or_user_id: i64) -> Option<PathBuf>` | small photo, cached in `~/.cache/omarchygram/avatars/` |
| `send_text(chat_id, markdown: &str, reply_to)` | markers: `**bold**` `__italic__` `~~strike~~` `` `code` `` ```` ```pre``` ```` `\|\|spoiler\|\|` `[text](url)` |
| `send_voice(chat_id, path, duration_secs)` | OGG/Opus voice note |
| `send_sticker(chat_id, sticker_id)`, `send_gif(chat_id, gif_id)` | |
| `delete_messages(chat_id, ids)` | multi-select |
| `forward_messages(from_chat, ids, to_chat) -> Vec<Msg>` | |
| `search_messages(chat_id, query, before_id) -> Vec<Msg>` | newest first, 50/page |
| `search_global(query) -> Vec<Msg>` | newest first, 50 |
| `search_chats(query) -> Vec<ChatSummary>` | usernames/contacts not in the dialog list (the UI filters loaded dialogs itself) |
| `get_pinned_message(chat_id) -> Option<Msg>`, `pin_message(chat_id, msg_id, pinned: bool)` | |
| `send_reaction(chat_id, msg_id, emoji: Option<String>)`, `get_available_reactions() -> Vec<String>` | None removes |
| `set_pinned(chat_id, bool)`, `set_muted(chat_id, MuteMode)`, `set_archived(chat_id, bool)`, `mark_unread(chat_id, bool)`, `delete_chat(chat_id)`, `clear_history(chat_id)` | each followed by `DialogsChanged` |
| `save_draft(chat_id, text, reply_to)` | UI debounces 1s; empty text clears |
| `get_chat_info(chat_id) -> ChatInfo`, `get_members(chat_id, offset, limit) -> Vec<Member>`, `get_shared_media(chat_id, kind, before_id) -> Vec<Msg>` | |
| `get_contacts() -> Vec<Contact>`, `open_user(user_id) -> ChatSummary`, `create_group(title, user_ids) -> ChatSummary` | |
| `get_folders() -> Vec<Folder>` | |
| `get_sticker_packs()`, `get_stickers(pack_id) -> Vec<Sticker>`, `download_sticker(id) -> Option<PathBuf>` | static webp→png; animated → None |
| `get_saved_gifs() -> Vec<Gif>`, `download_gif(id) -> Option<PathBuf>` | mp4 |
| `get_me() -> Me`, `log_out()` | log_out deletes the session file, returns to NeedPhone |

Existing methods keep their signatures (`send_text`/`edit_text` now take
markdown; plain text without markers is unchanged).

Settings (`src/settings.rs`, orchestrator): `Settings.keys: BTreeMap<String, String>`
(action id → GTK accelerator name, missing = default) with
`settings::key_actions() -> &[KeyAction { id, label, group, default }]`;
`Settings.ui: UiSettings { markdown_send: bool (default true), send_on_enter: bool (default true), show_avatars: bool (default true), compact_list: bool (default false) }`.
Window/pane state lives in `src/uistate.rs` (orchestrator): `UiState { window_w, window_h, maximized, sidebar_width, sidebar_collapsed, info_panel_open, folder_id }`, `UiState::load()`, `save()` — plain file `~/.local/state/omarchygram/ui-state.toml`, NOT hot-reloaded, written at most once per second (debounce in the caller).
Config (`src/config.rs`, orchestrator): `config::ai_keys() -> AiKeys`, `config::set_ai_key(provider, Option<&str>)`, `config::has_credentials()`, `config::set_credentials(api_id, api_hash)`. All writes atomic and 0600.
Recording (`src/local/`, orchestrator): `Local::record_start() -> Result<()>`, `record_stop() -> Result<(PathBuf, u32 secs)>`, `record_cancel()`. Uses ffmpeg (PipeWire/Pulse input), OGG Opus 48k mono. Errors are user-facing strings ("ffmpeg not installed", "no microphone").

Default key actions (id — label — default):
`switcher` — Chat switcher — `<Control>k`; `settings` — Settings — `<Control>comma`; `next_chat` — Next chat — `<Alt>Down`; `prev_chat` — Previous chat — `<Alt>Up`; `search` — Search chats and messages — `<Control>f`; `search_in_chat` — Search in this chat — `<Control><Shift>f`; `chat_info` — Chat info — `<Control><Shift>i`; `toggle_sidebar` — Collapse sidebar — `<Control><Shift>b`; `jump_to_date` — Jump to date — `<Control>j`; `reply_last` — Reply to last message — `<Control>Up`; `saved` — Saved messages — `<Control>0`; `contacts` — Contacts — `<Control><Shift>c`; `bold` — Bold — `<Control>b`; `italic` — Italic — `<Control>i`; `underline` — Underline — `<Control>u`; `strike` — Strikethrough — `<Control><Shift>x`; `mono` — Monospace — `<Control><Shift>m`; `link` — Link — `<Control><Shift>k`; `spoiler` — Spoiler — `<Control><Shift>p`; `escape` — Cancel / focus composer — `Escape` (fixed). Group "Composer" bindings apply only while the composer has focus.

---

## 2. Package 5A — Layout and chrome (UI worker)

Files: `src/ui/shell.rs`, `src/ui/chatlist.rs`, `src/ui/messages.rs`, new
`src/ui/icons.rs`, new `src/ui/avatar.rs`, new `src/ui/menus.rs`,
`src/theme/style.css` (add rules; existing tokens only). May extend
`src/tg/mock.rs` FIXTURES only (see §7). Must not touch any other backend file.

### 2.1 Window and columns
- `gtk::Paned` (horizontal) between sidebar and messages; handle 1px
  `--bg-lighter`, `omg-handle` class; `shrink_start_child=false`; sidebar
  width rules from §0; width persisted via `UiState` (debounced 1s).
- `toggle_sidebar` action collapses to the 64px avatar strip (rows show only
  the avatar + unread dot; tooltip = title). Auto-collapse below 640px
  window width; the auto state never overwrites the persisted user choice.
- Window size/maximized restored from `UiState` on start, saved on close.

### 2.2 Sidebar
- **Top bar (48px)**: MENU button (opens the main menu popover), search
  `gtk::SearchEntry` (placeholder "Search"), hexpand. While the entry has
  text the list shows results instead of dialogs: section "Chats" (loaded
  dialogs filtered case-insensitively by title/username, then
  `search_chats` results appended for ≥3 chars, deduplicated), section
  "Messages" (`search_global`, ≥3 chars, 300ms debounce, newest first, rows
  = chat title · sender · snippet · date; click opens the chat scrolled to
  that message). Esc clears and refocuses the list. `search` action focuses
  the entry.
- **Main menu** (popover, `omg-menu`): header = my avatar + name + phone
  (`get_me`), items: Saved messages, Contacts, New group, Archived chats,
  Settings, Keyboard shortcuts, About, Log out (confirm dialog). Items
  without a 5D implementation yet call the shell hooks the 5D package fills
  in (leave the item, wire a no-op shell method `open_contacts()`, etc.).
- **Folder tabs** (row under the top bar, only when `get_folders` is
  non-empty): "All" + one tab per folder, each with an unread count pill;
  active tab underlined with `--accent`; selection persisted (`folder_id`).
  Filtering is client-side on `Folder.chats`.
- **Rows (64px, 8px vertical padding)**: avatar 44 (`avatar.rs`: initials
  fallback immediately, then `download_avatar` async with an epoch guard so a
  reused row never shows a stale photo); line 1 = title (bold, ellipsize END,
  hexpand) + MUTE glyph (muted) + right column: time (today "HH:MM", this
  week "Mon", else "DD.MM.YY"); line 2 = preview (`last_sender: ` prefix in
  groups, "Draft: " in `--red` when draft non-empty and chat not open, media
  placeholders "[photo]" etc.) ellipsized + right column: PIN glyph when
  pinned and no unread, unread pill (accent bg; `--muted` bg when muted),
  "@" pill when `mentions>0`, or ✓/✓✓ when the last message is outgoing
  (✓✓ when `last_msg_id <= read_outbox_max_id`). Title never yields to the
  preview: the preview label gets `ellipsize END` first; the time column
  is natural width. Tooltip on the row = full title. Selected row =
  `--selection` background (existing), no left bar.
- **Row context menu**: Open, Mark as read / Mark as unread, Pin / Unpin,
  Mute / Unmute (submenu: 1 hour, 8 hours, 2 days, Forever), Archive /
  Unarchive, Clear history (confirm), Delete chat (confirm). Each calls the
  backend and relies on `DialogsChanged` to refresh.
- Archived chats: hidden from the main list; an "Archived chats" row pinned
  at the bottom of the list (count) when any exist; opening it shows the
  archived list with a back button in the top bar.

### 2.3 Chat header (56px)
- Left: avatar 36, title (bold), status line under it: presence text
  (`Online` in `--accent`, `last seen at HH:MM` / `last seen DD.MM.YY` /
  `last seen recently` / `last seen within a week/month` / `last seen a long
  time ago`), or `N members` (groups; from `get_chat_info`, cached per
  chat), or `N subscribers`, or `bot`. Typing indicator replaces the status
  line while active. Ghost pill stays; the header clock stays (setting).
- Right: SEARCH (in-chat search, 5C), INFO (toggles the info panel, 5D — a
  no-op with tooltip "Chat info" until 5D), MORE menu: Search, Mute/Unmute,
  Pin/Unpin, Mark as unread, Jump to date (moves here — the "Jump…" button
  is removed), Clear history, Delete chat, Chat info.
- Virtual chats (Assistant/Omarchy) keep a plain header: avatar with
  initials "AI"/"OM", status "local".

### 2.4 Messages
- **Bubble**: max width `min(66% of pane, 520px)`; incoming `--bg-lighter`,
  outgoing existing tint; 6px radius; padding 6px 10px; 4px between
  consecutive bubbles of the same sender, 12px otherwise. Consecutive
  messages from the same sender within 5 minutes: only the first shows the
  sender name.
- **Order inside a bubble**: forwarded header ("Forwarded from NAME",
  `omg-muted`, FORWARD glyph) → sender name (groups/channels, incoming only,
  `omg-cN` color; NEVER in 1:1 chats, NEVER "You") → reply quote (2px
  `--accent` bar, sender name + one line of text, click jumps to the
  message; text from `get_messages` when not loaded) → media → text with
  formatting (see 5C) → web preview card (5C) → meta line → reactions.
- **Meta inline**: time (existing format setting), "edited", EYE views for
  channels, and for outgoing: CLOCK while sending, CHECK sent, CHECK_DOUBLE
  when `id <= read_outbox_max_id` (updates on `ReadOutbox`). Meta sits
  bottom-right; when the last text line has room it shares the line
  (Telegram style) — acceptable simplification: always its own right-aligned
  line, 2px top margin, `omg-small omg-muted`.
- **Day separators**: centered pill between days ("Today", "Yesterday",
  "September 1", "1 September 2025"), `--bg-lighter`, 6px radius; must
  survive pagination (older pages insert separators; no duplicates; rule
  C2/C3 merge). The floating date chip stays.
- **Scroll-to-bottom**: 36px square button, DOWN glyph, unread count pill on
  it; bottom-right, 16px inset; visible when scrolled up > 1 screen.
- Empty chat: one muted line "No messages yet". History loading: spinner
  row at the top while paging.

### 2.5 Composer (min 44px)
- Row: ATTACH glyph button, EMOJI glyph button (opens `gtk::EmojiChooser`
  inserting at the cursor), TextView (1–8 lines, then scrolls; 6px 10px
  padding; placeholder "Message" as an overlay label), MIC glyph button
  when the text is empty, SEND (accent) when non-empty — the two swap in
  place. Reply/edit banner above the row: 2px accent bar, "Reply to NAME" /
  "Edit message" + one line, CLOSE button.
- Draft per chat: text, reply target and cursor are kept in a
  `HashMap<chat_id, Draft>` and restored on switch-back (fixes today's
  loss); `save_draft` to Telegram debounced 1s and on switch; on open, if the
  local draft is empty and `ChatSummary.draft` is not, prefill it.

### 2.6 Acceptance (machine-checkable)
1. `cargo build` 0 warnings; `cargo test` green.
2. Strict gate: `G_DEBUG=fatal-criticals ./target/debug/omarchygram --smoke --probe` ×6, `OMG_MOCK_AUTH=1` ×3, `OMG_MOCK_LATENCY_MS=400` ×1 — all exit 0 with zero `Gtk-WARNING|Gtk-CRITICAL|Theme parser` lines.
3. The probe traversal is extended (with `OMG_PROBE_TRACE` steps) to: drag/collapse/expand the sidebar via the action, type in search (chats + messages), open and close the main menu and the row menu, open the archived list and go back, switch folder tabs, open a group chat and verify a sender-name label exists and none exists in the 1:1 chat, open the ⋮ menu, switch chats with a draft and verify the draft is restored, resize the window to 600px wide and back (auto-collapse).
4. Screenshots: the worker runs `bin/shot` (new helper: launches `--smoke` with `OMG_SMOKE_OPEN=<chat>` and captures the window with grim, no synthetic input) for: sidebar+1:1 chat, group chat, search results, main menu open — files under `target/shots/`. The orchestrator reviews them.
5. No `set_can_focus(false)` on selectable labels; every popover/menu parented to a row is dismissed via `dismiss_row_popovers` before teardown (GTK lessons in CLAUDE.md).

---

## 3. Package 5B — In-app configuration (UI worker; orchestrator provides config/settings/uistate modules)

Files: `src/ui/auth.rs`, `src/ui/settings_view.rs`, new `src/ui/keys_view.rs`.

- **Login → credentials**: when `AuthState::NeedCredentials`, show a form:
  explanation (2 lines), `LinkButton` to https://my.telegram.org/apps,
  fields "API ID" (digits only) and "API hash" (32 hex chars, masked with a
  reveal toggle), "Continue" (disabled until valid) → `submit_credentials`.
  Errors inline. Never log or print the values. The old SETUP_HELP text
  screen is removed.
- **Settings → Account** (new first section): my name, phone, username
  (`get_me`), "API credentials: configured" + "Change…" (re-opens the
  credentials form as a dialog), "Log out" (confirm).
- **Settings → AI**: providers become `gtk::DropDown`s (Auto + the fixed
  ids); model a `gtk::Entry`; **API keys**: one masked `PasswordEntry` per
  provider (Anthropic, OpenAI, Groq, Gemini) with status "set"/"not set",
  Save writes via `config::set_ai_key`, "Clear" removes. Whisper model path
  entry with a file chooser. A "Test" button runs the assistant `/status`
  probe and shows which providers answered.
- **Settings → Keyboard** (`keys_view.rs`): grouped list (Global /
  Composer) of `key_actions()` rows: label, current accelerator label
  (`gtk::accelerator_get_label`), "Change" → the row captures the next key
  press via `EventControllerKey` (Esc cancels, Backspace clears to default),
  conflicts within a group highlighted in `--red` with "also used by X" and
  the Save refused until resolved; "Reset all". Writes `Settings.keys`.
  The shell's `ShortcutController` rebinds live on settings change.
- **Settings → Appearance**: show avatars (toggle), compact chat list
  (toggle: 56px rows), send on Enter (toggle; off = Ctrl+Enter sends),
  markdown formatting on send (toggle).
- Settings pages are a left `gtk::StackSidebar`-style list (Account,
  Appearance, Timestamps, Privacy, AI, Omarchy, Keyboard, Animations) +
  content, replacing the single scroll; the probe walks every page.

Acceptance: build/test/strict gate as §2.6; probe walks credentials form
(`OMG_MOCK_AUTH=1` starts at NeedCredentials when `OMG_MOCK_NEED_CREDS=1`),
every settings page, key capture (probe uses `probe_submit`-style hooks,
never synthetic desktop input), key conflict display; `grep -rn "api_hash"
src/ui` shows no logging of values.

---

## 4. Package 5C — Daily-use functions (UI worker)

Files: `src/ui/messages.rs`, `src/ui/shell.rs`, `src/ui/chatlist.rs`, new
`src/ui/viewer.rs`, new `src/ui/forward.rs`, new `src/ui/markup.rs`,
`src/theme/style.css`.

- **In-chat search**: SEARCH in header opens a bar under the header: entry,
  "N of M", up/down, close. `search_messages`; results highlighted (row
  `omg-hit` class), navigation jumps (loads the page via
  `get_history_at_date`-style anchoring: use `get_messages` + surrounding
  `get_history(before_id)`), Esc closes.
- **Formatting**: `markup.rs` converts `Msg.text + spans` to Pango markup:
  bold/italic/underline/strike, code (`<tt>` on `--bg-darker`), pre as a
  block, links underlined `--accent` (clickable → `gtk::show_uri`;
  `Mention` opens the user via `open_user`), spoiler = text on `--muted`
  background revealed on click, blockquote = 2px bar. Composer shortcuts
  wrap the selection with the markers from §1; edit prefills `Msg.markdown`.
- **Web preview card**: below text, 2px `--accent` bar, site name (small),
  title (bold), description (max 3 lines), click opens `url`.
- **Reactions**: message context menu starts with a row of 7 quick
  reactions (`get_available_reactions`, first 7) + "…" opening the emoji
  chooser; clicking a reaction pill toggles it (`send_reaction`, chosen =
  accent outline). Optimistic update, revert on error.
- **Pinned bar**: under the header, 40px, PIN glyph, "Pinned message" +
  one line; click jumps; MORE menu "Unpin". Fed by `get_pinned_message` on
  open and `PinnedChanged`.
- **Forward**: context menu "Forward" → `forward.rs` dialog: search entry +
  chat list (dialogs; multi-target allowed) → `forward_messages`; then
  switch to the (last) target. Forwarded bubbles show the header (§2.4).
- **Image viewer** (`viewer.rs`): click a photo → full-window overlay
  (`--bg-darker` at 96%), picture fit-to-window, caption + sender + time at
  the bottom, LEFT/RIGHT between photos in the loaded history, SAVE (file
  chooser, default `~/Pictures`), "Open" (xdg-open), CLOSE / Esc. Focus
  trapped inside; returns focus to the row on close.
- **Chat actions** from 2.2/2.3 menus are all wired here if 5A left any
  as stubs; plus "Mark as unread" also from the ⋮ menu.
- **Read state**: `ReadOutbox` flips ticks; `ReadInbox` clears the sidebar
  badge; presence events update header + rows.
- **Media kinds**: Video/Gif/Audio/VideoNote render as a card with the kind
  glyph, filename/duration/size, Download → opens externally (no in-app
  playback in this wave). Voice: existing play-externally + transcribe.
- **Multi-message keyboard**: `reply_last` action.

Acceptance: §2.6 gate; probe walks in-chat search with a hit and a miss,
formatting rendering on the fixtures (bold/code/link/spoiler), reaction
toggle, pinned bar jump, forward dialog to another chat, viewer open/next/
close, ticks flip on the mock `ReadOutbox` event; screenshots of a formatted
message, the viewer, the forward dialog.

---

## 5. Package 5D — Regular-use functions (UI worker)

Files: new `src/ui/info_panel.rs`, `src/ui/stickers.rs`, `src/ui/contacts.rs`,
`src/ui/newgroup.rs`, `src/ui/recorder.rs` (UI only), plus edits to
messages.rs/shell.rs/chatlist.rs.

- **Info panel**: third column / overlay sheet (§0): avatar 96, title,
  status, username (COPY), phone, about; "Notifications" switch (mute
  forever/unmute); groups: members list (`get_members`, paged 50, role
  badge, presence; click → `open_user`); shared media tabs Photos (3-column
  grid of thumbnails, click → viewer) / Files / Links / Voice
  (`get_shared_media`, paged). `chat_info` action + INFO button toggle it;
  state persisted.
- **Stickers & GIFs**: STICKER glyph next to EMOJI opens a popover with
  tabs: Recent, Favorites, each installed pack (`get_sticker_packs`), GIFs
  (`get_saved_gifs`). Grid of 64px cells, lazy `download_sticker`/
  `download_gif` with epoch guards; animated stickers show a muted "tgs"
  cell and are not sendable. Click sends (`send_sticker` / `send_gif`).
- **Voice recording**: MIC click → `record_start`; composer becomes a
  recording bar: red dot, mm:ss timer, "Cancel", SEND → `record_stop` then
  `send_voice`. Errors shown inline (ffmpeg missing → message with the
  pacman command).
- **Contacts**: dialog (from main menu / `contacts` action): search entry +
  list (name, presence, avatar) → click `open_user` → opens the chat.
  Empty state "No contacts".
- **New group**: dialog: title entry, member picker (contacts with
  checkboxes, search), "Create" → `create_group` → opens the chat.
- **Multi-select**: context menu "Select" → selection mode: checkbox at the
  left of every row, click toggles, action bar replaces the composer:
  "N selected", Forward, Delete (own only; confirm), Copy (texts joined
  with newlines), Cancel/Esc. Row removal rules from CLAUDE.md apply.
- **File captions**: attach → after choosing a file, a small dialog with the
  file name and a caption entry → `send_file(caption)`.
- **Per-chat notifications**: the mute submenu from 2.2 + the info panel
  switch; muted chats never trigger desktop notifications (shell check).
- **Saved messages** menu item / action opens the `ChatKind::Saved` dialog
  (create the row if not loaded, via `open_user(me.id)`).

Acceptance: §2.6 gate; probe walks the info panel (members + shared media
tabs), sticker popover (send one), the recording bar with the mock recorder
(start/cancel/start/stop/send), contacts → open chat, new group, multi-select
forward+copy+cancel; screenshots of the info panel, sticker popover,
recording bar, contacts dialog.

---

## 6. Rules carried over and new invariants

C1–C15 from `specs/spec-ui.md` and D1–D3 from wave 2 apply unchanged.
New:
- **D4 draft debounce**: at most one `save_draft` in flight per chat; the
  latest text wins; switching chats flushes immediately.
- **D5 dialogs reload coalescing**: `DialogsChanged` schedules one
  `get_dialogs` per 300ms window; reconciliation keeps selection and scroll
  (C10); a reload never clears the search results view.
- **D6 selection mode**: entering selection mode cancels reply/edit; new
  incoming rows are selectable; leaving mode removes every checkbox before
  any row teardown; deleted rows drop out of the selection set.
- **D7 overlays** (viewer, sheets, dialogs): exactly one open at a time;
  Esc closes the topmost; focus returns to where it was; chat switch closes
  them all.
- **D8 epoch on every async fill**: avatars, thumbnails, stickers, members,
  shared media, search — every async result carries the generation it was
  requested under and is dropped if stale (as media already does).
- **D9 menus**: popovers are stored in one slot per owner and
  popdown+unparent before teardown (existing helper); no menu survives a
  chat switch.

---

## 7. Mock fixtures (orchestrator provides; workers may extend)

Chats: Marta (User, online, has_photo via the sample image), Deni (User,
last seen 02:40, muted, draft "let me check"), Mom (User, LastWeek, pinned),
Arch Linux ARM (Group, 3 unread, 1 mention, 42 members; messages from three
senders with name colors), Omarchy News (Channel, views, a forwarded post,
pinned message, web preview), Saved (Saved), Old project (Group, archived).
Folders: "Work" (Deni, Arch Linux ARM), "Family" (Mom). Messages with spans
(bold, italic, code, pre, link, spoiler, mention), a video document, a gif,
an audio file, a voice with duration. Contacts: 5. Sticker packs: "recent"
(3 stickers from the sample image), one pack "Omarchy" (4), one animated
marker. GIFs: 2 (paths None → card fallback). `get_me` = "Leo Test".
Behaviors: search is substring over texts; forward copies messages with
`forwarded_from`; reactions toggle `chosen`; `set_*` flags mutate and emit
`DialogsChanged`; a `ReadOutbox` for the open chat fires 2s after every send;
presence flips Marta offline/online every 20s; `record_*` produce a fake
1-second OGG in the runtime dir. Demo triggers ("delete"/"edit") stay.

---

## 8. Probe

`--smoke --probe` must traverse every new surface (each package extends
`src/ui/shell.rs::probe` with `OMG_PROBE_TRACE` step names). New env hooks:
`OMG_SMOKE_OPEN=<title>` opens a chat at start (screenshots),
`OMG_MOCK_NEED_CREDS=1` starts the mock at NeedCredentials. The 45s
failsafe stays; if the traversal exceeds ~35s, split it behind
`OMG_PROBE_PART=a|b` and the gate runs both.

---

## 9. Delegation plan

- Phase 0 (orchestrator, done first): §1 types + mock + config/settings/
  uistate/recording modules + Cargo `markdown` feature + CSS color tokens.
  Real backend implemented in parallel with phase 1.
- Phase 1 (parallel worktrees): 5A (codex xhigh) and 5B (kimi, thinking on).
  Spec pre-review by codex before either starts.
- Phase 2: 5C (after 5A merge), then 5D (after 5C merge) — both touch
  messages.rs/shell.rs heavily; serial avoids merge hell. Worker choice by
  quota at the time.
- Each package: verifier → cross-reviewer (grok for codex, codex for kimi) →
  fix round → orchestrator screenshot review → merge --no-ff.
