# Project
- Type: small  <!-- recorded at kickoff, never ask again -->
- Goal: Omarchygram — a from-scratch Telegram client for Omarchy that auto-themes from the active Omarchy theme.

# Commands (orchestrator runs these directly — never delegate)
- Build: `cargo build`
- Run: `cargo run` (real Telegram) / `cargo run -- --smoke` (offline mock data, no login)
- Test: `cargo test`
- Smoke check: `cargo run -- --smoke --probe` (scripted traversal of every feature on mock data — auth, chats, media, settings, anti-delete, virtual chats, animations — quits on success; exit 0 = pass, 45s failsafe)
- `OMG_MOCK_AUTH=1 cargo run -- --smoke` walks the login screens offline (code `2fa` routes via the password screen)
- Mock test hooks (wave 5): `OMG_MOCK_NEED_CREDS=1` starts at the credentials form; `OMG_MOCK_SLOW=GetDialogs,SearchGlobal` delays those backend commands 1.5s; `OMG_MOCK_FAIL_ONCE=SaveDraft,ForwardMessages` fails the first call of each (names = `Command` variants in `src/tg/mod.rs`); `OMG_UISTATE_PATH` points window/pane state at a throwaway file (smoke sets it automatically).
- Worker launches: ALWAYS `systemd-run --user --scope --quiet --collect -p TasksMax=infinity -- <codex|kimi …>` plus `setsid nohup … &` and a marker file (see global CLAUDE.md; unwrapped parallel builds killed the orchestrator session twice on 2026-09-02).
- HARD acceptance gate for any UI change (learned the hard way, 2026-09-02): `G_DEBUG=fatal-criticals ./target/debug/omarchygram --smoke --probe` ×6 + the `OMG_MOCK_AUTH=1` variant ×3 + `OMG_MOCK_LATENCY_MS=400` ×1 must all exit 0. A plain probe exits 0 even when GTK prints CRITICALs. `OMG_PROBE_TRACE=1` prints each traversal step (bisects crashes that have no Rust frame); `coredumpctl -1 debug` gives the backtrace.

# Architecture (decided 2026-09-01, orchestrator; Rust rewrite same day at user's request — Python v2 lives in git history)
- Rust + gtk4-rs (native GTK4 UI) + grammers 0.10 (MTProto) + tokio.
- Threading: the backend (grammers + tokio runtime) runs on its own thread (`src/tg/mod.rs::Tg::spawn`); the UI thread runs GTK. They talk ONLY via the `Tg` handle: async command methods (tokio oneshot under the hood) + an `Event` stream (async-channel). Both are safe to await on the GLib main context (`glib::MainContext::default().spawn_local`). UI code never sees grammers types and never spawns threads.
- grammers 0.10 specifics (they differ from older docs): `SqliteSession::open(path).await` (auto-persisting sqlite session), `SenderPool::new(session, api_id)` → spawn `runner.run()`, `Client::new(handle)`; `request_login_code(phone, api_hash)`; updates via `client.stream_updates(pool.updates, UpdatesConfiguration)`. Chat ids exposed to the UI are Bot-API dialog ids (`PeerId::bot_api_dialog_id_unchecked`).
- Theme source of truth: `~/.local/state/omarchy/current/theme/colors.toml` (semantic keys: mode, accent, background/dark_background/darker_background/lighter_background, foreground/light_foreground, muted, selection, red…). `src/theme/mod.rs` builds GTK CSS from `src/theme/style.css` (dollar-placeholder substitution) and re-applies via a Gio.FileMonitor on `~/.local/state/omarchy/current/theme.name` (150ms debounce).
- Config: `~/.config/omarchygram/config.toml` (api_id, api_hash; optional `[ai]` table: anthropic_api_key / openai_api_key / groq_api_key / gemini_api_key / whisper_model — env vars ANTHROPIC_API_KEY etc. are the fallback), chmod 600. User settings (non-secret toggles, all off by default): `~/.config/omarchygram/settings.toml`, hot-reloaded (`src/settings.rs`); `--smoke` runs use a throwaway file via `OMG_SETTINGS_PATH`.
- Local archive: `~/.local/share/omarchygram/archive.sqlite` (libsql, 0600, single-writer task in `src/tg/archive.rs`) — always on; powers anti-delete + edit history. Uses libsql (NOT rusqlite: a second bundled SQLite collides with grammers' at link time).
- Local services (`src/local/`): a second tokio thread hosting AI (`src/ai/`, providers auto-detected: ollama local, anthropic/openai/groq/gemini by key; transcription: whisper.cpp local via ffmpeg, groq, openai) and OS actions (`src/os/`: curated builtins + `[os.actions]` user commands + Omarchy tools auto-discovered from their `# omarchy:summary=` headers; 30s timeout; audit log `~/.local/state/omarchygram/os-audit.log`). The "Assistant"/"Omarchy" chats are LOCAL virtual chats — nothing goes to Telegram, so there is no remote command surface; arbitrary shell needs `os.shell` AND a confirmation dialog per command. Session: `~/.local/share/omarchygram/omarchygram.session` (sqlite), chmod 600. Media cache: `~/.cache/omarchygram/media/` (stickers auto-converted webp→png because GdkPixbuf here has no webp loader; animated .tgs stickers are reported unavailable).
- Telegram API creds come from the user's my.telegram.org account; never commit them.

# Design spec (orchestrator owns; workers implement verbatim)
- Reference tone: terminal-adjacent, flat, dense — closer to a TUI than to Telegram Desktop. When unsure, plainer wins.
- One typeface: "JetBrainsMono Nerd Font" everywhere. 8px spacing grid. No shadows, no rounded cards (4px max radius on bubbles), no gradients, no emoji in UI chrome.
- Colors ONLY from theme tokens via the omg-* CSS classes in `src/theme/style.css` (bg/bg-dark/bg-darker/bg-lighter/fg/muted/accent/selection/red). Never hardcode a color in UI code.
- Layout: left sidebar chat list (280px fixed), message pane, composer at bottom. One primary action per screen. Keyboard-first: Ctrl+K switcher, Alt+Up/Down chat nav, Enter send / Shift+Enter newline, Esc cancel/focus-composer.

# GTK4 lessons (verified crashes, 2026-09-02)
- Selectable `gtk::Label`s MUST stay keyboard-focusable: `set_can_focus(false)` on them makes `popover.popup()` on their row hit `gtk_widget_is_ancestor` on a dead widget (reproduced 6/6).
- Never remove rows while keyboard focus or a parented popover is inside them: call `MessagesView::move_focus_before_removal` + `dismiss_row_popovers` first (both exist; reuse them). Popovers parented to rows are stored in one slot each and popdown+unparent before any teardown.

# Known accepted limitations (decided 2026-09-01)
- Backend command/event channels are unbounded and data commands spawn freely — accepted at personal-client scale; revisit only if memory growth is ever observed.
- Animated (.tgs) stickers render as "image unavailable". Voice messages open in the default audio app, no in-app playback.
- A non-"wrong password" error during 2FA (e.g. network drop) requires an app restart — the server-side password token is consumed and the error message says so.
- Missing HOME/XDG dirs panic the backend with a clear message rather than degrade — never writable-relative-path session files.
- Downloaded document extensions are normalized to plain ascii, not whitelisted — opening is always an explicit user click; a whitelist would block legitimate files from contacts.

# Delegation notes
- Areas external agents must NOT touch: `src/tg/`, `src/ai/`, `src/os/`, `src/local/`, `src/settings.rs` (backend/auth/session/keys/process execution — orchestrator only), `src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`, `Cargo.toml`, any file containing credentials. `src/theme/style.css`: UI workers may ADD rules using existing var(--) tokens only.
- Commit tags: delegated commits end with [codex] / [grok] / [kimi] / [agy]
