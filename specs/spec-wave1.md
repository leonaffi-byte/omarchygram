# Spec: Wave 1 — settings panel + power tweaks (seconds, IDs, jump-to-date)

## Context — read first
- `src/settings.rs`: `Settings` (all fields, defaults), `SettingsStore` (Rc;
  `get()`, `update(|s| ..)` persists + notifies, `on_change(f)`, hot-reload
  when the file changes). `Settings::time_format()` gives the strftime for
  message times. Create ONE store in `Shell::new` (`SettingsStore::new()`).
- `src/tg/mod.rs`: `Tg::set_flags(BackendFlags{ghost_mode, anti_delete})`,
  `Tg::get_history_at_date(chat_id, date)`, `Msg.sender_id`, `Msg.deleted`.
- Existing UI: `src/ui/{shell,messages,chatlist,switcher,auth}.rs`,
  `src/theme/style.css` (omg-* classes; you may ADD rules using var() tokens).
- Rules: CLAUDE.md design spec + `specs/spec-ui.md` concurrency rules C1–C15
  still apply to every change here (epochs, no RefCell borrow across await).
- Run: `cargo run -- --smoke` (mock). Probe: `--smoke --probe` (scripted).

## Files to create/modify (ONLY these)
- NEW `src/ui/settings_view.rs`: a settings panel. Opened with **Ctrl+,** and
  closed with Esc (Esc precedence: switcher > settings > reply/edit cancel >
  focus composer). Presented as a `gtk::Stack` page "settings" swapped in
  place of the main view (title row "Settings" + Esc/Close button
  `omg-bar-close`). Sections (uppercase 10pt labels, class `omg-section`),
  each row = label + description (`omg-auth-hint` style) + `gtk::Switch`
  (class `omg-switch`) or `gtk::Entry`:
  - Timestamps: "Show seconds" (show_seconds), "Header clock" (header_clock),
    "Time format" (timestamp_format entry, placeholder "%H:%M").
  - Privacy: "Ghost mode" (ghost_mode), "Keep deleted messages" (anti_delete),
    "Keep edit history" (edit_history).
  - AI: "Enable AI features" (ai.enabled), "Auto-transcribe voice" (ai.transcribe_auto),
    "Chat provider" / "Transcribe provider" / "Chat model" entries, "Ollama URL".
  - Omarchy actions: "Enable" (os.enabled), "Allow shell commands" (os.shell).
  Every control writes through `store.update(..)` immediately (no Save button).
  Switches reflect external file changes via `store.on_change`.
- `src/ui/mod.rs`: declare the module.
- `src/ui/shell.rs`:
  - own the `SettingsStore`; on READY and on every change call
    `tg.set_flags(BackendFlags{ghost_mode, anti_delete})` (spawn_local, log Err).
  - Ctrl+, toggles the settings page; Esc closes it per precedence above.
  - Header clock: when `header_clock` is on, a label (`omg-clock`, tabular
    digits) in the chat header ticks every second (`glib::timeout_add_local`
    1s; keep ONE SourceId, remove when turned off or Shell dropped).
  - Ghost indicator: when `ghost_mode` is on show a small "ghost" pill
    (`omg-ghost`) in the chat header.
  - Context menu additions (via MessagesView action enum): "Copy message id"
    → clipboard `"chat {chat_id} msg {msg_id}"`; "Copy user id" (only when
    `sender_id` is Some) → clipboard the id.
  - Jump to date: a header button "Jump…" (`omg-attach` style) opens a
    `gtk::Popover` with a `gtk::Calendar`; picking a day → bump epoch, reset
    the chat store, `get_history_at_date(chat_id, that day 23:59:59 local)`,
    render like an initial load (C1/C3/C4), and set a `detached` flag. While
    detached, the ▼ jump button is always visible and clicking it reloads the
    latest page (`get_history(chat_id, None)`) and clears `detached`.
    Pagination upward from the jumped page keeps working (before_id).
- `src/ui/messages.rs`: render times with `settings.time_format()` (pass the
  format into the view; re-format every row's time label on settings change),
  add the two copy actions + "Jump…" + clock/ghost slots in the header, the
  `detached` behavior above.
- `src/ui/chatlist.rs`: row times stay `HH:MM`/`Mon d` (sidebar never shows
  seconds — decided).
- `src/theme/style.css`: add `omg-section`, `omg-switch` (accent when active,
  bg-lighter track, no Adwaita blue anywhere: set `background-image:none`),
  `omg-clock`, `omg-ghost` (accent border pill), `omg-settings` page bg — all
  from existing var() tokens only.

## Probe traversal additions (`probe == true`, after the existing steps)
Open settings (Ctrl+, path — call the same method), toggle show_seconds on,
assert the last message's time label now matches `\d\d:\d\d:\d\d`, toggle it
off, close settings; open chat "Marta", jump to today's date, assert the view
re-rendered (store non-empty), click ▼ to reload latest, assert detached is
cleared; then continue to `app.quit()`. Step failures → eprintln + exit(1).

## Must NOT touch
`src/tg/*`, `src/settings.rs`, `src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`,
`Cargo.toml`, `tests/`, `specs/`, git state (NO commits). No new dependencies.

## Acceptance criteria (run them, report results)
1. `cargo build` → exit 0, ZERO warnings.
2. `timeout 40 ./target/debug/omarchygram --smoke --probe` → exit 0, no panics.
3. `timeout 40 env OMG_MOCK_AUTH=1 ./target/debug/omarchygram --smoke --probe` → exit 0.
4. `cargo test` → exit 0.
5. `grep -rEn "#[0-9a-fA-F]{3,8}|rgb\(|rgba\(" src/ui/` → none; `grep -cE "#[0-9a-fA-F]{3,8}" src/theme/style.css` → 0.
6. `grep -rEn "std::thread|tokio::|std::sync::(Mutex|RwLock)" src/ui/` → none.
7. After a probe run, `~/.config/omarchygram/settings.toml` is UNCHANGED
   (smoke mode writes to a temp file; verify with a checksum before/after).
