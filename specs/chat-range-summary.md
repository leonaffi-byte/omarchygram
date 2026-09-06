# Assistant Start and chat range summaries — 0.1.6

The Assistant's empty history was treated as a bot chat, which displayed Start.
The Start handler rejected local virtual chats, so pressing the button returned
without doing anything. Start now opens a local introduction and enables the
composer; `/start` also works as a help command. The stale “No messages yet”
overlay now disappears when the first message arrives in an empty conversation.

## Using summaries

Enable AI in Settings. In a real chat, right-click the first message and choose
**Summarize from here**, then right-click the last and choose **Summarize to
here**. The chat header's menu also explains this flow. Endpoints can be selected
in either order. Both are included, along with every message between them.

The result appears in a private panel above the composer with Copy and Close.
The result area scrolls within 180 logical pixels rather than displacing the
whole conversation. The panel shows history, transcription and summarization
progress, with Cancel during work and Retry on failure. No summary is posted to
Telegram. Content is processed by the user's configured AI providers.

## Processing and limits

- History is paged backwards from the last endpoint to the first. An unloaded
  gap in the displayed history does not silently exclude messages. Topic ranges
  filter by forum and topic. Missing endpoints cause an error.
- Voice notes inside the range use cached transcripts or one explicit
  transcription at a time. Existing manual/automatic jobs are reused. A failed
  or empty transcript stops the summary and provides a retryable error.
- Cancelling stops the summary's owned transcription and drops pending AI
  requests. A transcription already started independently remains independent.
  Switching chats, confirming history clearing/chat deletion, disabling AI and
  logout also clear the summary workflow.
- Automatic transcription still applies only to new incoming voice messages;
  selecting a range does not enqueue unrelated history.
- Ranges are limited to 2,000 messages and one million characters. Oversize
  input produces an explicit error, not a truncated summary. Deleted content
  is represented as unavailable, and non-voice media is not falsely described
  as transcribed.
- Long text is prepared on an existing background worker, split into bounded
  prompts, and summarized in successive parts before final synthesis. Network
  and transcription calls use the existing asynchronous services. No new
  dependencies or permanent worker threads were added.

The release executable is `target/release/omarchygram`, which is already the
desktop launcher's target. Fully quit the running app and reopen it to load the
new executable; closing its window can leave the previous process in the
background. About identifies the build as **0.1.6**.

## Files in this change

- `Cargo.toml`: version 0.1.6.
- `Cargo.lock`: application version, with no dependency changes.
- `README.md`: Start, range-selection flow, cancellation and processing limits.
- `src/ai/mod.rs`: summary fixture replies and rejection of empty speech responses.
- `src/ai/prompts.rs`: fixed range-summary and partial-summary instructions.
- `src/ai/summary.rs`: inclusive range collection, topic filtering, transcript formatting, chunking and unit tests.
- `src/local/mod.rs`: cancel chat provider futures when their caller disappears.
- `src/tg/mock.rs`: honor arbitrary exclusive history offsets for range queries.
- `src/theme/style.css`: panel styling using existing theme tokens.
- `src/ui/mod.rs`: register the summary panel module.
- `src/ui/summary.rs`: compact progress/result/error panel and native controls.
- `src/ui/messages.rs`: message/header menu actions, panel wiring, empty-history label fix and UI probe helpers.
- `src/ui/shell.rs`: local Start handling, cancellation lifecycle and probe integration.
- `src/ui/shell/summary.rs`: asynchronous range workflow, transcript reuse, synthesis, and native regression scenarios.
- `specs/chat-range-summary.md`: implementation and validation record.

The preceding performance changes are included alongside this feature; see
`specs/100-fps-performance.md` for their file list and measurements.

## Validation

- Final debug and release builds passed.
- 112 tests passed: 85 library, 3 config, 20 mock backend, 4 theme.
- Clippy passed for all targets with warnings denied.
- All targets compiled with default features disabled.
- The quick native gate passed its normal and authentication runs.
- The exact optimized launcher executable passed a separate complete native
  probe (264 steps, exit 0 with GTK criticals fatal); About displayed 0.1.6.
- Final screenshot inspection verified the Assistant introduction without the
  empty-history overlay and the summary panel's readable text and controls.
- The full final gate passed all 12 cases with no GTK/GStreamer warnings or
  criticals in its logs. The debug executable was unchanged throughout that
  gate; its source includes the history-clearing/chat-deletion cancellation guard.

```text
ok   probe-1 (264 probe lines)
ok   probe-2 (264 probe lines)
ok   probe-3 (264 probe lines)
ok   probe-4 (264 probe lines)
ok   probe-5 (264 probe lines)
ok   probe-6 (264 probe lines)
ok   auth-1 (267 probe lines)
ok   auth-2 (267 probe lines)
ok   auth-3 (267 probe lines)
ok   latency (264 probe lines)
ok   seed-info (266 probe lines)
ok   seed-sidebar (265 probe lines)
gate: PASS
```

Native tests use offline mock Telegram and AI providers, inside `bin/headless`.
They cover the actual Start and message-menu buttons, reverse endpoints, voice
inclusion, exclusion outside the range, unloaded history gaps, missing-provider
feedback, Retry, cached transcript reuse, active cancellation, chat switching,
and disabling AI. Existing new-only auto-transcription checks still pass.
Live account content was not sent to an AI provider during validation; real
provider output quality and response latency were not measured.

The separate optimized probe passed but emitted four nonfatal GStreamer
warnings about media data arriving before stream-start/segment events. Those
occurred in the media checks before the Assistant steps; no playback assertion
or AI assertion failed. They remain recorded in the release probe log.

The optimized executable is 56,762,080 bytes, 101,312 bytes (about 99 KiB) larger
than the preceding 0.1.5 performance build. SHA-256:
`06e417288adeec0d54baf9d30970ec33c703ef21f434645ca8ae6cb178a1ac48`.

Logs: `target/ai-summary-{build-final,release,tests-final,clippy-final,no-calls-final}.log`.
Full native logs: `target/ai-summary-gate-final/` and `target/ai-summary-gate-final.log`.
Optimized native probe: `target/ai-summary-release-probe.log`.
Screenshots: `target/ai-summary-final-shots/`.
