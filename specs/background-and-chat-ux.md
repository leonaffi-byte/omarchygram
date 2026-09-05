# Background operation and chat UX

Follow-up to the code/UI reviews and chat-list startup fix, requested on September 5, 2026. The user subsequently authorized implementation, commits and pushing without an orchestrator. All native GUI checks run through `bin/headless` with mock data. The running real-account application is left untouched.

## Behavior

- Closing hides the window by default. The same backend, message sync, notifications and call handling stay alive without a workspace window. The launcher restores the existing window and selected chat. Settings → Messaging contains **Keep running when the window closes**.
- **Quit Omarchygram**, **Ctrl+Q** and `--quit` exit explicitly and wait for backend teardown. `--background` launches hidden; an account needing login still shows authentication. Window geometry is saved on close and explicit quit. Automatic startup at login is not installed.
- Presence follows visible, focused use with a five-minute inactivity threshold. Hiding or losing focus requests offline immediately; idle is checked every five seconds. The backend serializes status requests and renews them each minute. Ghost mode always overrides to offline. Shutdown attempts offline with a bounded wait.
- Pins show at most four wrapped preview lines. Expand restores the original text and line breaks inside a scrollable panel capped at 240 pixels of content; Collapse returns to the preview. Pango's line limit is per paragraph, so only the preview normalizes whitespace to prevent multi-paragraph pins bypassing the cap.
- Sidebar search and the conversation header share a GTK height group. Entries are vertically centered. Chat search uses a smaller minimum entry width, shorter older-results controls in narrow conversations, and no misleading result count before a query is entered. Full search errors remain available as tooltips when their label is ellipsized.

## Chat opening

Initial history starts before optional chat info, pins and scheduled messages. Cached and fresh history load concurrently. A cache hit renders immediately; Telegram refreshes it in place. Missing cache data falls back to the normal history request. Fresh requests have a 30-second timeout and cache reads have a two-second fallback. Failed refreshes retain displayed messages with a Retry action.

The new full-message cache preserves formatting, metadata, reactions, polls, keyboards and media descriptions. It is separate from the edit/deletion archive, which cannot reconstruct all those fields. Files live in `~/.cache/omarchygram/accounts/<id>/history/`, with private file/directory permissions. There are at most 24 pages, 50 messages per page, and 512 KiB per serialized page: at most 12 MiB of page data, plus filesystem overhead. Oversized or malformed pages become cache misses. Recent reads update eviction order. All file and serialization work runs on an ordered blocking worker outside the GTK and async executor threads.

New messages, edits, sends, forwards, deletions and poll changes update existing pages. Clearing/deleting a chat invalidates the appropriate pages, including forum-topic scope. Per-dialog revision tokens prevent old responses from undoing live mutations or newer loads; activity in an unrelated dialog does not disable caching. Revision tracking is also bounded. The UI independently preserves intervening edits/deletions and pending sends, rejects responses for a previous chat selection, and maintains scroll position during refresh. Existing downloaded media can display before fresh Telegram references arrive; unresolved media is retried after the fresh response.

A first visit without a cache still depends on Telegram. This does not prefetch every chat or claim to fix every source of network delay. Cached pages persist across restarts once the account has started successfully. Telegram remains authoritative.

## Validation

- `cargo test --all-targets`: **92 passed** (66 library, 3 config, 19 mock-backend, 4 theme).
- `cargo clippy --all-targets -- -D warnings`: **passed**.
- `cargo check --no-default-features --all-targets`: **passed**. Normal builds retain calls.
- Debug and optimized builds, with calling support: **passed**. Final full `bin/gate`: **PASS**, all twelve runs exited 0; summary below.
- New cache regressions cover rich-message round trips, account separation, persistence across cache instances, permissions, topic separation, edits/deletions, stale responses, unrelated busy chats, newest-load precedence, bounded revision tracking, page/message bounds and corrupt/oversized data.
- The optimized native probe also **passed** (235 steps, fatal GTK critical checks).
- Native regressions cover hiding/reopening the same session, offline intent while hidden, cached rendering before a delayed response, cache/fresh responses preserving live mutations, retaining content on failure, and multi-paragraph pin collapse/expansion bounds.
- `python3 tests/background_instance.py` and its `target/release/omarchygram` variant: **passed**, isolated D-Bus/Wayland CLI checks for hidden startup, single-instance reopen and remote quit. This test disables accessibility only inside its private bus; the full gate retains normal accessibility and fatal GTK critical checks.
- Rendered mock screenshots cover collapsed/expanded long pins, simultaneous sidebar and chat search, narrow windows, and 150% text. Intermediate failed captures are retained separately from final evidence under `target/background-chat-fixes/`.

The first long-pin regression failed because Pango allowed four lines per paragraph; the corrected preview passes. An initial private-bus experiment auto-started an accessibility service that conflicted with the user's accessibility socket. The normal accessibility service was restored and its bus/registry checked. The committed CLI test uses a bus without desktop-service activation; this failure is not counted as acceptance. An earlier full gate was stopped after further responsive-search changes, so only the final full gate below establishes acceptance.

## Size and timing

The optimized binary is **56,362,304 bytes**. The preceding chat-list startup
build was 56,014,048 bytes: **+348,256 bytes (+0.62%)**. This is a small binary
size increase for the added behavior, not a size reduction. Calls and existing
media capabilities remain enabled. The earlier review report retains its
separate before/after memory measurements; they were not repeated here.

Three optimized mock runs with a deliberate 1.5-second `GetHistory` delay:

| Run | First open | Cached reopen | Fresh response after reopen |
| --- | ---: | ---: | ---: |
| 1 | 1503 ms | 1 ms | 1501 ms |
| 2 | 1503 ms | 1 ms | 1502 ms |
| 3 | 1504 ms | 4 ms | 1501 ms |

These timings count insertion of message models into the native UI, not the
first compositor frame. Mock-cache timing does not measure the real disk-cache
deserializer or Telegram latency. The source of results is
`target/background-chat-fixes/cache-timing-results.json`.

Reproduce with `bin/headless env OMG_MOCK_SLOW=GetHistory OMG_HISTORY_TRACE=1
OMG_SMOKE_OPEN=Marta OMG_SMOKE_OPEN_SEQUENCE='Deni@3000,Marta@6000'
OMG_SMOKE_SHOT_DELAY_MS=8500 OMG_SMOKE_SHOT=/tmp/omg-timing.png
target/release/omarchygram --smoke` (one shell command).

A separate temporary harness compiles the actual `history_cache.rs` and
`storage.rs` modules against the built library. Ten fresh cache instances each
read and verify 50 synthetic rich messages from a private temporary directory:
**0.511 ms minimum, 0.608 ms median, 0.897 ms maximum**. These reads include file
access and deserialization, with warm filesystem data; they are not physical
cold-disk measurements. The harness and output remain in
`target/background-chat-fixes/disk-cache-bench.rs` and `disk-cache-timing.log`.

## Evidence limits

Presence decisions and lifecycle are verified locally with mock data; no live Telegram messages or calls were sent. Server-side presence visibility also depends on other logged-in clients, Telegram privacy/activity rules and connectivity. Telegram documents brief online visibility after sending, reading or typing even under restrictive last-seen settings; see [Telegram’s online-status FAQ](https://telegram.org/faq#q-who-can-see-me-39online-39). Explicit presence requests use [account.updateStatus](https://core.telegram.org/method/account.updateStatus). Network loss can prevent an immediate offline update; Telegram's own expiry still applies. The current real-account window was not restarted or driven during this follow-up. The preceding startup report contains the earlier real-account read-only checks.

## Final acceptance gate

```text
ok   probe-1 (235 probe lines)
ok   probe-2 (235 probe lines)
ok   probe-3 (235 probe lines)
ok   probe-4 (235 probe lines)
ok   probe-5 (235 probe lines)
ok   probe-6 (235 probe lines)
ok   auth-1 (238 probe lines)
ok   auth-2 (238 probe lines)
ok   auth-3 (238 probe lines)
ok   latency (235 probe lines)
ok   seed-info (237 probe lines)
ok   seed-sidebar (236 probe lines)
gate: PASS
```

Command: `GATE_LOGS=target/background-chat-fixes/final-gate-logs bin/gate`.
Debug and release binary hashes remained unchanged throughout acceptance;
`target/background-chat-fixes/final-binary-hashes.txt` records all four built
artifacts. Tests/Clippy/build logs use the `tests-9`, `clippy-9`,
`no-default-9`, `build-9` and `release-final` names in the same directory.
`git diff --check` and the staged equivalent pass. The launcher references the
rebuilt `target/release/omarchygram`; restart an existing app instance to load it.

The earlier review/startup reports retain their historical no-commit notes;
the subsequent user authorization covers committing and pushing the combined
fixes recorded across these reports.

## Files changed in this follow-up

- `Cargo.toml` — Enable serialization for cached timestamps.
- `Cargo.lock` — Resolve the adjusted feature graph.
- `src/main.rs` — Close-to-hide, hidden startup, single-instance reopen, explicit quit and geometry persistence.
- `src/settings.rs` — Persist the background close-behavior preference.
- `src/tg/mod.rs` — Presence/cache commands and full-message serialization.
- `src/tg/history_cache.rs` — Private, bounded account/topic cache and regression tests.
- `src/tg/real.rs` — Presence lifecycle, cache reads/writes/mutations, history timeout and downloaded-media reuse.
- `src/tg/mock.rs` — Cache command contract, presence command and long-pin fixture.
- `src/ui/menus.rs` — Hide and Quit menu actions.
- `src/ui/settings_view.rs` — Background preference and help text.
- `src/ui/chatlist.rs` — Centered search field and shared header sizing.
- `src/ui/messages.rs` — Bounded pins, cache refresh/retry UI, live-update reconciliation and responsive search.
- `src/ui/shell.rs` — Activity-based presence, priority history loading, menu actions, shared sizing and native regressions.
- `src/theme/style.css` — Aligned top bars, entry styling and refresh status.
- `tests/mock_backend.rs` — Recent-history cache contract.
- `tests/background_instance.py` — Reproducible isolated CLI lifecycle regression.
- `README.md` — Background, presence and cache behavior.
- `TODO.md` — Completed requested follow-ups and remaining live-validation limits.
- `specs/review-fixes.md` — Link to this follow-up while preserving historical review results.
- `specs/background-and-chat-ux.md` — Implementation, evidence and limitations.
