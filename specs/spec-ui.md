# Spec: Omarchygram UI (chat list, messages, composer, auth)

## Context
Python 3.14, GTK4 via PyGObject, asyncio runs ON the GLib main loop (gi.events) —
never create threads or a second event loop. The Telegram client contract is in
`omarchygram/tg/client.py` (`TgClient`, `AuthState`, `ChatSummary`, `Message`,
`AuthError`); an offline duck-type twin is `omarchygram/tg/mock.py`. Client
callbacks (`on_new_message`) already arrive on the GTK main thread. To call an
async client method from a signal handler: `asyncio.get_event_loop().create_task(...)`.

Run with: `.venv/bin/python -m omarchygram --smoke` (mock data, no login).
`OMG_MOCK_AUTH=1 .venv/bin/python -m omarchygram --smoke` starts at the phone
screen (code `2fa` routes through the password screen; any other code signs in).

## Files to create/modify (ONLY these)
- `omarchygram/ui/shell.py` — REWRITE. `Shell(Gtk.Box)` keeps its constructor
  signature `Shell(client)` and its `async def start()` entry point (main.py
  depends on both). Internally: a `Gtk.Stack` switching between AuthView and
  MainView based on `AuthState`. On `NEED_CREDENTIALS` show
  `omarchygram.tg.config.SETUP_HELP` in a selectable label (class `omg-empty-state`).
- `omarchygram/ui/auth.py` — NEW. One view, three sequential steps:
  phone → code → password (password step only if state says so). Each step:
  title (`omg-auth-title`), one-line hint (`omg-auth-hint`), one `Gtk.Entry`,
  one continue button (`omg-primary`). Enter key submits. `AuthError` from
  submit_* shows its message in a label with class `omg-error` above the entry
  and leaves the input editable; the button shows a busy state (insensitive)
  while a submit coroutine is in flight. Root widget class: `omg-auth`,
  content centered, max width ~360px.
- `omarchygram/ui/chatlist.py` — NEW. `Gtk.ListBox` in a `Gtk.ScrolledWindow`,
  sidebar container class `omg-sidebar`, fixed width 280. Row (class
  `omg-chat-row`): line 1 = title (`omg-chat-title`, ellipsize END) + time
  right-aligned (`omg-chat-time`, format `HH:MM` if today else `MMM d`);
  line 2 = last-message preview (`omg-chat-preview`, ellipsize END, single line)
  + unread badge right (`omg-unread`, hidden when 0). Selecting a row emits a
  callback to Shell with the chat id.
- `omarchygram/ui/messages.py` — NEW. Vertical: header bar with chat title
  (`omg-chat-header`), scrollable message list (`omg-messages`), composer
  (`omg-composer`) = one `Gtk.Entry` (placeholder "Message") + send button
  (`omg-primary`, label "Send"). Enter in entry sends. Message widget (`omg-msg`
  plus `omg-msg-in`/`omg-msg-out`): sender name (`omg-msg-sender`, only for
  incoming in groups — always fine to show when non-empty), text (wrap
  WORD_CHAR, selectable), time (`omg-msg-time`). Incoming aligned start,
  outgoing aligned end, bubble max width ~70% of pane (use halign +
  a Gtk.Label with max-width-chars ~60 and wrap). Newest at bottom; auto-scroll
  to bottom on load and on append IF the view was already at the bottom.
  Empty chat selected → centered `omg-empty-state` label "No messages yet".
  No chat selected → centered `omg-empty-state` label "Select a chat".

## Behavior wiring (Shell owns it)
- `start()`: `state = await client.start()` → route stack. After auth reaches
  READY (from start or from auth flow): load dialogs into chatlist, register
  `client.on_new_message`: if msg.chat_id == open chat → append to messages
  view; always refresh that chat's row preview/time and increment its unread
  count unless the chat is open. Selecting a chat: `await get_history(chat_id)`,
  render, clear that row's unread badge.
- Send: optimistic is NOT required — `await send_text`, then append the
  returned Message and clear the entry (entry disabled while sending).
- All awaits guarded: exceptions from client calls → `print` to stderr and show
  a one-line `omg-error` label in the affected view; never crash the app.

## Design rules (verbatim from CLAUDE.md — deviations are review failures)
- Colors ONLY via the CSS classes above; NO inline colors, NO new hex values,
  NO extra CSS files, NO Adwaita widgets (plain Gtk only), no icons/emoji in
  UI text. All styling already exists in `omarchygram/theme/style.css` — the
  UI only attaches classes. If a needed class is missing, add a rule to
  style.css using ONLY existing `var(--...)` tokens (allowed exception to the
  file list; use dollar-placeholders never, they only belong in `:root`).
- 8px spacing grid (margins/spacing of 4/8/12/16). Keyboard: Tab order sane,
  Enter submits/sends, entries have placeholder text as labels.

## Must NOT touch
`omarchygram/tg/*`, `omarchygram/theme/omarchy.py`, `omarchygram/main.py`,
`pyproject.toml`, `CLAUDE.md`, `tests/`, git state (NO commits — orchestrator owns git).

## Acceptance criteria (machine-checkable)
1. `.venv/bin/python -c "from omarchygram.ui import shell, auth, chatlist, messages"` → exit 0.
2. `timeout 20 .venv/bin/python -m omarchygram --smoke --probe` → exit 0, stderr free of tracebacks.
3. `timeout 20 env OMG_MOCK_AUTH=1 .venv/bin/python -m omarchygram --smoke --probe` → exit 0, no tracebacks.
4. `.venv/bin/python -m pytest` → exit 0 (existing tests keep passing; adding UI tests not required).
5. `grep -rn "#[0-9a-fA-F]\{6\}" omarchygram/ui/` → no matches (no inline colors).
