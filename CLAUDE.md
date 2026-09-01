# Project
- Type: small  <!-- recorded at kickoff, never ask again -->
- Goal: Omarchygram — a from-scratch Telegram client for Omarchy that auto-themes from the active Omarchy theme.

# Commands (orchestrator runs these directly — never delegate)
- Build: `cargo build`
- Run: `cargo run` (real Telegram) / `cargo run -- --smoke` (offline mock data, no login)
- Test: `cargo test`
- Smoke check: `cargo run -- --smoke --probe` (opens themed window, auto-quits after 2s, exit 0 = pass)
- `OMG_MOCK_AUTH=1 cargo run -- --smoke` walks the login screens offline (code `2fa` routes via the password screen)

# Architecture (decided 2026-09-01, orchestrator; Rust rewrite same day at user's request — Python v2 lives in git history)
- Rust + gtk4-rs (native GTK4 UI) + grammers 0.10 (MTProto) + tokio.
- Threading: the backend (grammers + tokio runtime) runs on its own thread (`src/tg/mod.rs::Tg::spawn`); the UI thread runs GTK. They talk ONLY via the `Tg` handle: async command methods (tokio oneshot under the hood) + an `Event` stream (async-channel). Both are safe to await on the GLib main context (`glib::MainContext::default().spawn_local`). UI code never sees grammers types and never spawns threads.
- grammers 0.10 specifics (they differ from older docs): `SqliteSession::open(path).await` (auto-persisting sqlite session), `SenderPool::new(session, api_id)` → spawn `runner.run()`, `Client::new(handle)`; `request_login_code(phone, api_hash)`; updates via `client.stream_updates(pool.updates, UpdatesConfiguration)`. Chat ids exposed to the UI are Bot-API dialog ids (`PeerId::bot_api_dialog_id_unchecked`).
- Theme source of truth: `~/.local/state/omarchy/current/theme/colors.toml` (semantic keys: mode, accent, background/dark_background/darker_background/lighter_background, foreground/light_foreground, muted, selection, red…). `src/theme/mod.rs` builds GTK CSS from `src/theme/style.css` (dollar-placeholder substitution) and re-applies via a Gio.FileMonitor on `~/.local/state/omarchy/current/theme.name` (150ms debounce).
- Config: `~/.config/omarchygram/config.toml` (api_id, api_hash), chmod 600. Session: `~/.local/share/omarchygram/omarchygram.session` (sqlite), chmod 600. Media cache: `~/.cache/omarchygram/media/` (stickers auto-converted webp→png because GdkPixbuf here has no webp loader; animated .tgs stickers are reported unavailable).
- Telegram API creds come from the user's my.telegram.org account; never commit them.

# Design spec (orchestrator owns; workers implement verbatim)
- Reference tone: terminal-adjacent, flat, dense — closer to a TUI than to Telegram Desktop. When unsure, plainer wins.
- One typeface: "JetBrainsMono Nerd Font" everywhere. 8px spacing grid. No shadows, no rounded cards (4px max radius on bubbles), no gradients, no emoji in UI chrome.
- Colors ONLY from theme tokens via the omg-* CSS classes in `src/theme/style.css` (bg/bg-dark/bg-darker/bg-lighter/fg/muted/accent/selection/red). Never hardcode a color in UI code.
- Layout: left sidebar chat list (280px fixed), message pane, composer at bottom. One primary action per screen. Keyboard-first: Ctrl+K switcher, Alt+Up/Down chat nav, Enter send / Shift+Enter newline, Esc cancel/focus-composer.

# Known accepted limitations (decided 2026-09-01)
- Backend command/event channels are unbounded and data commands spawn freely — accepted at personal-client scale; revisit only if memory growth is ever observed.
- Animated (.tgs) stickers render as "image unavailable". Voice messages open in the default audio app, no in-app playback.
- A non-"wrong password" error during 2FA (e.g. network drop) requires an app restart — the server-side password token is consumed and the error message says so.
- Missing HOME/XDG dirs panic the backend with a clear message rather than degrade — never writable-relative-path session files.
- Downloaded document extensions are normalized to plain ascii, not whitelisted — opening is always an explicit user click; a whitelist would block legitimate files from contacts.

# Delegation notes
- Areas external agents must NOT touch: `src/tg/` (Telegram backend/auth/session — orchestrator only), `src/theme/mod.rs`, `src/main.rs`, `Cargo.toml`, any file containing credentials. `src/theme/style.css`: UI workers may ADD rules using existing var(--) tokens only.
- Commit tags: delegated commits end with [codex] / [grok] / [kimi] / [agy]
