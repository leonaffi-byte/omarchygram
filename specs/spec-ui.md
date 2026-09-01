# Spec: Omarchygram UI — full v0.2 (chat list, messages, media, replies, keyboard-first, notifications)

## Context
Python 3.14, GTK4 via PyGObject, asyncio runs ON the GLib main loop (gi.events) —
never create threads or a second event loop. The Telegram client contract is in
`omarchygram/tg/client.py` (`TgClient`, `AuthState`, `ChatSummary`, `Message`,
`Reaction`, `AuthError`); an offline duck-type twin is `omarchygram/tg/mock.py` —
read both files first. All client callbacks (`on_new_message`,
`on_message_changed`, `on_typing`) arrive on the GTK main thread; widgets may be
touched directly. Calling an async client method from a signal handler:
`asyncio.get_event_loop().create_task(...)`.

Run: `.venv/bin/python -m omarchygram --smoke` (mock data, no login).
`OMG_MOCK_AUTH=1 ... --smoke` starts at the phone screen (code `2fa` routes via
the password screen; any other code signs in). Mock chat "Marta" contains a
photo, a sticker, a reply, a reaction, and one older pagination page; "Deni"
has an edited message and a document; "Mom" has a voice message. Sending in any
mock chat triggers a typing signal after 0.8s and an incoming reply after 2s.

## Files to create/modify (ONLY these)
- `omarchygram/ui/shell.py` — REWRITE (keep `Shell(client)` ctor and
  `async def start()`; main.py depends on both). Gtk.Stack: AuthView ↔ MainView.
  On `NEED_CREDENTIALS`: show `omarchygram.tg.config.SETUP_HELP` in a
  selectable label (`omg-empty-state`). Owns all wiring listed below.
- `omarchygram/ui/auth.py` — NEW. Steps phone → code → password (password only
  when the state machine says so). Each step: title (`omg-auth-title`),
  one-line hint (`omg-auth-hint`), one entry, one continue button
  (`omg-primary`). Enter submits. `AuthError` → message in `omg-error` label,
  input stays editable; button insensitive while a submit is in flight.
  Root class `omg-auth`, centered, max width ~360px.
- `omarchygram/ui/chatlist.py` — NEW. Sidebar (`omg-sidebar`, fixed width 280):
  Gtk.ListBox in Gtk.ScrolledWindow. Row (`omg-chat-row`): title
  (`omg-chat-title`, ellipsize END) + time right (`omg-chat-time`, `HH:MM` if
  today else `MMM d`); below: preview (`omg-chat-preview`, single line,
  ellipsize END) + unread badge (`omg-unread`, hidden when 0). Selection
  callback with chat id. Public methods to: upsert a chat (new preview/time,
  bump to top, set unread), clear unread.
- `omarchygram/ui/messages.py` — NEW. Header (`omg-chat-header`): chat title +
  typing indicator slot (`omg-typing`, e.g. "Marta is typing…", auto-clears
  after 5s). Scrollable list (`omg-messages`), composer (`omg-composer`).
  Message widget (`omg-msg` + `omg-msg-in`/`omg-msg-out`; incoming halign
  start, outgoing halign end, max ~60 chars wide, wrap WORD_CHAR, text
  selectable):
  - optional quote block (`omg-msg-quote`) when `msg.reply_to` is set: sender +
    first ~60 chars of the quoted message if currently loaded, else "replied
    message"; 
  - sender name (`omg-msg-sender`) when non-empty;
  - media block (see Media below) when `msg.media`;
  - text; time (`omg-msg-time`) plus the word `edited` (same class) when
    `msg.edited`;
  - reactions row: one small label per Reaction, "EMOJI COUNT" (count hidden
    when 1), class `omg-reaction`.
  Newest at bottom; autoscroll on load and on append IF already at bottom.
  Empty chat → `omg-empty-state` "No messages yet"; no chat selected →
  "Select a chat".
- `omarchygram/ui/switcher.py` — NEW. Ctrl+K overlay: centered Gtk.Popover or
  overlay box (`omg-switcher`, ~420px wide) with an entry + Gtk.ListBox of
  chats filtered by case-insensitive substring of the title, ordered as the
  sidebar. Up/Down moves selection, Enter opens the chat, Esc closes. Opens
  pre-focused on the entry.

## Behaviors (Shell owns wiring)
- READY → load dialogs, select nothing, register the three client callbacks.
- Open chat: `get_history`, render, `create_task(client.mark_read(chat_id))`,
  clear badge.
- `on_new_message(msg)`: if its chat is open → append, mark_read if window
  `is_active()`; else increment badge. Always upsert the chat row (preview,
  time, bump to top). Desktop notification (Gio.Notification via
  `window.get_application().send_notification`) when window not active OR chat
  not open: title = sender/chat title, body = text or a `[photo]`-style
  placeholder. No notification for the open chat while the window is active.
- `on_message_changed(msg)`: if displayed, re-render that message in place
  (text/edited/reactions).
- `on_typing(chat_id, name)`: if that chat is open, show typing indicator 5s
  (reset the timer on repeat signals).
- Pagination: when the message scroll reaches the top and a chat is open,
  `get_history(chat_id, before_id=<oldest displayed id>)`; prepend, PRESERVE
  the visual scroll position (anchor trick: measure adjustment delta), stop
  asking once an empty page returns (per chat, reset on reopen).
- Send: Enter in composer sends (`send_text` with `reply_to` if reply mode),
  Shift+Enter inserts newline. Composer is a Gtk.TextView (min 1 line, grows to
  max ~5 lines, scrolls beyond). Entry disabled while sending; clear on
  success; append the returned Message.
- Attach: a button labeled "+" in the composer opens Gtk.FileDialog; chosen
  file → `send_file(chat_id, path)` (caption = current composer text, which is
  then cleared); append result. Also accept file drag-and-drop onto the
  message pane (Gtk.DropTarget for Gio.File).
- Context menu (right-click a message, Gtk.GestureClick button 3 +
  Gtk.PopoverMenu): Copy (text to clipboard); Reply; Edit (only own text
  messages); Delete (only own). Reply → reply bar (`omg-reply-bar`) above
  composer: "Reply to <sender>: <snippet>" + close button; Esc cancels. Edit →
  edit bar (`omg-edit-bar`, "Editing message"), composer pre-filled; Enter
  applies `edit_text` and re-renders in place; Esc cancels and restores.
  Delete → `delete_message` + remove the widget (no confirm dialog; personal
  client).
- Media rendering:
  - photo/sticker: placeholder `omg-media-placeholder` label "loading image…",
    then `create_task(client.download_media(chat_id, msg.id))` → on a path,
    swap in Gtk.Picture (content-fit contain, max width 320, max height 320);
    on None or error → label "image unavailable". Click a photo →
    `Gio.AppInfo.launch_default_for_uri` with the file URI. Never download the
    same message twice (keep the path on the widget).
  - document: pill (`omg-doc-pill`) with `doc_name`; click → download_media →
    launch default handler; while downloading, pill is insensitive.
  - voice: pill (`omg-doc-pill`) "voice message" — playback is out of scope,
    clicking downloads and opens with the default handler.
- Keyboard (Gtk.ShortcutController on the window, spec is exact):
  - Ctrl+K → switcher; Alt+Down / Alt+Up → next / previous chat in sidebar
    order; Esc → close switcher, else cancel reply/edit, else focus composer;
  - Enter send / Shift+Enter newline (composer only);
  - Tab order sane everywhere; entries/buttons reachable by keyboard.
- Errors: every await wrapped; failures print to stderr and show a one-line
  `omg-error` label in the affected view; the app never crashes.

## Design rules (verbatim from CLAUDE.md — deviations are review failures)
- Colors ONLY via CSS classes; NO inline colors, NO new hex values, NO
  Adwaita widgets (plain Gtk only), no icons or emoji in UI chrome (the "+"
  attach label and message-content emoji are fine). All classes above already
  exist in `omarchygram/theme/style.css`. If a class you need is missing, add
  a rule there using ONLY existing `var(--...)` tokens — never new colors.
- 8px spacing grid (4/8/12/16 margins). When unsure, plainer wins.

## Must NOT touch
`omarchygram/tg/*`, `omarchygram/theme/omarchy.py`, `omarchygram/main.py`,
`pyproject.toml`, `CLAUDE.md`, `tests/`, git state (NO commits — orchestrator
owns git).

## Acceptance criteria (machine-checkable)
1. `.venv/bin/python -c "from omarchygram.ui import shell, auth, chatlist, messages, switcher"` → exit 0.
2. `timeout 20 .venv/bin/python -m omarchygram --smoke --probe` → exit 0, stderr free of tracebacks.
3. `timeout 20 env OMG_MOCK_AUTH=1 .venv/bin/python -m omarchygram --smoke --probe` → exit 0, no tracebacks.
4. `.venv/bin/python -m pytest -q` → exit 0.
5. `grep -rn "#[0-9a-fA-F]\{6\}" omarchygram/ui/` → no matches (no inline colors).
6. `grep -rn "Adw\b\|libadwaita" omarchygram/ui/` → no matches.
