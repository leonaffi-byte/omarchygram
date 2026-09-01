# Spec: Omarchygram UI (Rust/gtk4-rs) — chat list, messages, media, replies, keyboard-first, notifications
# v2 — revised after adversarial spec review; the Concurrency rules section is load-bearing.

## Context — read these files first
- `src/tg/mod.rs` — the ONLY backend interface: `Tg` handle (async methods),
  `Event` stream, `AuthState`, `ChatSummary`, `Msg` (note `chat_title`),
  `MediaKind`, `Reaction`, `SETUP_HELP`. Errors are `Result<_, String>` with
  user-facing text. Backend processes data commands concurrently — responses
  can arrive out of order; the epoch rules below exist for that reason.
- `src/theme/style.css` — the complete design system (omg-* classes,
  including omg-attach, omg-bar-close, omg-menu, omg-menu-item, omg-danger).
- `src/main.rs` — Shell contract: `Shell::new(tg, probe: bool)` with a public
  `widget: gtk::Box` field and `pub async fn start(&self)`; main wraps Shell
  in `Rc`. `probe == true` means: run the scripted traversal (below) after
  start and quit the app on success; main.rs already installs a 20s failsafe
  that exits 1.
- `CLAUDE.md` — architecture + design rules.

Threading rules (violations = review failure): all UI work on the GTK thread;
call backend via `glib::MainContext::default().spawn_local(...)`. NEVER
`std::thread`, `tokio::*`, `std::sync::Mutex/RwLock` in `src/ui/`. State via
`Rc<RefCell<…>>`. ONE narrow exception: image decoding may use
`gio::spawn_blocking` (it touches no widgets) as described under Media.

Run: `cargo run -- --smoke` (mock data, no login).
`OMG_MOCK_AUTH=1 cargo run -- --smoke` starts at the phone screen (code `2fa`
routes via the password screen; any other code signs in). Mock chat "Marta"
contains a photo, a sticker, a reply, a reaction, and one older pagination
page; "Deni" has an edited message and a document (downloadable); "Mom" has a
voice message (download returns Ok(None) → show unavailable). Sending in any
mock chat triggers Event::Typing after 0.8s and an incoming Event::NewMessage
after 2s (persisted to mock history, so a reload agrees with the live view).

## Files to create/modify (ONLY these)
- `src/ui/mod.rs` — declare the new modules.
- `src/ui/shell.rs` — REWRITE (keep the `Shell::new(tg, probe)` / `widget` /
  `start` contract). `gtk::Stack`: AuthView ↔ MainView, inside a
  `gtk::Overlay` (for the switcher). On `AuthState::NeedCredentials` show
  `SETUP_HELP` in a selectable label (class `omg-empty-state`). Owns all
  wiring in "Behaviors" and the state described in "Concurrency rules".
- `src/ui/auth.rs` — steps phone → code → password (password only when a
  submit returns NeedPassword). Each step: title (`omg-auth-title`), one-line
  hint (`omg-auth-hint`), one `gtk::Entry`, one continue button
  (`omg-primary`). Enter submits. Err(String) → text in an `omg-error` label.
  ONE submit in flight max: while in flight the Entry AND button are
  insensitive (Enter therefore inert); re-enabled on error. READY handling is
  idempotent (see Concurrency rule C8).
- `src/ui/chatlist.rs` — sidebar (`omg-sidebar`, width 280 fixed):
  `gtk::ListBox` in `gtk::ScrolledWindow`. Row (`omg-chat-row`): line 1 title
  (`omg-chat-title`, ellipsize END) + time right (`omg-chat-time`; `HH:MM` if
  today else e.g. `Aug 30`); line 2 preview (`omg-chat-preview`, single line,
  ellipsize END) + unread badge (`omg-unread`, hidden when 0). Rows are STABLE
  objects keyed by chat id — reordering moves the same row widget; internal
  reorders/updates must not emit user-visible selection callbacks and must not
  reopen the already-selected chat (C7). Public API:
  `set_chats(Vec<ChatSummary>)`,
  `upsert(chat_id, title: &str, preview: &str, time, unread: UnreadUpdate)`
  (creates the row if missing — title from `Msg::chat_title`, "Unknown" if
  empty; moves to top), `clear_unread(chat_id)`, `select_next()`,
  `select_prev()` (clamp at ends, no wrap), `selected() -> Option<i64>`,
  `ordered() -> Vec<(i64, String)>`. Define
  `pub enum UnreadUpdate { Set(i32), Delta(i32) }` in this module.
- `src/ui/messages.rs` — header (`omg-chat-header`): chat title + typing slot
  (`omg-typing`, "NAME is typing…" / "typing…" when the name is empty).
  Scrollable message list (`omg-messages`), composer (`omg-composer`).
  Message widget (`omg-msg` + `omg-msg-in`/`omg-msg-out`; incoming halign
  Start, outgoing halign End, hexpand false; text wraps WORD_CHAR,
  max-width-chars ~60, selectable):
  - quote block (`omg-msg-quote`) when `msg.reply_to` is set: sender + first
    ~60 chars of the quoted msg if in the current store, else "replied message";
  - sender name (`omg-msg-sender`) when non-empty;
  - media block (below) when `msg.media` is Some;
  - text (omit the label entirely when text is empty);
  - time (`omg-msg-time`) + the word `edited` (same class) when `msg.edited`;
  - reactions row: per Reaction a label "EMOJI COUNT" (count omitted when 1),
    class `omg-reaction`.
  Message rows are STABLE per msg_id: MessageChanged updates the existing
  row's text/edited/reactions labels in place — it must NOT rebuild the row,
  its gestures, or its media widget, and must not restart downloads (C6).
- `src/ui/switcher.rs` — Ctrl+K overlay (`omg-switcher`, ~420 wide, centered
  top via the Shell's `gtk::Overlay`): entry + `gtk::ListBox` filtered by
  case-insensitive substring of title, sidebar order. First row selected
  initially and after every filter change; Up/Down moves; Enter opens the
  selected chat (inert when no rows); Esc closes. Entry pre-focused on open.

## Concurrency & correctness rules (load-bearing — each is a review item)
- C1 **Open epoch.** Shell keeps `epoch: Cell<u64>`, incremented on EVERY chat
  open/switch/close. Every spawned completion (history, send, edit, delete,
  mark_read, download that touches the open view) captures
  `(chat_id, epoch)` at spawn and, on completion, applies view changes ONLY if
  the epoch is still current. Sidebar reconciliation (C10) applies regardless
  of epoch.
- C2 **Message store.** Per open chat keep `IndexMap`-like ordered state:
  `Vec<msg_id>` + `HashMap<msg_id, (Msg, row_widget)>` (in Rc<RefCell<…>>).
  History pages and events MERGE by msg_id (insert sorted by id if absent,
  update in place if present) — never wholesale-replace the displayed list.
  This makes the "event arrives during initial history load" race harmless.
- C3 **Pagination.** Per open chat: `paging: bool`, `exhausted: bool` (reset
  on every open). Trigger when the vadjustment reaches the top with tolerance
  (`value <= 4.0`), only if `!paging && !exhausted`. On response: dedupe by
  msg_id against the store, prepend, set `exhausted` on an empty page, clear
  `paging` on success AND error. Scroll restore: capture `upper`+`value`
  immediately before inserting; restore in a ONE-SHOT `notify::upper` handler
  (`value = saved_value + (new_upper - saved_upper)`); suppress pagination
  triggers until restored.
- C4 **Bottom anchoring.** Track `stick_to_bottom: bool` — true when
  `value >= upper - page_size - 4.0` (recompute on user scroll). Initial and
  reopen loads ALWAYS scroll to newest (after layout, via one-shot
  `notify::upper`). Later appends and any async size change (image swap-in)
  re-scroll to bottom only if `stick_to_bottom` was true before the change.
- C5 **Composer single-operation.** One operation at a time: while a send
  (text or file) is in flight, the TextView, Enter, attach button, and drop
  target are all inert. A send snapshots `(chat_id, epoch, text, reply_to)`.
  On success: always run sidebar reconciliation (C10) for the SNAPSHOT chat;
  append to the view only if epoch is current; clear the composer only if its
  content still equals the snapshot text. On error: `omg-error` line above
  the composer, everything re-enabled, text preserved.
- C6 **Media state machine.** Per `(chat_id, msg_id)` exactly one download
  state: NotStarted → InFlight → Done(path) | Failed. Kept in the store, not
  the widget, so re-renders and MessageChanged never duplicate downloads.
  A completed download applies to the widget only if that row is still in the
  current store (C1).
- C7 **Sidebar stability.** upsert on the selected chat must not fire a
  reopen; row selection callbacks fire only for user actions (click,
  select_next/prev, switcher).
- C8 **READY idempotence.** All three READY paths (start, code, password) go
  through one `on_ready()` that is guarded by a `started: Cell<bool>` — the
  event loop is spawned exactly once, dialogs load exactly once.
- C9 **Event loop independence.** `on_ready()` FIRST spawns the event loop,
  THEN loads dialogs. A dialogs failure shows an `omg-error` label with a
  "Retry" button (`omg-primary`) in the MainView; events keep flowing and may
  create rows via upsert meanwhile.
- C10 **Sidebar reconciliation.** After send success: upsert(chat, preview =
  text or media placeholder, time = now, Delta(0)). After edit success: if the
  edited message is the store's last for that chat, upsert with the new text.
  After delete success: remove from store; if it was the last, upsert with the
  new last message's preview (or "" when the store emptied). After
  NewMessage: upsert(preview, time, Delta(+1) unless the chat is open AND
  read was triggered (C11)).
- C11 **Unread & mark_read.** Open chat + window active → incoming messages
  are read: coalesced mark_read (at most one in flight per chat; if a newer
  message arrived during flight, send ONE follow-up), badge stays 0. Open
  chat + window NOT active → Delta(+1) like a background chat; connect to the
  window's `notify::is-active` and, when it becomes active with an open chat
  whose badge > 0, mark_read + clear. clear_unread only after a SUCCESSFUL
  mark_read whose chat is still open (epoch check); a failed mark_read leaves
  the badge.
- C12 **Typing timer.** `(chat_id, generation)` pair; each Typing event for
  the open chat bumps the generation and (re)schedules a 5s
  `glib::timeout_add_local_once` that clears the indicator only if its
  generation is still current; indicator cleared immediately on chat switch.
- C13 **Reply/edit lifecycle.** Chat switch cancels reply mode and edit mode
  (edit-cancel restores the pre-edit composer draft). Activating the attach
  button or a drop while reply mode is active first cancels reply mode (the
  backend cannot reply with a file). Esc: switcher → close; else reply/edit →
  cancel; else focus composer.
- C14 **No RefCell borrow across await.** Snapshot what you need, drop the
  borrow, await, re-borrow to apply (with the C1 epoch check). No exceptions.
- C15 **Notifications.** On NewMessage where (window not active) OR (chat not
  open): `gio::Notification` via the window's application
  (`send_notification(Some(&format!("chat-{chat_id}")), …)` so one chat
  coalesces): title = chat title (fallback sender), body = text or
  placeholder. Never for the open chat while active.

## Behaviors (Shell owns the wiring; all subject to the rules above)
- `start()`: `tg.start().await` → route stack; NeedPhone → AuthView; Ready →
  `on_ready()`. Backend Err → `omg-error` in a centered label with Retry.
- Open chat (click, switcher, Alt+Up/Down): bump epoch, reset C3 flags,
  build store from `get_history(chat_id, None)`, render, always scroll to
  newest, then C11 read handling. While history is in flight show
  `omg-empty-state` "Loading…" — a stale history response (epoch) is dropped.
- Send: Enter sends; Shift+Enter newline. Composer `gtk::TextView` (wrap
  WordChar, grows 1→~5 lines then scrolls).
- Attach: "+" button (`omg-attach`, plain text label) → `gtk::FileDialog`
  `open_future`. Dismissal/cancellation is NOT an error (no log, no UI).
  A `gio::File` with `path() == None` → `omg-error` "only local files can be
  sent". Otherwise `send_file(chat_id, path, caption)` with caption = composer
  text (C5 snapshot semantics). Drag-drop: `gtk::DropTarget` for
  `gdk::FileList`; take the FIRST file, same rules.
- Context menu: right-click a message (`gtk::GestureClick` button 3) →
  `gtk::Popover` (class `omg-menu`) with `omg-menu-item` buttons: Copy
  (clipboard set_text), Reply, Edit (own messages with text only), Delete
  (own only, class also `omg-danger`). Reply → `omg-reply-bar` above composer
  ("Reply to SENDER: SNIPPET" + close button `omg-bar-close`). Edit →
  `omg-edit-bar` ("Editing message"), composer pre-filled. Enter applies;
  results follow C1/C10. Delete: no confirm dialog.
- Media rendering:
  - Photo/Sticker: placeholder "loading image…" (`omg-media-placeholder`);
    C6 download; on Done(path): decode inside `gio::spawn_blocking` with
    `gdk::Texture::from_filename` (await the handle on the main context), then
    a `gtk::Picture` with that paintable: `set_can_shrink(true)`, hexpand
    false, halign per direction, and `set_size_request(w, h)` where (w, h) =
    texture size scaled to FIT within 320×320, never upscaled — with no
    expand flags this yields an exact allocation. Failure at any stage →
    "image unavailable" (same class). Click → open externally with
    `gio::AppInfo::launch_default_for_uri(&gio::File::for_path(&path).uri(), …)`;
    launch errors show an `omg-error` line.
  - Document: pill (`omg-doc-pill`) with `doc_name`; click → C6 download →
    launch default handler; pill insensitive while InFlight; Ok(None)/Failed →
    pill label gets " (unavailable)" and stays clickable for retry only after
    Failed (Ok(None) is final).
  - Voice: pill "voice message" — same click behavior as Document (mock
    returns Ok(None) → unavailable; real backend downloads the audio file).
- Keyboard (exact): Ctrl+K switcher; Alt+Down / Alt+Up next/previous chat
  (clamp); Esc per C13; Enter/Shift+Enter per composer. Sane Tab order.
- Errors: every awaited call handled per the rules; Err → eprintln + the
  designated `omg-error` label. The app never panics on backend errors.
  Exempt from error display: file-dialog cancellation.

## Probe traversal (`Shell::new(tg, probe=true)`, only reachable with --smoke)
After start: if OMG_MOCK_AUTH is set, programmatically submit phone "123",
code "2fa", password "x" through the real auth-view code paths. Then, driving
the real UI methods (not the backend directly): open the first chat; await
history render; open the chat titled "Marta"; trigger one pagination and await
its merge; send "probe message"; await the Typing event and the mock reply
appearing in the store; edit the sent message to "probe edited"; delete it;
then `app.quit()` (exit 0 — get the app via
`widget.root().and_downcast::<gtk::ApplicationWindow>().application()`).
Any step failing → `eprintln!` the step name and `std::process::exit(1)`.
Steps await real signals/events with short poll loops (`glib::timeout_future`)
— no fixed sleeps longer than 3s total per step. The 20s failsafe in main.rs
catches hangs.

## Design rules (verbatim from CLAUDE.md — deviations are review failures)
- Colors ONLY via the omg-* CSS classes; NO inline colors in ANY form (hex,
  rgb()/rgba(), named colors), NO libadwaita, no icons or emoji in UI chrome
  (the "+" attach label and emoji inside message/reaction content are fine).
  All needed classes exist in `src/theme/style.css`. If one is missing, ADD a
  rule there using ONLY existing `var(--...)` tokens — never new color values
  (dollar-placeholders belong only in `:root`).
- 8px spacing grid (4/8/12/16 margins). When unsure, plainer wins.

## Must NOT touch
`src/tg/`, `src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`, `Cargo.toml`,
`CLAUDE.md`, `tests/`, `specs/`, git state (NO commits — orchestrator owns
git). No new dependencies.

## Acceptance criteria (machine-checkable — run them, report results)
1. `cargo build` → exit 0 with ZERO warnings (`cargo build 2>&1 | grep -c "^warning"` prints 0).
2. `timeout 30 ./target/debug/omarchygram --smoke --probe` → exit 0 (this now
   proves the full traversal: open, paginate, send, typing+reply events, edit,
   delete), stderr free of panics.
3. `timeout 30 env OMG_MOCK_AUTH=1 ./target/debug/omarchygram --smoke --probe`
   → exit 0 (auth screens traversed programmatically).
4. `cargo test` → exit 0.
5. `grep -rEn "#[0-9a-fA-F]{3,8}|rgb\(|rgba\(" src/ui/` → no matches, and
   `grep -cE "#[0-9a-fA-F]{3,8}" src/theme/style.css` → 0.
6. `grep -rEn "std::thread|tokio::|std::sync::(Mutex|RwLock)" src/ui/` → no
   matches (`gio::spawn_blocking` for texture decode is the allowed exception).
