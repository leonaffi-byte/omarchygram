# Omarchygram feature roadmap

Beyond-vanilla-Telegram features the user wants (decided 2026-09-01). Ordered
by build wave. Status (2026-09-02): waves 0, 1, 2 and 3 are MERGED (settings, power tweaks,
ghost mode, anti-delete, edit history, Assistant/Omarchy virtual chats,
transcription, AI draft/translate/summarize, ticketed shell gate). Wave 4
(animations: all 54 Motion Lab effects as toggles + presets + previews) is
MERGED. Wave 5 (Telegram parity: layout/chrome, in-app configuration, daily-use and regular-use functions — specs/spec-wave5.md) is IN PROGRESS: backend contract + real implementation merged 2026-09-02; all four UI packages (5A layout/chrome, 5B in-app configuration, 5C daily-use functions, 5D regular-use functions) MERGED 2026-09-02. Wave 5 is complete. Wave 6 (Telegram media parity: playback,
location, polls, remaining media kinds, scheduling, bots, topics, stories, calls) is
PLANNED (decided 2026-09-03) — see the Wave 6 section at the end; secret chats are
deliberately left out.

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

## Wave 5 backend notes (2026-09-02)
- Archived dialogs come from a raw `messages.getDialogs { folder_id: 1 }` call (grammers' iterator has no folder option); their peers are built from the response's access hashes, so history/media in archived chats work.
- Drafts are saved without their reply target (raw `InputReplyTo` shape varies by layer).

## Wave 6 — Telegram media parity (PLANNED 2026-09-03, orchestrator-owned order)

Everything Telegram can do that the client still shows as "[unsupported]",
opens externally, or lacks entirely. Ordered by build package; each package
is one delegation wave and merges before the next starts. Secret chats are
deliberately OUT (grammers has no MTProto 2.0 secret-chat layer; they are
device-bound anyway — revisit only if the user asks again).

Current state (2026-09-03): voice notes and video circles open in the default
external player; location, venue, contact card, dice and polls render as
"[unsupported]"; animated .tgs stickers render as "image unavailable";
scheduled messages, forum topics, stories, bot inline keyboards and calls do
not exist.

### 6A — In-app playback (medium; no new crate: `gtk::MediaFile`/`gtk::Video`)
- Voice messages: inline play/pause bar with progress and duration, seek by
  click, speed toggle (1x/1.5x/2x). Transcription stays where it is.
- Audio (music) messages: same bar plus title/performer.
- Video: inline player in the bubble (poster frame + play), fullscreen on
  double-click.
- Video circles (received): inline round playback (circular clip), autoplay
  muted on hover/click like Telegram Desktop.
- Only one player plays at a time; players stop when the row leaves the
  viewport or the chat changes.

### 6B — Remaining media kinds, display only (medium)
- Location: static map tile (OpenStreetMap tile fetched through the existing
  reqwest client, cached in the media cache) + coordinates + "Open in browser".
- Venue: same tile plus title/address line.
- Contact card: name, phone, "Add to contacts"/"Open chat" when the user id
  is known.
- Dice / darts / slots: final value with the emoji, no animation.
- Polls: question, options with vote counts and percentages, closed/anonymous
  /multiple-choice/quiz markers; voting (`messages.sendVote`) and retract;
  live update on `updateMessagePoll`.
- Backend: `MediaKind` grows `Location`, `Venue`, `Contact`, `Dice`, `Poll`
  with typed payloads; archive stores them; mock fixtures cover each.

### 6C — Sending (medium)
- Create poll (question, 2–10 options, anonymous, multiple choice, quiz with
  correct answer) from the attach menu.
- Send location: coordinates entry or a "pick on map" tile grid (no GPS on
  desktop); send venue is out.
- Scheduled messages: "Send later" in the composer (date/time picker), a
  "Scheduled" strip in the chat header that lists and lets you send-now or
  delete (`messages.getScheduledHistory`, `sendScheduledMessages`,
  `deleteScheduledMessages`).

### 6D — Animated stickers (medium–large; new dependency)
- Render .tgs (gzipped Lottie JSON) stickers: decision between `rlottie`
  bindings and a pure-Rust Lottie renderer is the orchestrator's; render to
  frames at the sticker's fps, pause when off-screen, respect the animations
  master toggle. Replaces the "image unavailable" placeholder.
- Sticker picker previews animate on hover only.

### 6E — Bots and forums (medium)
- Bot inline keyboards under messages (callback buttons, URL buttons, switch
  -inline); `/command` autocomplete from the bot's command list; the bot
  "Start" button on empty bot chats.
- Forum topics: topic list for forum supergroups, open a topic as a chat,
  topic name in the header, create topic.

### 6F — Capture and live features (large)
- Record and send video circles (webcam via PipeWire/GStreamer, round preview
  while recording).
- Live location: send with a duration, periodic updates while the app runs,
  stop sharing; render other people's live locations with last-update time.
- Stories: ring on avatars, view stories in a viewer (photo/video, auto
  advance), mark seen; posting is out.

### 6G — Calls (spike DONE 2026-09-03; verdict: build later via ntgcalls, not in wave 6)
- Spike result (orchestrator, research agent, sources checked 2026-09-03):
  - No pure-Rust path exists and none is realistic: Telegram's call media
    protocol (tgcalls v2) needs a custom reflector relay (not TURN), an
    MTProto-style encrypted signaling channel and per-version negotiation
    on top of a patched libwebrtc; `webrtc-rs`/`str0m` cannot speak it.
  - Building `tgcalls` + `tg_owt` from source has no standalone build and
    a C++-only API (1–2 weeks, fragile).
  - The ONE workable path is **ntgcalls** (github.com/pytgcalls/ntgcalls,
    LGPL-3, C ABI + an official `ntgcalls` Rust crate 3.0.0-rc01): prebuilt
    Linux x86_64 static libs, `create_p2p_call`/`exchange_keys`/
    `connect_p2p`, signaling stays in `src/tg/` on grammers (`phone.*` TL
    functions are all in grammers-tl-types 0.10). Estimate: 3–5 days for
    voice, +2–3 for video. Risks: release-candidate crate whose build.rs
    downloads a 32 MB lib from GitHub (must be vendored), LGPL relinking
    obligations, PipeWire capture/playback unverified, interop with the
    current official clients only reported for ntgcalls 2.x.
- Decision: NOT built in wave 6. Scheduled as its own wave 7 after the user
  confirms the LGPL/vendoring trade-off; it starts with a 1-day interop
  spike (one outgoing call to a real phone with two-way audio) before any
  UI work.

