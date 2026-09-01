# Spec: tests, desktop entry, README

## Context
Omarchygram = Python 3.14 + GTK4 (PyGObject, system) + Telethon (venv) Telegram
client themed from Omarchy. Read `CLAUDE.md` for architecture. Venv already
exists: run everything with `.venv/bin/python`.

## Files to create (ONLY these)
1. `tests/test_theme.py` — for `omarchygram/theme/omarchy.py`:
   - `build_css(load_colors())` returns a string containing no `$` characters
     and containing every DEFAULTS hex value or theme override.
   - `load_colors()` with `COLORS_FILE` monkeypatched to a nonexistent path
     returns exactly DEFAULTS.
   - `load_colors()` with `COLORS_FILE` monkeypatched to a tmp toml defining
     `accent = "#123456"` returns that accent and DEFAULTS for missing keys.
   - Do NOT instantiate ThemeManager (needs a display) — pure functions only.
2. `tests/test_mock.py` — for `omarchygram/tg/mock.py` (use `asyncio.run` or
   pytest-style async helpers WITHOUT adding dependencies; plain
   `asyncio.run(main())` inside sync tests is fine):
   - `start()` returns READY normally; with env `OMG_MOCK_AUTH=1` returns
     NEED_PHONE, then phone→code("2fa")→NEED_PASSWORD→password→READY, and
     code("12345")→READY (use monkeypatch.setenv).
   - `get_dialogs()` sorted by last_time descending; 4 chats.
   - `send_text` appends to history; `get_history` returns it last.
   - The 1.5s echo reply: skip testing it (needs a running loop with time) OR
     test via `asyncio.get_event_loop().call_later` being irrelevant — simply
     call `_echo(1)` directly after registering `on_new_message` and assert the
     callback received a Message for chat 1.
3. `packaging/omarchygram.desktop` — Name=Omarchygram,
   Comment=Telegram client themed by Omarchy, Exec=omarchygram, Terminal=false,
   Type=Application, Categories=Network;InstantMessaging;
   (no Icon line yet).
4. `bin/install-desktop` — bash, executable: writes a copy of the .desktop file
   to `~/.local/share/applications/omarchygram.desktop` with Exec rewritten to
   the absolute path `<repo>/.venv/bin/python -m omarchygram` (derive repo root
   from the script location), then runs `update-desktop-database
   ~/.local/share/applications` if that command exists. Idempotent.
5. `README.md` — short and factual: what it is (one sentence), screenshot
   placeholder line, requirements (Arch: gtk4 python-gobject), setup (venv
   command from CLAUDE.md), Telegram API credential setup (summarize the steps
   from `omarchygram/tg/config.py` SETUP_HELP), run commands (normal and
   --smoke), how theming works (reads
   `~/.local/state/omarchy/current/theme/colors.toml`, live-retints on Omarchy
   theme switch). Plain language, no marketing tone, no emoji, no badges.

## Must NOT touch
Anything under `omarchygram/` (the package), `pyproject.toml`, `CLAUDE.md`,
`specs/`, git state (NO commits — orchestrator owns git).

## Acceptance criteria (machine-checkable)
1. `.venv/bin/python -m pytest -q` → exit 0.
2. `bash -n bin/install-desktop` → exit 0; file is executable.
3. `grep -q "^Exec=" packaging/omarchygram.desktop` → exit 0.
4. `README.md` exists and mentions `--smoke` and `my.telegram.org`.
