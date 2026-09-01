# Omarchygram feature roadmap

Beyond-vanilla-Telegram features the user wants (decided 2026-09-01). Ordered
by build wave. Status (2026-09-02): waves 0, 1, 2 and 3 are MERGED (settings, power tweaks,
ghost mode, anti-delete, edit history, Assistant/Omarchy virtual chats,
transcription, AI draft/translate/summarize, ticketed shell gate). Wave 4
(animations, `specs/spec-wave4-animations.md`) is in progress.

## Design decisions (orchestrator-owned)

- **OS control + AI live as a LOCAL virtual chat**, not a real Telegram
  conversation and not Saved Messages. Pseudo-chats ("Omarchy", "Assistant")
  appear in the sidebar and look like normal chats, but their input/output
  never touch Telegram servers — you type locally, it runs locally. This is
  the security model: there is NO remote-message surface, so no injection/RCE
  path exists. Commands run on the machine; AI queries go to the configured
  provider. Everything off by default; every OS action is audit-logged.
- **AI provider layer = one trait, many backends, auto-detected.**
  - Transcription: local `whisper.cpp`/`faster-whisper` if on PATH, else Groq
    API, else OpenAI API. All three supported; app picks best available, user
    can pin per-task.
  - Chat/LLM: local `ollama` if running, else Anthropic / OpenAI / Groq /
    Gemini by whichever API key is present in config.
  - Detection = CLIs on PATH + keys in `~/.config/omarchygram/config.toml`.
  - Keys stored in config (chmod 600), never in code/logs/commits.
- **Ghost mode / anti-delete are ToS-adjacent** — the user opted in knowingly.
  Both off by default, clearly labelled.

## Wave 1 — cheap power tweaks (fully independent)
- Seconds in message timestamps (option); live seconds clock in header.
- Copy message id / user id (context menu).
- Jump to date (scroll history to a chosen date).
- Configurable timestamp format.

## Wave 2 — ghost mode + anti-delete
- Ghost mode: suppress read receipts (don't call ReadHistory) and online
  status while browsing; a per-session toggle.
- Anti-delete: keep a local archive of messages; when a delete update arrives,
  keep the row with strikethrough + "deleted" marker instead of removing it.
- Edit history: store prior versions; show a history popover on edited msgs.
  (We already receive edit/delete events live — this is local storage + UI.)

## Wave 3 — AI, local virtual chat, OS bridge
- Provider abstraction + auto-detection + config.
- Voice transcription (local + Groq + OpenAI) shown inline under voice msgs.
- "Assistant" virtual chat: catch-up digests ("what did I miss in <chat>"),
  reply drafting into the composer, thread summaries, semantic search,
  translation.
- "Omarchy" virtual chat: named local actions (screenshot, theme next, lock,
  notify, volume, user-defined scripts); optional confirm-gated shell.
- OS actions also exposed as AI tools (natural language -> named action).

## Animations
- Standalone Motion Lab artifact (54 options across 10 groups) is the design
  sandbox. User reviews there, sends picks, then animations ship as individual
  settings toggles in an in-app Animations menu. Keep ALL as options.
