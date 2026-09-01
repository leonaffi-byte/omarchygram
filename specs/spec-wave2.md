# Spec: Wave 2 — anti-delete rendering, edit history, ghost mode UI
# v2 — revised after adversarial spec review (tombstones, forced reload,
# ordering of flags vs history, background reconciliation, probe fixes).

## Context — read first
Wave 1 is merged (settings panel, `SettingsStore` in Shell, flags forwarded).
Backend contract (`src/tg/mod.rs`): `Event::MessageDeleted{chat_id, msg_ids}`;
`Msg.deleted` (true for archived-deleted rows that `get_history` merges in
when the anti_delete flag is on — ONLY within the id window of each fetched
page: `[oldest returned id, before_id)`; older deleted rows appear as you
paginate); `Tg::get_edit_history(chat_id, msg_id) -> Result<Vec<MsgVersion>, String>`
(`MsgVersion{text, replaced_at}`, oldest first, may be empty); `Msg.edited`;
`Tg::set_flags(BackendFlags) -> Result<(), String>`.
Mock backend: commands complete CONCURRENTLY (like the real one); set
`OMG_MOCK_LATENCY_MS=400` to force out-of-order completions. In any mock chat,
sending text containing "delete" makes the mock reply get deleted 2.5s after
it arrives; "edit" makes it edited 2.5s later (MessageChanged + a stored
version). Chat "Deni" has an archived-deleted message id 205 (inside the
first page window when anti_delete is on) and an edited message 203 with one
stored version. Rules: CLAUDE.md design spec + spec-ui.md C1–C15.

## Shell-owned state to add (`src/ui/shell.rs`)
- `settings_gen: Cell<u64>` — bumped on every settings change; async
  completions that captured an older gen are discarded.
- `tombstones: RefCell<HashMap<i64, HashSet<i32>>>` — per chat, ids known
  deleted while anti_delete is on. Applied on EVERY merge into the message
  store (initial load, pagination, `MessageChanged`, `NewMessage`): a merged
  Msg whose id is tombstoned is forced to `deleted = true` (monotonic; a
  stale `deleted: false` response can never resurrect a row). Cleared for a
  chat when anti_delete is turned off.

## Behaviors
- **Flags before data (D1).** On any settings change that affects
  `BackendFlags`: bump `settings_gen`, snapshot the flags, `await
  tg.set_flags(snapshot)`, and ONLY THEN, if the change flipped anti_delete
  and a real chat is open, run `force_reload(chat_id)`. A completion whose
  captured gen != current gen is discarded (no reload).
- **force_reload(chat_id) (D2)** — a new Shell method: bump epoch, `messages.reset_chat`
  (this already resets store, paging, exhausted, scroll hooks, loading label),
  then the normal initial `get_history` path (C1/C3/C4) including mark_read
  rules. It does NOT early-return when the chat is already open (unlike
  `open_chat`).
- **Event::MessageDeleted (D3):** if anti_delete is on → add ids to that chat's
  tombstones; if the chat is open, `messages.mark_deleted(id)` for ids in the
  store (rows stay); sidebar preview unchanged. If anti_delete is off → the
  existing remove+reconcile behavior. EITHER WAY, if any deleted id equals the
  tracked last-message id of a chat that is NOT open (`last_by_chat`), refresh
  that chat's preview by re-running `get_dialogs` (coalesced: at most one
  dialogs refresh in flight; a second request while one is in flight sets a
  "refresh again" flag).
- **mark_deleted(msg_id) (`src/ui/messages.rs`):** row gets class
  `omg-msg-deleted` (line-through, muted) and a small "deleted" tag next to
  the time; store entry `deleted = true`; context menu on a deleted row offers
  only Copy. Rows arriving with `deleted == true` render the same way.
- **Edit history:** context menu "Edit history" only when `msg.edited &&
  settings.edit_history`. Capture `(chat_id, epoch, msg_id, settings_gen)`;
  `await get_edit_history`; on completion re-validate: same epoch, row still in
  store and not deleted, `edit_history` still on — else drop silently. Popover
  (`omg-history`): one row per version, oldest first: time (`omg-msg-time`) +
  text (wrap, selectable); final row "current" with the text taken from the
  LIVE store at completion time. Empty versions → single row "no earlier
  versions". `Err(e)` → `omg-error` line (epoch-guarded).
- **Ghost mode:** the header "ghost" pill (wave 1) stays; nothing else — the
  backend suppresses receipts/online status itself.

## Files to modify (ONLY these)
`src/ui/shell.rs`, `src/ui/messages.rs`, `src/theme/style.css`
(`omg-msg-deleted`, `omg-history` — tokens only).

## Probe traversal additions (after wave-1 steps)
Use the store for every toggle. 1) `anti_delete = true` (await the
flags-before-data path to settle: poll until the reload's history rendered);
open "Deni"; assert row 205 exists with `omg-msg-deleted`. 2) capture
`last_id`; send "please delete this"; await `MessageDeleted` (≤6s) and assert
the mock reply row (id > last_id) is present AND marked deleted, and that
`tombstones[Deni]` contains it. 3) `anti_delete = false`; assert after the
reload that 205 is absent and the tombstone set for Deni is cleared.
4) `edit_history = true`; send "edit this"; await `MessageChanged` (≤6s) on a
row with id > the previous `last_id`; open edit history for it; assert the
popover lists ≥1 version and the "current" row shows the edited text.
5) Race check: set env-free path — call `force_reload` twice back-to-back and
assert exactly one final render (no duplicate rows, ids unique).
Then continue to quit. The whole traversal (waves 1+2) must stay under the
45s failsafe in main.rs.

## Must NOT touch
`src/tg/*`, `src/ai/*`, `src/os/*`, `src/local/*`, `src/settings.rs`,
`src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`, `Cargo.toml`, `tests/`,
`specs/`, git state (NO commits). No new dependencies.

## Acceptance criteria (run them, report results)
1. `cargo build` → 0 warnings. 2–3. both probes exit 0 within 45s, no panics.
4. `cargo test` → 0 failures. 5. color greps (as spec-wave1). 6. threading
greps (as spec-wave1). 7. real `~/.config/omarchygram/settings.toml`
unchanged by probe runs. 8. `OMG_MOCK_LATENCY_MS=400 timeout 60
./target/debug/omarchygram --smoke --probe` → exit 0 (out-of-order
completions must not break the traversal).
