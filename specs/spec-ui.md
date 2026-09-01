# Spec: Omarchygram UI (Rust/gtk4-rs) — chat list, messages, media, replies, keyboard-first, notifications

## Context — read these files first
- `src/tg/mod.rs` — the ONLY backend interface: `Tg` handle (async methods),
  `Event` stream, `AuthState`, `ChatSummary`, `Msg`, `MediaKind`, `Reaction`,
  `SETUP_HELP`. Errors are `Result<_, String>` with user-facing text.
- `src/theme/style.css` — the complete design system (omg-* classes).
- `src/main.rs` — how Shell is constructed; its contract must not change:
  `Shell::new(tg) -> Shell` with a public `widget: gtk::Box` field and
  `pub async fn start(&self)`. `Shell` may internally be `Rc`-based
  (main.rs wraps it in `Rc` and keeps it alive).
- `CLAUDE.md` — architecture + design rules.

Threading rules (violations = review failure): all UI work on the GTK thread;
call backend via `glib::MainContext::default().spawn_local(...)` awaiting `Tg`
methods; consume `tg.events` in ONE spawn_local loop owned by Shell. NEVER
`std::thread::spawn` or `tokio::spawn` in `src/ui/`. State via `Rc<RefCell<…>>`;
use `glib::clone!` (or manual clones of `Rc`) for closures.

Run: `cargo run -- --smoke` (mock data, no login).
`OMG_MOCK_AUTH=1 cargo run -- --smoke` starts at the phone screen (code `2fa`
routes via the password screen; any other code signs in). Mock chat "Marta"
contains a photo, a sticker, a reply, a reaction, and one older pagination
page; "Deni" has an edited message and a document; "Mom" has a voice message.
Sending in any mock chat triggers Event::Typing after 0.8s and an incoming
Event::NewMessage after 2s.

## Files to create/modify (ONLY these)
- `src/ui/mod.rs` — declare the new modules.
- `src/ui/shell.rs` — REWRITE (keep the `Shell::new` / `widget` / `start`
  contract). `gtk::Stack`: AuthView ↔ MainView. On `AuthState::NeedCredentials`
  show `SETUP_HELP` in a selectable label (class `omg-empty-state`). Owns all
  wiring in "Behaviors".
- `src/ui/auth.rs` — steps phone → code → password (password only when the
  state machine returns NeedPassword). Each step: title (`omg-auth-title`),
  one-line hint (`omg-auth-hint`), one `gtk::Entry`, one continue button
  (`omg-primary`). Enter submits. An Err(String) from a submit → show the text
  in an `omg-error` label, keep input editable; button insensitive while a
  submit is in flight. Root class `omg-auth`, centered, max width ~360px.
- `src/ui/chatlist.rs` — sidebar (`omg-sidebar`, width 280 fixed):
  `gtk::ListBox` in `gtk::ScrolledWindow`. Row (`omg-chat-row`): line 1 title
  (`omg-chat-title`, ellipsize END) + time right (`omg-chat-time`; `HH:MM` if
  today else e.g. `Aug 30`); line 2 preview (`omg-chat-preview`, single line,
  ellipsize END) + unread badge (`omg-unread`, hidden when 0). Selection
  callback with chat id. Public methods: set_chats(Vec<ChatSummary>),
  upsert(chat_id, preview, time, unread_delta_or_set), clear_unread(chat_id),
  select_next()/select_prev(), ordered_ids().
- `src/ui/messages.rs` — header (`omg-chat-header`): chat title + typing slot
  (`omg-typing`, "NAME is typing…" or "typing…" when name is empty; auto-clear
  after 5s, timer reset on repeat). Scrollable message list (`omg-messages`),
  composer (`omg-composer`). Message widget (`omg-msg` + `omg-msg-in`/
  `omg-msg-out`; incoming halign Start, outgoing halign End; text wraps
  WORD_CHAR, max-width-chars ~60, selectable):
  - quote block (`omg-msg-quote`) when `msg.reply_to` is set: sender + first
    ~60 chars of the quoted msg if currently displayed, else "replied message";
  - sender name (`omg-msg-sender`) when non-empty;
  - media block (below) when `msg.media` is Some;
  - text (skip the label entirely when text is empty);
  - time (`omg-msg-time`) + the word `edited` (same class) when `msg.edited`;
  - reactions row: per Reaction a small label "EMOJI COUNT" (count omitted
    when 1), class `omg-reaction`.
  Newest at bottom; autoscroll on load and on append IF the view was already
  at the bottom. Empty chat → centered `omg-empty-state` "No messages yet";
  no chat selected → "Select a chat".
- `src/ui/switcher.rs` — Ctrl+K overlay (`omg-switcher`, ~420 wide, centered
  via `gtk::Overlay` in Shell): entry + `gtk::ListBox` of chats filtered by
  case-insensitive substring of title, sidebar order. Up/Down moves selection,
  Enter opens, Esc closes. Entry pre-focused on open.

## Behaviors (Shell owns the wiring)
- `start()`: `tg.start().await` → route stack. When READY (from start or after
  auth): `get_dialogs` → chatlist, then run the event loop over `tg.events`:
  - `NewMessage(msg)`: if its chat is open → append + (if window `is_active()`)
    `mark_read`; else bump that chat's unread. Always upsert the chat row
    (preview, time, move to top). Desktop notification (`gio::Notification`,
    send via the window's `gtk::Application::send_notification`) when the
    window is not active OR the chat is not open: title = sender or chat
    title, body = text or `[photo]`-style placeholder. Never notify for the
    open chat while the window is active.
  - `MessageChanged(msg)`: if displayed, re-render that message widget in
    place (text/edited/reactions).
  - `Typing { chat_id, name }`: if that chat is open, show typing indicator.
- Open chat (click, switcher, Alt+Up/Down): `get_history(chat_id, None)`,
  render, `mark_read(chat_id)`, clear badge.
- Pagination: when the message pane scrolls to the top (vadjustment value hits
  0 / upper edge) with a chat open: `get_history(chat_id, Some(oldest_id))`;
  prepend and PRESERVE the visual position (capture `upper` before, restore
  `value + (new_upper - old_upper)` after). An empty page → stop asking for
  that chat until it is reopened.
- Send: Enter in composer sends; Shift+Enter inserts a newline. Composer is a
  `gtk::TextView` (wrap WordChar, grows 1→~5 lines then scrolls). While a send
  is in flight the TextView is insensitive; on Ok clear it and append the
  returned Msg; on Err show `omg-error` line above the composer.
  `send_text(chat_id, text, reply_to)` with reply id when in reply mode.
- Attach: "+" button (plain text label) in the composer → `gtk::FileDialog`
  (`open_future`); on a file: `send_file(chat_id, path, caption)` where
  caption = current composer text (cleared on success). Also accept drag-drop
  of files onto the message pane (`gtk::DropTarget` for `gio::File`).
- Context menu: right-click a message (`gtk::GestureClick` button 3) opens a
  `gtk::Popover` with a vertical box of flat buttons: Copy (clipboard
  `set_text`), Reply, Edit (only own messages with text), Delete (only own).
  Reply → reply bar (`omg-reply-bar`) above composer "Reply to SENDER:
  SNIPPET" + a close button; Esc cancels. Edit → edit bar (`omg-edit-bar`,
  "Editing message"), composer pre-filled; Enter applies `edit_text` and
  re-renders in place; Esc cancels and restores the composer. Delete →
  `delete_message` + remove the widget (no confirm dialog).
- Media rendering:
  - Photo/Sticker: placeholder label "loading image…" (`omg-media-placeholder`),
    then spawn_local `download_media(chat_id, msg.id)`. On Ok(Some(path)):
    load `gtk4::gdk::Texture::from_filename`; scale to fit within 320×320
    (never upscale) via `set_size_request` on a `gtk::Picture` (content-fit
    Contain); replace the placeholder. On Ok(None)/Err/texture failure →
    "image unavailable" (same class). Click a photo → open externally:
    `gio::AppInfo::launch_default_for_uri(&format!("file://{}", path), ...)`.
    Never download the same message twice (cache the result per widget).
  - Document: pill (`omg-doc-pill`) with `doc_name`; click → download, then
    launch default handler; pill insensitive while downloading.
  - Voice: pill "voice message" — click downloads and opens with the default
    handler (in-app playback is out of scope).
- Keyboard (exact bindings): Ctrl+K switcher; Alt+Down / Alt+Up next/previous
  chat; Esc closes switcher, else cancels reply/edit, else focuses composer;
  Enter/Shift+Enter as above. Sane Tab order.
- Errors: every awaited call handled; Err → eprintln + one-line `omg-error`
  label in the affected view. The app must never panic on backend errors.

## Design rules (verbatim from CLAUDE.md — deviations are review failures)
- Colors ONLY via the omg-* CSS classes; NO inline colors, NO new hex values,
  NO libadwaita, no icons or emoji in UI chrome (the "+" attach label and
  emoji inside message/reaction content are fine). All classes above exist in
  `src/theme/style.css`. If one you need is missing, ADD a rule there using
  ONLY existing `var(--...)` tokens — never new colors (dollar-placeholders
  belong only in `:root`).
- 8px spacing grid (4/8/12/16 margins). When unsure, plainer wins.

## Must NOT touch
`src/tg/`, `src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`, `Cargo.toml`,
`CLAUDE.md`, `tests/`, git state (NO commits — orchestrator owns git).
No new dependencies.

## Acceptance criteria (machine-checkable — run them, report results)
1. `cargo build` → exit 0 with ZERO warnings (`cargo build 2>&1 | grep -c "^warning"` prints 0 — the current "never read" warnings must disappear because the UI now consumes the whole tg contract).
2. `timeout 30 ./target/debug/omarchygram --smoke --probe` → exit 0, no panic/traceback on stderr.
3. `timeout 30 env OMG_MOCK_AUTH=1 ./target/debug/omarchygram --smoke --probe` → exit 0.
4. `cargo test` → exit 0.
5. `grep -rn "#[0-9a-fA-F]\{6\}" src/ui/` → no matches.
6. `grep -rEn "std::thread::spawn|tokio::spawn" src/ui/` → no matches.
