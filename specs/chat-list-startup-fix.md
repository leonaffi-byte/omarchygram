# Chat-list startup follow-up

The user reported a blank chat list after the review changes. The existing Telegram session remained authorized. Read-only checks returned the account's dialogs, and native UI checks confirmed that the sidebar could remain blank while startup work was pending.

## Corrections

- Chat loading no longer waits for the optional stories request. Stories load independently and update avatar rings directly, regardless of which response arrives first.
- Story notifications no longer invalidate an in-flight dialog snapshot. Previously, an unrelated story update could discard a successful chat response and restart the entire fetch. Other snapshot protections remain intact.
- Removed the second archive fetch: the unfiltered dialog iterator already returns archived chats. The first live check returned 95 entries, including duplicates; corrected checks returned 87 unique dialogs. Both the backend and chat list reject duplicate IDs defensively.
- Initial chat loading displays a spinner and “Loading your chats…”. A failed or timed-out request displays an error with Retry. Dialog and folder requests have a 30-second timeout, including time spent waiting for the shared dialog load.
- Added explicit backend shutdown without signing out. Normal application exit and diagnostic exit wait for runtime teardown. The call-service shutdown now waits for the actor, native call object and runtime to finish, beyond its earlier acknowledgement.
- Rebuilt the release executable referenced by the installed desktop entry: `target/release/omarchygram`.

## Evidence and limits

The corrected debug backend returned **87 unique dialogs: 80 normal and 7 archived, plus 1 folder**, in 2.13 seconds. Two sequential debug native UI runs each displayed 80 real chat rows and exited normally. Counts reflect the live account at test time and can change.

All three sequential optimized native UI runs also displayed 80 real chat rows and exited 0, with fatal GTK criticals enabled. A subsequent backend-only release check hit the new 30-second dialog timeout and exited 1 normally; its cause was not established. A later retry returned all 87 unique dialogs and 1 folder in 1.82 seconds and exited 0. The timeout is retained as a failed attempt in the evidence, not counted as a successful startup. No shutdown crashes recurred in these corrected debug/release checks.

An earlier pair of real-account probes was accidentally run concurrently against the same session. One UI run stalled, while the backend probe fetched its data successfully and then crashed during process exit. These concurrent runs are excluded from acceptance. Subsequent real-session checks run sequentially.

The crash dump identified the `omg-calls` thread as the crashing thread while the main thread was executing process-exit/destructor code. Memory was available and there was no OOM kill. Application frames in the stripped binary remained unresolved; a native teardown race is the inference supported by the thread stacks and detached-thread lifecycle, not a symbolized function-level diagnosis. Explicit shutdown removes that lifecycle race. The temporary extracted core was deleted after inspection. The original system-managed dump was left unchanged.

These checks use the existing session and only read startup data. They do not send messages, place calls, sign out, delete sessions, or import private history. GUI checks use `bin/headless`; native live UI checks use temporary copies of window state and an isolated status file. A temporary live screenshot used to verify the displayed rows was deleted. Loading/error captures use mock data: `target/chat-list-loading.png` and `target/chat-list-error.png`.

The exact conditions on the user's desktop were not observed. The verified result is successful real-account backend and native UI startup after the corrections, rather than proof that every possible connectivity failure has the same cause. Live calling/audio hardware and server-side write operations remain outside this validation.

## Acceptance

- `cargo test --all-targets`: **85 passed** (60 library, 3 config, 18 mock backend, 4 theme).
- `cargo clippy --all-targets -- -D warnings`: **passed**.
- `cargo check --no-default-features --all-targets`: **passed**. Normal debug/release builds retain default call support.
- Added regression coverage for simultaneous/repeated shutdown through cloned handles, stopped request handling, and completed event-producer teardown.
- Native probe coverage checks loading feedback, story events preserving pending chat loads, duplicate snapshots, and the existing stories/read-state/navigation behavior.
- `cargo build --bin omarchygram --example backend_probe --example startup_probe` and the corresponding `--release` build: **passed**.
- Native live UI: **2/2 debug and 3/3 release runs passed**, each displaying 80 real chat rows and exiting 0.
- Narrow live backend: debug passed; release timed out once, then a later retry passed. Both timeout and success shut down normally.
- `git diff --check`: **passed**. No commits or remote writes.
- Final full `bin/gate`: **PASS**, all twelve runs exited 0. The validated debug and release artifact hashes stayed unchanged throughout acceptance (`target/chat-list-final-binary-hashes.txt`).

```text
ok   probe-1 (231 probe lines)
ok   probe-2 (231 probe lines)
ok   probe-3 (231 probe lines)
ok   probe-4 (231 probe lines)
ok   probe-5 (231 probe lines)
ok   probe-6 (231 probe lines)
ok   auth-1 (234 probe lines)
ok   auth-2 (234 probe lines)
ok   auth-3 (234 probe lines)
ok   latency (231 probe lines)
ok   seed-info (233 probe lines)
ok   seed-sidebar (232 probe lines)
gate: PASS
```

Logs use the `target/chat-list-final-` prefix. The full gate is run as `GATE_LOGS=target/chat-list-final-gate-logs bin/gate`. The sequential native startup wrapper is `python3 target/chat-list-startup-check.py target/release/examples/startup_probe 3`; it wraps each run in `bin/headless`. The narrow backend command is `bin/headless timeout 75 target/release/examples/backend_probe --dialogs-only`. Do not run real-session diagnostics alongside the normal app or each other.

## Files changed in this follow-up

- `examples/backend_probe.rs` — Counts-only startup check and orderly teardown.
- `examples/startup_probe.rs` — Read-only native chat-row verification and orderly teardown.
- `src/main.rs` — Wait for backend shutdown after the GTK application exits.
- `src/tg/mod.rs` — Explicit shared backend shutdown and corrected dialog API documentation.
- `src/tg/real.rs` — Independent chat loading, unique complete dialogs, bounded startup requests and shutdown handling.
- `src/tg/mock.rs` — Matching shutdown handling.
- `src/tg/calls.rs` — Wait for full native call-thread teardown.
- `src/ui/chatlist.rs` — Defensive deduplication and direct story-ring updates.
- `src/ui/shell.rs` — Loading/retry feedback, independent story updates, count-only startup trace and regression probes.
- `tests/mock_backend.rs` — Backend shutdown regression.
- `specs/chat-list-startup-fix.md` — Follow-up findings and evidence.
- `specs/review-fixes.md` — Link to the follow-up, preserving the earlier review's historical results.
