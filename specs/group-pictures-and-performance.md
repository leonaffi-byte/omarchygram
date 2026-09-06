# Group sender pictures and UI responsiveness — 0.1.3

## Changes

Incoming group messages now have a 32px sender picture in a separate gutter.
Clicking a person's picture opens their existing profile, including Message,
Call where available, and the enlarged profile photo. Initials remain the
fallback when a photo is unavailable. The avatar preference also controls
these pictures. Regular, date-jumped and forum history register sender photo
metadata without fetching the group member list.

The sidebar reconciles its rows instead of removing and reattaching the whole
list on each chat update. Unchanged rows preserve their mapping and focus.
Sidebar pictures load only around the viewport. At most two avatar transfers
can compete for the four shared download slots.

Scrolling coalesces media/animation work every 80ms instead of scanning rows
and restarting timers on every adjustment event. Viewport lookup uses ordered
row geometry. The date label changes only when its text changes; its animation
is not restarted continuously. Animation flag lookups no longer clone every
setting, key binding, provider string and configured action.

Map completion could leave `suppress_paging` enabled indefinitely when the view
was at the bottom. It now releases that state after layout, like photo
completion, preventing stuck paging and repeated jumps back to the bottom.
A fast server refresh could also replace the cached page's initial layout
callback without releasing its paging suppression. The refresh callback now
performs that cleanup itself; an isolated 500-message case and a native gate
assertion cover refresh completion before the first layout.
Explicit search/pinned-message navigation and wheel/touchpad gestures now
cancel pending automatic scroll callbacks. A late image or cache-layout
callback can no longer override the reader's new scroll position. The native
scroll helper issues a real navigation request and waits for its allocation;
a transient pre-layout glimpse is not treated as settled visibility.

Automatic photo, sticker and map downloads target the viewport plus a small
prefetch margin, with at most two active media operations. Manual requests
start immediately. Switching chats cancels downloads owned by the old view,
including backend transfer workers, so they cannot occupy the next view's
capacity. Late results from another chat are rejected before image decoding.
Chat previews use bounded decoders and a 32MiB texture cache at the
display's scale; originals remain unchanged for viewing and saving. Repeated
sender pictures share a decode/texture, including simultaneous first loads.

Temporary connection failures and Telegram server errors retry once after
250ms. Disk, permission, session and permanent errors do not blindly retry;
expired media references retain their separate metadata refresh. Stale profile
photo references refresh once. Download errors stay on the affected row with
Retry and the detailed error as a tooltip, rather than repeatedly replacing
the chat's main error banner.

## Evidence and validation

`examples/ui_perf_probe.rs` constructs 1,000 dialogs and 500 synthetic messages
under a private headless compositor. The unchanged baseline took 2,469ms for
100 updates to an existing row and 2,540ms for 100 incoming-message updates.
The final measurement took 167.3ms and 178.0ms respectively, about 14 times
less work than the baseline. An earlier run took 74.7ms and 79.9ms; timing
varies with machine load. Ten thousand animation flag lookups took 6.63ms
versus 7.22ms; 500 scroll callbacks took 4.30ms versus 7.32ms, including the
new cancellation of pending automatic navigation. These measure UI-thread callback work,
not end-to-end frame rate or network latency. Measurements are in
`target/group-performance/performance-final.json`.

Native assertions cover mapped-row preservation, group photo loading for
repeated senders, picture-to-profile navigation, avatar preferences, narrow
window layouts, image recovery, and voice playback. Existing map/photo checks
now scroll the media into view before asserting its download, matching lazy
loading; their download/content assertions are preserved.

Unit tests cover one reconnect, a persistent failure's retry bound, permission
errors, wrapped SDK network errors, bounded preview dimensions, cache reuse,
and retaining original image bytes.

All 105 unit/integration tests passed (78 library, 3 configuration, 20 mock
backend and 4 theme tests). Strict Clippy for all targets, the all-target check
without default features, the debug build, and the optimized release build
passed. An explicit `DownloadMedia` fault-injection native run passed through
Retry, voice playback and logout. The full gate also checks that completing a
map releases paging suppression before scrolling again, and that a fast
refresh restores paging after the cached page's first layout.

The release executable is 56,621,376 bytes: 86,656 bytes (0.153%) larger than
0.1.2, with no added dependencies and calling support retained. Each preview's
dimensions and the shared preview cache are bounded; full-resolution originals
remain available.

The full `bin/gate` passed with no GTK warnings or critical errors. All GUI
checks ran through its private headless compositor. Logs are retained in
`target/group-performance/gate/`.

```text
ok   probe-1 (256 probe lines)
ok   probe-2 (256 probe lines)
ok   probe-3 (256 probe lines)
ok   probe-4 (256 probe lines)
ok   probe-5 (256 probe lines)
ok   probe-6 (256 probe lines)
ok   auth-1 (259 probe lines)
ok   auth-2 (259 probe lines)
ok   auth-3 (259 probe lines)
ok   latency (256 probe lines)
ok   seed-info (258 probe lines)
ok   seed-sidebar (257 probe lines)
gate: PASS
```

The user's real app was running, so no second real Telegram session was opened.
The reported intermittent download error was not captured in available journal
logs. Transient recovery is verified with controlled failures; this does not
establish that every possible network/provider error is resolved.

## Files

- `Cargo.toml` — version 0.1.3.
- `Cargo.lock` — application version.
- `src/settings.rs` — allocation-free animation flag access.
- `src/tg/real.rs` — sender references, download fairness and bounded recovery.
- `src/theme/style.css` — sender gutter and bubble styling.
- `src/ui/anim/mod.rs` — cheap flag reads and stable date-chip animation.
- `src/ui/avatar.rs` — shared first-load decoding and photo readiness.
- `src/ui/chatlist.rs` — incremental rows, visible-avatar loading and native assertion.
- `src/ui/messages.rs` — sender pictures, viewport scheduling and inline errors.
- `src/ui/media_image.rs` — bounded, cached previews and original-file regression test.
- `src/ui/mod.rs` — preview module registration.
- `src/ui/shell.rs` — media scheduling, profile actions and native regression coverage.
- `vendor/grammers-client/src/client/files.rs` — abort cancelled parallel download workers and avoid sending into a closed receiver.
- `examples/ui_perf_probe.rs` — isolated synthetic performance workload.
- `README.md` — group picture and loading behavior.
- `specs/group-pictures-and-performance.md` — implementation and validation report.
