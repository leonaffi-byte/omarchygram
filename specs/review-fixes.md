# Review fixes

Follow-up: the subsequently reported blank chat list is addressed in [Chat-list startup fix](chat-list-startup-fix.md), including real-account startup verification. The measurements and acceptance results below describe the original review implementation. Later background, presence, caching and header/pin work is recorded in [Background operation and chat UX](background-and-chat-ux.md).

Original review scope (before the later commit/push authorization): implement the correctness, efficiency, and UI/UX findings from both September 5 reviews while retaining capabilities. No commits, publishing, desktop GUI runs, or real-message/call tests. All GUI verification uses `bin/headless` and the final full `bin/gate`.

## Worklist

- [x] Calls: incoming setup/key exchange, responsive actor operations, phase-specific timeouts, stale callbacks, mute/device error handling, and shutdown.
- [x] Account lifecycle: recreate login client after logout; cancel old commands/update streams/call actors; isolate archive and media by account without deleting legacy data.
- [x] Downloads: atomic completed files, per-resource coalescing, failures/retries, nonblocking decode/stitch, avatar cache.
- [x] Forum safety: topic-scoped delete/clear/read/search/date/drafts/pins/mute actions; prevent parent-wide fallthrough.
- [x] Completeness: paginate dialogs/topics/shared media; deterministic folder metadata; preserve search/history navigation.
- [x] Persistence: preserve malformed config/settings; atomic private writes and non-destructive migration.
- [x] Archive: batch writes in transactions, bound redundant work, retain edit/delete history and correct account scope.
- [x] Resource lifetime: bounded reusable media/story players, remove widget ownership cycles, bounded transcription/process cleanup, scalable message/chat rendering.
- [x] Build: remove unused dependency features, clean lint failures without disabling checks; measure release size and capability parity.
- [x] Theme: accessible contrast, surface separation, consistent selected/focus/checkbox/disabled/danger states.
- [x] Conversation: compact internal message layout, keyboard-accessible reactions, uniform composer controls, accurate compact/comfortable density, useful empty/loading/error states.
- [x] Responsive layout: reachable sidebar transition, conversation-aware info panel, narrow settings, text scaling.
- [x] Search/navigation: match emphasis, scope/counts, useful empty groups, switcher identity/hints, forwarding preview/selection count, topic metadata.
- [x] Forms: sign-in progress/labels/help, poll labels/requirements, deliberate labeled location selection, static/live clarity, place/current-location support where available.
- [x] Settings: searchable categories, Messaging grouping, readable animation descriptions/previews, time presets, provider/privacy explanations, shorter form layout.
- [x] Media/calls/stories UX: explicit external-open action, retry/loading feedback, accessible navigation/pause, clear call/device state.
- [x] Verification: regression tests for correctness, native widget/keyboard/popover captures, multiple themes/scales/widths, tests/build/lint, full headless gate, final measurements/report.

## Implemented

- **Calls and sign-in:** phase-specific deadlines and stale-event rejection; tracked signaling work keeps the actor responsive; native calls remain serialized; verified mute/device errors; in-call device selection preserves mute and rolls back failed changes. Transport errors during password verification refresh the SRP challenge on explicit retry.
- **Account isolation:** logout stops old updates, data tasks and call actors, recreates the login client, and clears UI/media state. Archives and downloads live under account IDs. A confirmed, transactional legacy import preserves the old archive and all known edits.
- **Persistence:** private atomic files, unique temporary downloads and cross-process locks; custom-file destinations preserve existing parent-directory permissions; malformed files and unknown settings survive writes; API IDs reject overflow; smoke runs preserve supplied window-state seeds inside isolation.
- **Forums:** complete topic/dialog pagination and metadata; topic-scoped search, drafts, pin/mute, read state, deletion and clearing; General-topic filtering; unsafe parent-wide fallthroughs rejected. Mock contracts assert that other topics and the parent remain unchanged.
- **Resource use:** bounded reusable video wrappers with explicitly owned GStreamer pipelines; bounded frame queues; offscreen/closed pipelines release their threads; fullscreen playback stays active. Bounded avatar decoding/cache, coalesced downloads, tile caching/concurrency, batched archive transactions, bounded transcription and subprocess cancellation. Message regrouping and quote refresh avoid copying the loaded history.
- **Conversation:** readable metadata, compact message internals, visible photo-only messages, keyboard-accessible reactions, consistent composer controls, correct send-shortcut hints and a useful home action. GTK scroll callbacks are deferred until allocation finishes so visible content matches the scrollbar.
- **Navigation and forms:** narrower sidebar/story rail, conversation-aware info overlay, highlighted search matches and counts, switcher identities and keyboard hints, forwarding previews/counts, labeled sign-in/poll/location forms, confirmed pins, explicit place search and optional GeoClue system location.
- **Settings and media:** searchable categories, Messaging grouping, time presets/custom previews, text sizing, density previews, understandable effect descriptions, provider/privacy explanations, explicit external opening, story navigation/pause/loading/retry and clear call/device states. Expanded call settings scroll on short windows.
- **Theme:** derived text contrast and semantic foregrounds, explicit native GTK light/dark surfaces, consistent selection/focus/check/disabled states. Existing flat monospace design retained.
- **Build:** duplicate JPEG decoder feature removed; all existing Clippy failures corrected without suppressing rules. Calls remain enabled by default. Size-oriented compiler experiments and safe identical-code folding were rejected because they did not reduce the binary; release optimization remains level 3.

## Verification

- `cargo test --all-targets`: 84 tests passed (60 library, 3 config, 17 mock backend, 4 theme).
- `cargo clippy --all-targets -- -D warnings`: passed with no warnings, including the final persistence changes.
- `cargo check --no-default-features --all-targets`: passed. The normal build retains calls by default.
- Native captures: `target/review-visuals-2026-09-05/`; normal/narrow settings, text scaling, light/dark themes, forms, forwarding, media, stories and calls. Final examples: `38-conversation-complete.png`, `31-light-conversation.png`, `32-light-poll.png`, `40-call-150-controls.png`, and `41`–`43` sign-in screens. Seventeen accepted final PNGs also pass checksum/decompression validation. Earlier captures that exposed defects are retained as intermediate evidence, not final acceptance images.
- Full `bin/gate`: **PASS**, all twelve runs exited 0 on the final debug build (`target/review-acceptance-gate-summary.txt`, `target/review-acceptance-gate-logs/`). Command: `GATE_LOGS=target/review-acceptance-gate-logs bin/gate`. The preceding full run passed eleven traversals but failed the restored-info assertion: it compared a 360 px requested outer width with GTK’s 358 px content width, excluding two borders. The panel was bound and visible. The assertion now measures outer bounds and additionally verifies overlay attachment and right alignment; the one-pixel tolerance is unchanged. The targeted seeded rerun then exited 0 with 231 probe lines (`target/review-restore-verified.log`). The failed full run is retained in `target/review-complete-gate-summary.txt` and is not counted as acceptance. [GTK content-width documentation](https://docs.gtk.org/gtk4/method.Widget.get_width.html), [GTK bounds documentation](https://docs.gtk.org/gtk4/method.Widget.compute_bounds.html).
- `cargo build --bin omarchygram` and `cargo build --release`: passed. The final release seeded traversal exited 0 with 231 probe lines and `G_DEBUG=fatal-criticals` (`target/review-release-probe.log`). Command: `bin/headless env G_DEBUG=fatal-criticals OMG_PROBE_TRACE=1 OMG_UISTATE_PATH=<seed-file> timeout 170 target/release/omarchygram --smoke --probe`; the seed contains `info_panel_open = true` and `info_width = 360`.
- `git diff --check`: passed. No tests or acceptance checks were disabled, and no changes were committed.

Selected final screenshots: [conversation](../target/review-visuals-2026-09-05/38-conversation-complete.png), [light theme](../target/review-visuals-2026-09-05/31-light-conversation.png), [labeled poll](../target/review-visuals-2026-09-05/32-light-poll.png), [large-text call controls](../target/review-visuals-2026-09-05/40-call-150-controls.png), [password sign-in](../target/review-visuals-2026-09-05/43-auth-password.png).

### Full gate summary

```text
ok   probe-1 (229 probe lines)
ok   probe-2 (229 probe lines)
ok   probe-3 (229 probe lines)
ok   probe-4 (229 probe lines)
ok   probe-5 (229 probe lines)
ok   probe-6 (229 probe lines)
ok   auth-1 (232 probe lines)
ok   auth-2 (232 probe lines)
ok   auth-3 (232 probe lines)
ok   latency (229 probe lines)
ok   seed-info (231 probe lines)
ok   seed-sidebar (230 probe lines)
gate: PASS
```

## Resource measurements

Three alternating before/after repetitions per scenario, using release builds with calls enabled and fresh isolated config/cache/data. The chat-switch case opens Media Lab and Deni alternately 24 times at 500 ms intervals and ends in Deni. After a 5-second idle or 17-second switching warmup, six process samples are taken over five seconds. Table values are medians across repetitions; RSS comes from `/proc`, threads from the same process, and CPU is a percentage of one core over the sampling interval.

| Scenario / measure | Before | After |
|---|---:|---:|
| Idle RSS | 100.36 MiB | 103.61 MiB |
| Idle threads | 23 | 23 |
| Idle CPU | 0.60% | 0.40% |
| RSS after repeated chat switches | 382.46 MiB | 111.11 MiB |
| Threads after repeated chat switches | 98 | 32 |
| CPU after repeated chat switches | 0.80% | 0.60% |

The repeat-switch workload uses **70.95% less RSS and 67.35% fewer threads**. Idle memory is 3.25 MiB higher; these short CPU samples are too noisy to establish a speed improvement. This measures retained resources after switching, not startup time, frame rate, network latency or maximum history size. Full-gate work was also running on the machine, so CPU figures should be treated cautiously.

Reproduce with `bin/headless python3 target/review-benchmark.py`. Raw results are in `target/review-benchmark-results.json`; the preserved baseline is `target/review-before-omarchygram`. The benchmark includes the final media/UI resource changes; the subsequent parent-directory permission correction, removal of its unused helper, and probe-only geometry assertion correction were validated separately. The stripped release binary is **55,997,344 bytes**, versus **55,618,880 bytes** before: **+378,464 bytes (+0.68%)**. This is a small size regression, not a size reduction; calls and the existing codecs remain available. Safe size-oriented compiler experiments were rejected because they increased size or produced no gain. Final artifact hashes and sizes are recorded in `target/review-binary-hashes.txt`.

## Evidence limits

All GUI runs use `bin/headless` and mock data. No real Telegram messages, calls, forwards, location sharing, or private-history imports were performed. Real server interoperability, audio hardware and system-location availability require live verification. Native widget and token checks are not a screen-reader or accessibility certification. Desktop popup placement cannot be verified without a seat in the private compositor; attachment-menu content was inspected in a temporary native container. Forced capture-only popup/key operations hit missing-seat assertions and are excluded from acceptance. The actual application probe uses its existing native action hooks.

Loaded message models/widgets still grow when deliberately paging further back. This change removes redundant work and bounds media/decoding resources; it does not claim complete history virtualization or a new rendering framework.

## Files changed

- `CLAUDE.md` — Updated account-storage and player-lifetime architecture notes.
- `Cargo.lock` — Resolved the adjusted dependency feature graph.
- `Cargo.toml` — Streaming uploads, explicit GStreamer adapter dependencies, removed duplicate JPEG decoder.
- `README.md` — Documented account archives, search/text settings, location options and call devices.
- `examples/backend_probe.rs` — Updated backend API contracts and lint issues.
- `specs/review-fixes.md` — Review report and verification evidence.
- `src/ai/mod.rs` — Streaming transcription uploads and bounded cancellable subprocess work.
- `src/config.rs` — Preserve malformed credential files and reject invalid/overflowed API IDs.
- `src/lib.rs` — Register the shared persistence module.
- `src/local/mod.rs` — Transcription concurrency and cancellation.
- `src/main.rs` — Isolate and preserve supplied smoke state seeds.
- `src/os/mod.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/settings.rs` — Locked settings updates and new text/place-provider preferences.
- `src/status.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/storage.rs` — Private atomic writes, strict TOML merge, locking and persistence regression tests.
- `src/tg/archive.rs` — Account separation, transaction batches, topic-aware history and safe legacy import.
- `src/tg/calls.rs` — Call actor setup, signaling, deadlines, stale work, devices, mute and shutdown.
- `src/tg/markdown.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/tg/mock.rs` — Matching topic/account/call/location contracts and deterministic fixtures.
- `src/tg/mod.rs` — Backend commands and public data contracts for archive import, places and call devices.
- `src/tg/places.rs` — Explicit bounded and rate-limited Photon-compatible search.
- `src/tg/real.rs` — Account lifecycle, complete pagination, topic scope, downloads/maps and password retry.
- `src/theme/mod.rs` — Accessible derived colors and point/pixel text scaling with tests.
- `src/theme/style.css` — Readable themes, native control states, tighter layout, density and responsive styling.
- `src/ui/anim/mod.rs` — Readable effect names/descriptions and callback/lint cleanup.
- `src/ui/anim/overlays.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/auth.rs` — Sign-in progress, persistent labels and guidance; weak callbacks.
- `src/ui/avatar.rs` — Bounded background decoding and texture cache.
- `src/ui/bots.rs` — Show/hide keyboard slots correctly when bot keyboards change.
- `src/ui/call.rs` — Clear connected/error/mute state, live device selection and scrollable large-text layout.
- `src/ui/cards.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/chatlist.rs` — Correct row density, responsive rail, search counts and match emphasis.
- `src/ui/forward.rs` — Message preview and recipient identity/count feedback.
- `src/ui/geolocation.rs` — Cancellable one-shot GeoClue acquisition and service cleanup.
- `src/ui/keys.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/keys_view.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/locationdialog.rs` — Labeled confirmed pins, place search, one-shot system location and map debounce.
- `src/ui/lottie.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/markup.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/messages.rs` — Compact rows, visible images, deferred scrolling, smaller update work and weak handlers.
- `src/ui/mod.rs` — Register new UI modules and shared callback aliases.
- `src/ui/player.rs` — Bounded stream wrappers, offscreen teardown/resume and fullscreen visibility.
- `src/ui/poll.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/polldialog.rs` — Persistent question/option labels and validation guidance.
- `src/ui/settings_view.rs` — Search, categories, text/density/time previews, privacy/provider copy and archive import.
- `src/ui/shell.rs` — Account/topic integration, responsive panels, search/forms and stronger regression probes.
- `src/ui/stories.rs` — Compact story rail, bounded playback, loading/retry and accessible navigation.
- `src/ui/switcher.rs` — Disambiguated identities, scrollable results, keyboard hints and weak parent capture.
- `src/ui/topics.rs` — Topic-specific metadata.
- `src/ui/video_stream.rs` — Owned GStreamer video pipeline, bounded zero-copy frame delivery and cleanup.
- `src/ui/videonote.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/ui/viewer.rs` — Explicit external-open/save labels and non-cyclic control ownership.
- `src/ui/virtual_chat.rs` — Callback ownership/type cleanup and existing Clippy fixes; related UI contracts updated.
- `src/uistate.rs` — Atomic merged window-state persistence.
- `tests/mock_backend.rs` — Regression contracts for topic isolation and call-device changes.
- `tests/theme.rs` — Verify the semantic foreground used on accent-colored controls.
