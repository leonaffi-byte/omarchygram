# Spec: Wave 2 — anti-delete rendering, edit history, ghost mode UI

## Context — read first
Wave 1 is merged (settings panel, store in Shell, flags forwarded). Backend
contract (`src/tg/mod.rs`): `Event::MessageDeleted{chat_id, msg_ids}`,
`Msg.deleted` (true for archived-deleted rows that `get_history` merges in
when the anti_delete flag is on), `Tg::get_edit_history(chat_id, msg_id) ->
Vec<MsgVersion{text, replaced_at}>`, `Msg.edited`.
Mock demo: in any mock chat, sending a message containing "delete" makes the
mock reply get deleted 2.5s after it arrives (Event::MessageDeleted); "edit"
makes it get edited 2.5s later (Event::MessageChanged + a stored version).
Chat "Deni" already has an archived-deleted message (id 200, shown only when
anti_delete is on) and an edited message (203) with one stored version.
Rules: CLAUDE.md design spec + spec-ui.md C1–C15.

## Files to modify (ONLY these)
- `src/ui/shell.rs`:
  - `Event::MessageDeleted`: if `settings.anti_delete` → for each id in the
    open chat's store call `messages.mark_deleted(id)` (row stays; see below)
    and do NOT change the sidebar preview; else keep the current behavior
    (remove + reconcile). Also: when anti_delete is toggled ON while a chat is
    open, reload that chat (bump epoch, get_history) so archived-deleted rows
    appear; toggled OFF → reload too (they disappear).
  - Context menu "Edit history" (only when `msg.edited` and
    `settings.edit_history`): `get_edit_history` → popover.
- `src/ui/messages.rs`:
  - `mark_deleted(msg_id)`: row gets class `omg-msg-deleted` (strikethrough
    text via CSS `text-decoration: line-through`, muted color) and a small
    "deleted" tag (`omg-msg-time` style) next to the time; store entry gets
    `deleted = true`; the context menu on a deleted row offers only Copy.
    Rows arriving from history with `deleted == true` render the same way.
  - Edit-history popover (`omg-history`): one row per version, oldest first:
    time (`omg-msg-time`) + text (wrap, selectable), then a final row
    "current" with the present text. Empty → single row "no earlier versions".
- `src/theme/style.css`: `omg-msg-deleted`, `omg-history` rules (tokens only).

## Probe traversal additions (after wave-1 steps)
Turn anti_delete on via the store; open "Deni"; assert a row with id 200
exists and has class `omg-msg-deleted`; send "please delete this"; await the
MessageDeleted event (≤6s) and assert the mock reply's row is present AND
marked deleted; turn anti_delete off; open "Deni" again; assert id 200 is
absent. Then send "edit this"; await MessageChanged (≤6s); open edit history
for that message; assert the popover lists ≥1 version. Then continue to quit.

## Must NOT touch
`src/tg/*`, `src/settings.rs`, `src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`,
`Cargo.toml`, `tests/`, `specs/`, git state (NO commits). No new dependencies.

## Acceptance criteria (run them, report results)
Same 7 as spec-wave1.md (build 0 warnings, both probes exit 0, tests, color
and threading greps, real settings.toml untouched).
