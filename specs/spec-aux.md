# Spec: tests, desktop entry, README (Rust)

## Context
Omarchygram = Rust + gtk4-rs + grammers Telegram client themed from Omarchy.
Read `CLAUDE.md` for architecture and commands. The crate builds as lib + bin;
tests import `omarchygram::…`. Backend contract: `src/tg/mod.rs` (`Tg`,
`Event`, `AuthState`, `ChatSummary`, `Msg`, `MediaKind`); theme functions:
`src/theme/mod.rs` (`load_colors()`, `build_css(&colors)`).

## Files to create (ONLY these)
1. `tests/theme.rs` — for `omarchygram::theme`:
   - `build_css(&load_colors())` output contains NO `$` character and contains
     every color value of the map it was built from.
   - `build_css` with a hand-built BTreeMap (all keys from load_colors()'s
     default result, `accent` set to `#123456`) contains `#123456`.
   - NOTE: `load_colors()` reads the live Omarchy state of the machine, so
     never assert specific colors from it — only structural properties (has
     the standard keys, values start with `#` or are "dark"/"light" for mode).
   - Do NOT touch GTK (no ThemeManager) — pure functions only.
2. `tests/mock_backend.rs` — for `Tg::spawn_mock()` (use `#[tokio::test]`;
   tokio is already a dependency with the macros feature):
   - `start()` → Ready. (Auth-flow env-var testing is NOT possible here since
     env vars race across tests — skip auth states.)
   - `get_dialogs()` → 4 chats, sorted by last_time descending.
   - `get_history(1, None)` → last message text contains "thursday";
     `get_history(1, Some(<first id>))` → 2 older messages;
     `get_history(2, Some(<first id of chat 2>))` → empty.
   - `send_text(1, "hi", None)` → Ok Msg with outgoing true; a subsequent
     `get_history(1, None)` contains it. After sending, two events arrive on
     `tg.events` (`Typing`, then `NewMessage`) within 5s — assert with
     `tokio::time::timeout`.
   - `edit_text` sets `edited`; `delete_message` removes;
     `download_media(1, 103)` → Ok(Some(path)) OR Ok(None) if the machine has
     no /usr/share/omarchy themes — accept both, but on Some the path exists.
3. `packaging/omarchygram.desktop` — Name=Omarchygram, Comment=Telegram client
   themed by Omarchy, Exec=omarchygram, Terminal=false, Type=Application,
   Categories=Network;InstantMessaging; (no Icon line yet).
4. `bin/install-desktop` — bash, executable: builds `cargo build --release`
   if `target/release/omarchygram` is missing, then writes
   `~/.local/share/applications/omarchygram.desktop` based on the packaging
   file with Exec rewritten to the absolute `target/release/omarchygram` path
   (derive the repo root from the script's own location), then runs
   `update-desktop-database ~/.local/share/applications` when available.
   Idempotent.
5. `README.md` — short and factual: what it is (one sentence); requirements
   (Arch: gtk4, rust); build/run commands from CLAUDE.md (normal, --smoke);
   one-time Telegram API credential setup (steps as in `SETUP_HELP` in
   `src/tg/mod.rs`: my.telegram.org → config.toml → chmod 600); the keyboard
   bindings table (Ctrl+K switcher, Alt+Up/Down chats, Enter send,
   Shift+Enter newline, Esc cancel/focus composer); how theming works (reads
   `~/.local/state/omarchy/current/theme/colors.toml`, live-retints on Omarchy
   theme switch); `bin/install-desktop` for a launcher entry. Plain language,
   no marketing tone, no emoji, no badges.

## Must NOT touch
Anything under `src/`, `Cargo.toml`, `CLAUDE.md`, `specs/`, git state
(NO commits — orchestrator owns git). No new dependencies.

## Acceptance criteria (machine-checkable — run them, report results)
1. `cargo test` → exit 0 (UI may still be the placeholder — do not test UI).
2. `bash -n bin/install-desktop` → exit 0; `test -x bin/install-desktop` → exit 0.
3. `grep -q "^Exec=" packaging/omarchygram.desktop` → exit 0.
4. README.md exists and mentions `--smoke` and `my.telegram.org`.
