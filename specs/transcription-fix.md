# Groq and automatic transcription — 0.1.2

## Reproduction

Automatic transcription was dispatched from `post_render`, including history
loads and edits. Enabling the setting also explicitly scanned every voice
message in the loaded chat. The two-provider-slot limit did not prevent that
scan from launching many downloads and queued tasks.

Telegram voice documents often have no filename, so their cache files use
`.bin`. The cloud uploader copied that extension into `audio.bin` and sent
`application/octet-stream`. A one-second synthetic Ogg clip, stored as `.bin`,
reproduced Groq's HTTP 400 unsupported-file-type response using the actual
configured provider. No Telegram recordings or messages were uploaded during
this diagnostic.

## Behavior

- Automatic transcription is triggered only by a fresh incoming voice-message
  event. Its timestamp must be at or after enabling auto transcription in the
  current signed-in session. Starting the app does not process older backlog.
- History, cached pages, pagination, edits, and settings changes never dispatch
  transcription. Eligibility excludes outgoing messages and messages already
  marked deleted.
- Newly arriving voices in other chats can transcribe while the app runs in the
  background. One automatic job runs at a time, with at most 64 waiting IDs;
  a larger burst leaves excess messages available for manual transcription.
- Manual requests bypass the automatic queue and can use the second provider
  slot. Duplicate events and repeated clicks do not start duplicate requests.
- Turning auto off cancels automatic jobs and their queue, preserving manual
  work. Disabling AI or logging out cancels all transcription work. Aborted jobs
  cannot apply late results, and transcripts are cleared after successful logout.
- Requests own their download independently of rendered rows. Chat switching
  cannot restart a transcription; forum topics share the parent chat's key.
- The uploader reads at most 64 header bytes to identify the format and supplies
  a supported anonymous filename/MIME type. The original audio is streamed
  unchanged. Old `.bin` caches work without redownloading or re-encoding.

The Groq endpoint and model remain unchanged. Supported formats and endpoint
parameters were checked against [Groq's speech-to-text documentation](https://console.groq.com/docs/speech-to-text).

## Validation

- Live Groq rejected the synthetic `.bin` fixture before the fix and accepted
  the identical bytes after it. This verifies upload compatibility, not speech
  recognition accuracy: the synthetic fixture contains a tone.
- Unit tests cover audio header detection, a real multipart request to a local
  HTTP server (supported filename, MIME, unchanged bytes, no cache identifiers),
  and incoming-message eligibility/forum-topic keying.
- Native checks exercise enabling/reopening history without backfill, old
  replay and edit exclusion, a new voice in a closed chat, event deduplication,
  concurrent manual work, queue bounds, and cancellation without late results.
- Both quick native runs passed. An initial strict Clippy check requested a
  named type for the job state; the final code uses `TranscriptionJob`.

- `cargo test --offline --lib --tests`: **103 passed** (76 library, 3 config,
  20 mock backend, 4 theme).
- `cargo clippy --offline --all-targets -- -D warnings`: **passed**.
- `cargo check --offline --no-default-features --all-targets`: **passed**.
- Default debug and optimized release builds: **passed**, including calling support.
- Optimized executable: **56,534,720 bytes**, versus 56,501,440 previously:
  **+33,280 bytes (+0.0589%)**. No new dependencies or audio conversion step.

Full `bin/gate`: **passed**, all 12 runs, with no GTK warnings or critical errors.

```text
ok   probe-1 (250 probe lines)
ok   probe-2 (250 probe lines)
ok   probe-3 (250 probe lines)
ok   probe-4 (250 probe lines)
ok   probe-5 (250 probe lines)
ok   probe-6 (250 probe lines)
ok   auth-1 (253 probe lines)
ok   auth-2 (253 probe lines)
ok   auth-3 (253 probe lines)
ok   latency (250 probe lines)
ok   seed-info (252 probe lines)
ok   seed-sidebar (251 probe lines)
gate: PASS
```

Local acceptance logs and Groq before/after diagnostics are in
`target/transcription-fix/`. All UI checks ran under `bin/headless`; no user
window was opened or real Telegram messages sent. Nothing remains outstanding
for this fix. Speech recognition accuracy was not assessed with private audio.

## Files

- `Cargo.toml` — identify the fixed build as 0.1.2 in About.
- `Cargo.lock` — lock the application version.
- `src/ai/mod.rs` — detect the audio format, normalize upload metadata, redact keys in provider errors, and test the multipart request.
- `src/ui/shell.rs` — dispatch from fresh events, bound/cancel automatic work, preserve manual requests, and test the native flows.
- `src/settings.rs` — correct the auto-transcription contract.
- `src/ui/settings_view.rs` — explain new incoming messages, background behavior, and cancellation.
- `examples/transcription_probe.rs` — explicit-fixture Groq diagnostic, without a Telegram session or transcript logging.
- `README.md` — document auto-transcription behavior and legacy cache compatibility.
- `specs/transcription-fix.md` — reproduction, implementation, and acceptance report.
