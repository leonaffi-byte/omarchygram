# Spec: Wave 3 — Assistant + Omarchy virtual chats, transcription, AI actions
# v2 — revised after adversarial spec review (authoritative virtual stores,
# per-chat in-flight counts, ticketed shell gate, aux caches that survive
# chat resets, request tokens, offline AI mock for probes).

## Context — read first
- `src/local/mod.rs`: `Local` handle (create ONE in Shell via `Local::spawn()`):
  `detect(prefs) -> Vec<ProviderInfo>`, `chat(prefs, system, messages) ->
  Result<ChatReply, String>` (use `.text`), `transcribe(prefs, path) ->
  Result<Transcript, String>` (use `.text`), `os_run(action, args) ->
  Result<String, String>`, `os_shell_confirmed(ticket) -> Result<String, String>`.
- `src/ai/mod.rs`: `Prefs` (from `Settings.ai`), `ChatMessage{role, content}`,
  `Role::{User, Assistant}`, `ProviderInfo{id, task, available, detail}`,
  `Task::label() -> "chat" | "transcribe"`. `src/ai/prompts.rs`: `ASSISTANT`,
  `catch_up`, `draft_reply`, `translate`, `summarize`, `search` → `(system, user_text)`.
  With `OMG_MOCK_AI=1` (set automatically by `--smoke`) every AI call answers
  offline: chat replies start with "(mock ai)", transcripts with
  "(mock transcript", `detect` lists a "mock" provider.
- `src/os/mod.rs`: `catalog(&settings.os.actions) -> Vec<Action>` (Clone),
  `parse(line) -> Parsed::{Help, List{filter}, Run{name,args}, Shell(cmd),
  Error(text), Empty}`, `help_text`, `list_text`, and the shell gate:
  `request_shell(cmd) -> Result<ShellTicket, String>` (single pending; Err
  while one is pending), `cancel_shell(ticket)`, `ticket.command()`. A ticket
  is the ONLY way to run shell and is single-use.
- Waves 1–2 merged. Design: CLAUDE.md; concurrency: spec-ui.md C1–C15.

## Shell-owned state (all in `src/ui/virtual_chat.rs` types, held by Shell)
```rust
pub const ASSISTANT_CHAT: i64 = i64::MIN + 1;
pub const OMARCHY_CHAT: i64 = i64::MIN + 2;
pub struct VirtualStore { pub msgs: Vec<Msg>, pub next_id: i32 /* starts i32::MIN+1, +1 per append */,
    pub in_flight: u32, pub mono_ids: HashSet<i32> /* rows rendered monospace */ }
pub struct AuxState { pub transcripts: HashMap<(i64,i32), ReqState<String>>,
    pub translations: HashMap<(i64,i32), ReqState<String>>, pub summaries: HashMap<(i64,i32), ReqState<String>>,
    pub draft_token: u64 /* bumps per draft request; only the latest applies */ }
pub enum ReqState<T> { InFlight, Done(T), Failed(String) }
```
`virtual_stores: RefCell<HashMap<i64, VirtualStore>>`, `aux: RefCell<AuxState>`
live in Shell and SURVIVE `reset_chat` (they are keyed by chat/msg id, not by
view). Ids are allocated only when a message is appended (monotonic
increasing, so MessagesView's id ordering == arrival order).

## The two virtual chats (local — never touch Telegram)
- Sidebar: rows "Assistant" (when `settings.ai.enabled`) and "Omarchy" (when
  `settings.os.enabled`) occupy a reserved prefix at the TOP. ChatList gains
  `set_virtual(rows)` AND every reorder path (`upsert`'s move-to-top,
  `set_chats`) inserts real rows AFTER the virtual prefix (C7 rules apply).
  Class `omg-chat-row omg-virtual`; preview = last line of the last message.
- Opening one: `messages.reset_chat` then render the store's msgs. The store
  is AUTHORITATIVE: every append (user line or reply) goes to the store first;
  if that virtual chat is currently open, the view is resynced from the store
  (append the new row). A reply arriving after the user switched away and
  back is therefore visible (no epoch check on the store; the view resync
  checks `open_chat == that virtual id`).
- Sends in a virtual chat bypass the real-send busy state entirely (route by
  a snapshotted "kind" BEFORE any busy check; `composer_operation` stays
  false; the composer remains enabled). `in_flight += 1` on send, `-= 1` on
  completion; the typing slot shows "thinking…"/"running…" while
  `in_flight > 0` for the OPEN virtual chat (recomputed on open/switch).
- Disabling `ai.enabled`/`os.enabled` while that virtual chat is open: bump
  epoch, clear the view to the "Select a chat" empty state, remove the row.
  Every dispatch re-reads the current setting (a stale menu/composer can not
  execute after the master switch is off).
- Right-click on a virtual message offers only Copy.

### Assistant behaviors
- `/help` → command list. `/status` → `local.detect(prefs)`: one line per
  provider `"{id} ({task.label()}): {available|unavailable} — {detail}"`.
- `/catchup [chat title]` → real chat by case-insensitive substring of
  sidebar titles (default: the most recently opened real chat; none → error
  line) → `tg.get_history(chat_id, None)` → transcript `"[HH:MM] Sender: text"`
  (media → `[photo]` etc.) → `prompts::catch_up` → `local.chat`.
- `/translate <lang> <text>` → translate. `/summarize <text>` → summarize.
  `/search <question>` → last 50 messages of up to 10 most recent real chats
  (concurrent `get_history`, C14-safe) → candidates `"[chat] HH:MM Sender:
  text"` → `prompts::search`.
- Anything else → `prompts::ASSISTANT` with the last 20 turns of this virtual
  chat as `messages`.
- `Err(e)` from `local.chat` → incoming message with `e`; if it contains
  "no chat provider", append one line: add `anthropic_api_key = "…"` (or
  openai/groq/gemini) under `[ai]` in `~/.config/omarchygram/config.toml`, or
  run `ollama serve`.

### Omarchy behaviors
- Each line → `os::parse`. `Error(t)` → incoming message `t`. Help/List →
  `help_text`/`list_text`. `Run{name,args}` → RE-CHECK `settings.os.enabled`
  → find in `catalog(&settings.os.actions)` (exact name, else unique
  case-insensitive prefix, else "unknown action `x` — try `list`") →
  `local.os_run` → incoming message rendered monospace (add its id to
  `mono_ids`; class `omg-msg-mono`: preserves newlines).
- `Shell(cmd)`: if `!settings.os.shell` → message "shell commands are off —
  enable 'Allow shell commands' in Settings". Else `os::request_shell(cmd)`;
  `Err(e)` → message `e`. With a ticket: `confirm_shell(ticket.command())`
  → `gtk::AlertDialog` (message = the exact command line, buttons Cancel /
  Run, default Cancel, cancel = index 0) awaited via `choose_future`. In the
  Run branch, RE-READ settings: require `os.enabled && os.shell`, else treat
  as cancel. Run → `local.os_shell_confirmed(ticket)` (the ONLY call site);
  Cancel / dialog closed / settings failed → `os::cancel_shell(ticket)`.
  Result → monospace message.
- First open of Omarchy with an empty store → seed with `help_text` as an
  incoming message (id allocated normally).

### Real-chat AI actions (context menu + voice)
- Context menu (items shown only when `ai.enabled`; re-checked at dispatch):
  - "Draft reply with AI": `aux.draft_token += 1`, capture `(token, chat_id,
    epoch, msg_id, composer_text_snapshot)`; recent store transcript + target
    → `prompts::draft_reply` → `local.chat`; on completion apply ONLY if token
    is current, epoch current, and the composer still equals the snapshot →
    composer text = reply, bar `omg-edit-bar` "AI draft — Enter sends, Esc
    discards" (Esc clears both). Otherwise discard.
  - "Translate" / "Summarize" (Summarize only for text > 300 chars): keyed by
    `(chat_id, msg_id)` in `aux.translations/summaries`; if InFlight → ignore
    the click; result rendered under the message in an `omg-msg-quote` block
    prefixed "translation: " / "summary: " whenever that row is (re)rendered
    while Done (so it survives reset_chat and reopen). Failed → `omg-error`
    line once; a later click retries.
- Voice messages: pill gets a "transcribe" button (`omg-attach` style) when
  `ai.enabled`. Click (or automatically on render when `ai.transcribe_auto`):
  if `aux.transcripts[(chat,msg)]` is InFlight/Done → no new request; else
  mark InFlight → obtain the file through the EXISTING media download state
  (C6): if Done(path) use it; if InFlight, register a one-shot continuation
  that runs when it completes (MessagesView gains `on_media_ready(msg_id, f)`;
  continuations are dropped on reset_chat and re-armed on next render since
  the aux state is still InFlight → re-arm rule: on render, InFlight with no
  media continuation registered → register again); then `local.transcribe`
  → Done(text) rendered as an `omg-msg-quote` block "transcript: ".

## Files to create/modify (ONLY these)
`src/ui/virtual_chat.rs` (NEW), `src/ui/shell.rs`, `src/ui/messages.rs`,
`src/ui/chatlist.rs`, `src/ui/mod.rs`, `src/theme/style.css` (`omg-virtual`,
`omg-msg-mono` — tokens only).

## Probe traversal additions (offline; OMG_MOCK_AI is set by --smoke)
First: `store.update(|s| { s.ai.enabled = true; s.os.enabled = true;
s.ai.ollama_url = "http://127.0.0.1:1".into(); })`. For every step capture the
store's last id BEFORE sending and assert the reply row has a GREATER id.
1) open Omarchy; the seed help row exists; send `help` → new incoming row
   containing "Omarchy control". 2) send `run echo hi` with os.shell off →
   row containing "shell commands are off". 3) `os.shell = true`; in probe
   mode `confirm_shell` must go through the same code path but take the
   scripted answer from a `probe_answer: Cell<Option<usize>>`: set Cancel →
   send `run echo hi` → assert no new monospace reply and that
   `os::request_shell("x")` now succeeds (ticket released; cancel it);
   set Run → send `run echo hi` → assert exactly one reply row "hi".
   Then `os.shell = false` + set Run → send `run echo no` → assert the
   re-check refused (reply says off) and nothing ran.
4) open Assistant; send `/status` → reply containing "mock (chat)";
   send `hello` → reply starting "(mock ai)"; send `/catchup marta` → reply
   containing "Thursday". 5) open "Mom"; click transcribe on the voice pill →
   quote block "transcript:" appears; click again → no second request
   (assert aux state unchanged). 6) right-click Marta's last message → "Draft
   reply with AI" → composer contains "(mock ai)" and the draft bar shows;
   Esc clears. 7) disable `os.enabled` while Omarchy is open → view shows the
   empty state and the row is gone. Then quit. Whole traversal < 45s.

## Must NOT touch
`src/tg/*`, `src/ai/*`, `src/os/*`, `src/local/*`, `src/settings.rs`,
`src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`, `Cargo.toml`, `tests/`,
`specs/`, git state (NO commits). No new dependencies.

## Acceptance criteria (run them, report results)
1–7 as spec-wave2.md (build 0 warnings, both probes < 45s, tests, greps,
real settings untouched) plus: 8. `grep -n "os_shell_confirmed" src/ui/*.rs`
→ exactly one call site, in the Run branch after `choose_future` resolved and
after the settings re-check. 9. `OMG_MOCK_LATENCY_MS=400` probe run → exit 0.
