# Spec: Wave 3 — Assistant + Omarchy virtual chats, transcription, AI actions

## Context — read first
- `src/local/mod.rs`: `Local` handle (create ONE in Shell via `Local::spawn()`):
  `detect(prefs)`, `chat(prefs, system, messages)`, `transcribe(prefs, path)`,
  `os_run(action, args)`, `os_shell_confirmed(cmdline)`. All async; await on
  the GLib main context like `Tg`.
- `src/ai/mod.rs`: `Prefs` (build from `Settings.ai`: chat_provider,
  transcribe_provider, chat_model, ollama_url), `ChatMessage{role, content}`,
  `Role::{User,Assistant}`, `ProviderInfo{id, task, available, detail}`.
  `src/ai/prompts.rs`: `ASSISTANT`, `catch_up(..)`, `draft_reply(..)`,
  `translate(..)`, `summarize(..)`, `search(..)` — each returns
  `(system, user_text)`.
- `src/os/mod.rs`: `catalog(&settings.os.actions) -> Vec<Action>`,
  `parse(line) -> Parsed::{Help, List{filter}, Run{name,args}, Shell(cmd), Empty}`,
  `help_text(&actions)`, `list_text(&actions, filter)`. `Action` is Clone.
- `src/settings.rs`: `Settings.ai.{enabled, transcribe_auto, ...}`,
  `Settings.os.{enabled, shell, actions}`.
- Waves 1–2 are merged: settings page, flags, anti-delete, edit history.
- Design: CLAUDE.md; concurrency rules: specs/spec-ui.md C1–C15.

## The two virtual chats (local — never touch Telegram)
Sentinel ids (define in `src/ui/virtual_chat.rs`):
`ASSISTANT_CHAT: i64 = i64::MIN + 1`, `OMARCHY_CHAT: i64 = i64::MIN + 2`.
- Sidebar: two rows at the TOP of the chat list (class `omg-chat-row` +
  `omg-virtual`; titles "Assistant" and "Omarchy"; preview = last exchange
  line; no unread badge). "Assistant" shows only when `settings.ai.enabled`;
  "Omarchy" only when `settings.os.enabled`. They appear/disappear live on
  settings change. They are NOT part of `get_dialogs`; ChatList gains
  `set_virtual(rows: Vec<(i64, &str, &str)>)` that (re)inserts them at the
  top without disturbing real rows (C7: no reopen, no selection callback).
- Opening one uses the normal MessagesView with a per-virtual-chat in-memory
  store (Vec<Msg>) kept in Shell (`virtual_stores: RefCell<HashMap<i64, Vec<Msg>>>`);
  ids are negative counters; `sender` "You"/"Assistant"/"Omarchy";
  `chat_title` set; no media; times = now. Reopening shows the transcript
  again (memory only; not persisted — decided).
- Composer sends in a virtual chat route to `Local`, not `Tg`. While a
  request is in flight show the typing slot "thinking…" (Assistant) or
  "running…" (Omarchy); the composer stays enabled (multiple requests may be
  in flight; replies append in order of arrival). Replies that arrive after
  the user switched away are appended to that virtual store anyway (C1
  applies only to the VIEW; the store always updates).
- Right-click on a virtual message offers only Copy.

### Assistant behaviors (text you type in the Assistant chat)
- `/help` → the command list below. `/status` → `local.detect(prefs)` rendered
  as one line per provider: `id (task): available/unavailable — detail`.
- `/catchup [chat title]` → the named real chat (case-insensitive substring
  over sidebar titles; default = the most recently opened real chat; none →
  error line) → `tg.get_history(chat_id, None)` → transcript lines
  `"[HH:MM] Sender: text"` (media → `[photo]` etc.) → `prompts::catch_up` →
  `local.chat` → reply message.
- `/translate <lang>` (no text) → error "select a message first" — the
  message form is the context-menu action below. `/translate <lang> <text>`
  → translate text.
- `/summarize <text>` → summary. `/search <question>` → over the last 50
  messages of up to 10 most recent real chats (fetch with `get_history`,
  concurrently, C14-safe), candidates formatted `"[chat] HH:MM Sender: text"`
  → `prompts::search` → reply.
- Anything else → `prompts::ASSISTANT` chat with the last 20 turns of this
  virtual chat as `messages` (User/Assistant roles).
- If `local.chat` returns Err → an incoming message with the error text and,
  when it mentions "no chat provider", a one-line hint: add
  `anthropic_api_key = "…"` (or openai/groq/gemini) under `[ai]` in
  `~/.config/omarchygram/config.toml`, or run `ollama serve`.

### Omarchy behaviors
- Each typed line → `os::parse`. Help/List → text from `help_text`/`list_text`.
  `Run{name,args}` → find in `os::catalog(&settings.os.actions)` (exact name,
  else case-insensitive unique prefix, else "unknown action, try `list`") →
  `local.os_run(action, args)` → output as an incoming message rendered
  monospace-preformatted (class `omg-msg-mono`, no wrapping changes: text
  wraps at word/char as usual but keeps newlines).
- `Shell(cmd)`: if `!settings.os.shell` → incoming message "shell commands are
  off — enable 'Allow shell commands' in Settings". Else a confirmation
  `gtk::AlertDialog` (message = the exact command line, buttons Cancel / Run,
  default Cancel) → only on Run: `local.os_shell_confirmed(cmd)`. The dialog
  is the ONLY path to os_shell_confirmed.
- First open of the Omarchy chat with an empty store → seed with the help
  text as an incoming message.

### Real-chat AI actions (context menu + voice)
- Context menu on a real message (only when `settings.ai.enabled`):
  "Draft reply with AI" → recent transcript of that chat (store contents) +
  target → `prompts::draft_reply` → puts the text into the composer with a
  bar (`omg-edit-bar`, text "AI draft — Enter sends, Esc discards"); Esc
  clears composer + bar. "Translate" → `prompts::translate(text, "English")`
  (the target language = `settings.ai.translate_to` if you add it? NO — keep
  "English" for now; decided) → result shown under the message in an
  `omg-msg-quote` block prefixed "translation: ". "Summarize" (only for
  messages > 300 chars) → same block prefixed "summary: ".
- Voice messages: the voice pill gets a second small button "transcribe"
  (`omg-attach` style) when `ai.enabled`; click → download (existing media
  path) → `local.transcribe(prefs, path)` → text under the pill in an
  `omg-msg-quote` block prefixed "transcript: "; Err → `omg-error` line.
  With `ai.transcribe_auto` on, do this automatically when the message is
  rendered (once per msg id; cache the result in the store; C6-style state
  machine NotStarted/InFlight/Done/Failed).

## Files to create/modify (ONLY these)
`src/ui/virtual_chat.rs` (NEW: sentinels, store types, command handling
helpers), `src/ui/shell.rs`, `src/ui/messages.rs`, `src/ui/chatlist.rs`,
`src/ui/mod.rs`, `src/theme/style.css` (`omg-virtual` accent-tinted title,
`omg-msg-mono` monospace block — tokens only).

## Probe traversal additions
The temp settings for probe runs start with defaults; the probe must first
`store.update(|s| { s.ai.enabled = true; s.os.enabled = true; })`, then:
open "Omarchy", send `help`, await an incoming row containing "Omarchy control";
send `status`, await a non-empty reply; send `run echo hi` and assert the
reply says shell commands are off (os.shell false); open "Assistant", send
`/status`, await a reply containing "ollama" (detect works offline: it only
probes localhost with a 2s timeout). Then continue to quit. No network chat
in the probe (no keys).

## Must NOT touch
`src/tg/*`, `src/ai/*`, `src/os/*`, `src/local/*`, `src/settings.rs`,
`src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`, `Cargo.toml`, `tests/`,
`specs/`, git state (NO commits). No new dependencies.

## Acceptance criteria (run them, report results)
Same as spec-wave1.md (build 0 warnings, both probes exit 0 within 40s,
tests, color/threading greps, real settings.toml untouched) plus:
`grep -n "os_shell_confirmed" src/ui/*.rs` shows exactly ONE call site, inside
the AlertDialog response handler.
