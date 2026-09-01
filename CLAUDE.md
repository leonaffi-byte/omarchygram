# Project
- Type: small  <!-- recorded at kickoff, never ask again -->
- Goal: Omarchygram — a from-scratch Telegram client for Omarchy that auto-themes from the active Omarchy theme.

# Commands (orchestrator runs these directly — never delegate)
- Setup: `python -m venv --system-site-packages .venv && .venv/bin/pip install -e .[dev]` (system-site-packages is required: PyGObject/GTK4 come from pacman, not pip)
- Run: `.venv/bin/python -m omarchygram`
- Test: `.venv/bin/python -m pytest`
- Smoke (no Telegram creds needed): `.venv/bin/python -m omarchygram --smoke` (opens themed window, no login)

# Architecture (decided 2026-09-01, orchestrator)
- Python 3.14 + GTK4 via PyGObject (system packages) + Telethon (venv) for Telegram MTProto.
- Event loop: PyGObject `gi.events.GLibEventLoopPolicy` — Telethon's asyncio runs ON the GLib main loop. Never spawn a second loop/thread for Telethon.
- Theme source of truth: `~/.local/state/omarchy/current/theme/colors.toml` (semantic keys: mode, accent, background/dark_background/darker_background/lighter_background, foreground/light_foreground, muted, selection, red…brown). App generates GTK CSS from it at startup and re-applies via Gio.FileMonitor on `~/.local/state/omarchy/current/theme.name`.
- Config: `~/.config/omarchygram/config.toml` (api_id, api_hash), chmod 600. Session file: `~/.local/share/omarchygram/omarchygram.session`, chmod 600.
- Telegram API creds come from the user's my.telegram.org account; never commit them.

# Design spec (orchestrator owns; workers implement verbatim)
- Reference tone: terminal-adjacent, flat, dense — closer to a TUI than to Telegram Desktop. When unsure, plainer wins.
- One typeface: "JetBrainsMono Nerd Font" everywhere. 8px spacing grid. No shadows, no rounded cards (4px max radius on bubbles), no gradients, no emoji in UI chrome.
- Colors ONLY from theme tokens: main bg=background, sidebar bg=dark_background, hover/selected=lighter_background, text=foreground, secondary text=muted, accent=accent (own-message marker, unread badge, focused borders), selection=selection. 1px borders in lighter_background.
- Layout: left sidebar chat list (280px fixed), message pane, single-line composer at bottom. One primary action per screen.

# Delegation notes
- Areas external agents must NOT touch: `omarchygram/tg/` (Telegram client/auth/session — orchestrator only), any file containing credentials, `.venv/`.
- Commit tags: delegated commits end with [codex] / [grok] / [kimi] / [agy]
