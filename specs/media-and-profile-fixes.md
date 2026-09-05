# Media recovery, sender profiles and notification photos

Implemented September 5, 2026, after reports that images and voice messages did not work reliably. The user also requested sender profiles, larger profile photos and photos in desktop notifications. All GUI validation uses `bin/headless`; the user's running Telegram session is not driven or restarted.

## Fixes

- Cached history contains message descriptions, not Telegram download references. The old media path returned `None` on a missing reference/file, and the UI made that failure permanent. A cache miss now fetches the specific message before deciding availability. Expired `FILE_REFERENCE_*` errors refresh metadata and retry once. Network failures remain retryable; deleted/unsupported media can still return unavailable. Individual metadata requests have a 20-second timeout. Transfers retain the SDK's parallel downloader and have no new total-transfer deadline, preserving large downloads on slow links.
- Explicit Retry replaces only completed cache files belonging to the exact account, chat and message, under the download writer's lock. It cannot remove similarly named messages, symlinks or partial files. Existing atomic writes and four-transfer concurrency remain in place.
- Voice decoder/output failures expose Retry. Retrying an errored player downloads again and starts playback. Deferred error cleanup cannot tear down a successful later attempt. Retry clears the old error while downloading; a superseded request leaves its Play button usable without auto-starting. A monotonic Play/Pause generation prevents an older voice download from interrupting a newer track. Probe playback uses a silent GStreamer sink; normal playback no longer has an explicit silent-sink fallback.
- The photo viewer decodes away from GTK's main thread, shows decoding/download failures with Retry, and rejects old decode completions by open/current-photo/decode generation. Closing releases the displayed texture and paths. Shared-photo errors no longer leave a permanent spinner.
- Group sender names are accessible links to a profile overlay. Details show available username, phone, biography and presence, with Message and Call actions. Opening the overlay preserves the group and does not create a private dialog. Message opens the private conversation; delayed profile/action results cannot replace a newer selection. Anonymous/channel senders are not treated as individual users. Group members use the same profile flow.
- Non-contact senders are resolved through the source message. Telegram's minimal user records can be resolved with `inputUserFromMessage` before fetching full details; a contacts-list fetch is not required.
- Profile photos in sender details and Chat info open in the large viewer, with Save, Open externally, Close and Escape. Large downloads use Telegram's `big` flag and a separate cache filename; Retry refreshes metadata and the cached photo. Full group-info responses now retain and update group photo metadata.
- Notifications carry the private conversation's user photo or the group's photo, never an individual member's photo for a group. `GFileIcon` is passed to `GNotification`; the [GLib freedesktop backend](https://github.com/GNOME/glib/blob/main/gio/gfdonotificationbackend.c) translates file icons into notification image paths. A lookup waits at most 800 ms before using the app icon, so unavailable photos do not prevent notifications. Pending lookups are bounded and discarded after a newer notification, chat activation, mute or logout. Probes observe notification payload selection without posting notifications to the desktop.

No new dependencies, services or desktop configuration changes were introduced. Existing calling support remains enabled in the standard build.

## Validation

The focused native traversal covers non-contact sender details without navigation, avatar loading, enlarged photos and return focus, the Message action, superseded profile requests, group-photo expansion, corrupt-image Retry, actual GStreamer voice failure/recovery with an advancing playback clock, private/group notification photo identity, and cancellation of pending notifications when a chat opens. It also runs inside the full gate before the existing active-playback/logout checks.

Backend tests cover cold-reference recovery, expired-reference replacement and retry limits, temporary versus permanent unavailability, unrelated failures, exact-message cache discovery and separate large-photo filenames. The mock contract verifies that viewing a non-contact profile does not create a chat and Message does.

Screenshots are generated from mock data at `target/media-profile-fixes/screenshots/`. The profile overlay and enlarged viewer have been visually inspected. Log files and build-size measurements remain under `target/media-profile-fixes/`.

Live Telegram downloads, physical speaker output and the user's notification daemon appearance were not exercised. The implemented recovery paths and interface behavior are verified with deterministic fixtures, real GTK/GStreamer execution, backend unit tests and GLib's notification API contract. Photos can still be unavailable because of Telegram privacy/deletion or connectivity; the UI reports failure and permits an explicit retry.

## Files changed

- `src/tg/real.rs` — recover media references, replace corrupt cache entries, resolve sender profiles and fetch full-size profile photos.
- `src/tg/mod.rs` — profile lookup, photo-size and retry commands across the UI/backend boundary.
- `src/tg/mock.rs` — non-contact profile and media API fixtures.
- `src/ui/profile.rs` — profile overlay, details, keyboard traversal, Message/Call/photo actions.
- `src/ui/messages.rs` — sender links and retryable playback state.
- `src/ui/info_panel.rs` — clickable profile photo and photo visibility state.
- `src/ui/viewer.rs` — profile-photo mode, background decoding, visible Retry and stale-result protection.
- `src/ui/player.rs` — retryable audio errors and guarded deferred cleanup.
- `src/ui/shell.rs` — connect profiles, media recovery, notification photos and native regression probes.
- `src/ui/mod.rs` — register the profile component.
- `src/ui/anim/mod.rs` — escape animation text and preserve sender-link markup when dissolving rows.
- `src/theme/style.css` — profile and sender-link styling using existing theme tokens.
- `tests/mock_backend.rs` — non-contact profile/chat-creation contract.
- `README.md` — user instructions for profiles, photos, retries and notification pictures.
- `specs/media-and-profile-fixes.md` — behavior, validation and remaining verification limits.

## Build checks and size

- `cargo test --lib --tests`: **99 passed** (72 library, 3 config, 20 mock backend, 4 theme).
- `cargo clippy --all-targets -- -D warnings`: **passed**.
- `cargo check --no-default-features --all-targets`: **passed**.
- Default debug and release builds: **passed**, including calling support. The optimized native media/profile traversal also **passed**.
- Optimized binary: **56,491,648 bytes**, versus 56,362,304 bytes before this change: **+129,344 bytes (+0.229%)**. No dependency changes.

The initial quick gate exposed a stale image decode replacing a newer failure state. Decode generations fix that race; the fault-injection assertion remains in the traversal. The final full gate below is the acceptance result for the corrected implementation.

An intermediate full-gate run also reproduced pre-existing markup warnings in the scramble animation. Animation frames now escape generated glyphs, and deletion effects animate visible text before restoring the original link/span markup. The intermediate gate was stopped so the final run includes this fix.

Repeated playback checks caught a late voice completion starting after a newer music request. Manual playback generations now keep the latest user action in control. A deterministic native regression completes an older voice request only after the newer track is playing, and checks both uninterrupted music and the older voice's usable Play control. The existing music/player assertions remain unchanged.

## Final acceptance gate

`GATE_LOGS=target/media-profile-fixes/gate-final-2 bin/gate`: **PASS**. All twelve runs exited 0, with no GTK warnings or criticals.

```text
ok   probe-1 (245 probe lines)
ok   probe-2 (245 probe lines)
ok   probe-3 (245 probe lines)
ok   probe-4 (245 probe lines)
ok   probe-5 (245 probe lines)
ok   probe-6 (245 probe lines)
ok   auth-1 (248 probe lines)
ok   auth-2 (248 probe lines)
ok   auth-3 (248 probe lines)
ok   latency (245 probe lines)
ok   seed-info (247 probe lines)
ok   seed-sidebar (246 probe lines)
gate: PASS
```
