# Interaction and animation performance — 0.1.4

The 0.1.3 sidebar callback benchmark did not measure rendered frames with the
enabled decorative effects. The running desktop process was confirmed to use
that release; this was not an old-executable explanation for the reported lag.

## Changes

GtkListBox keeps clipped rows mapped. Previously, every unread badge could
continue its CSS animation even when hundreds of rows were off screen. The
existing viewport scheduler now tracks animation visibility separately from
avatar prefetching. Only visible badges animate, including when avatars are
disabled. Updates to invisible badges retain their count and pulse eligibility
without starting transient animation timers. The 80 ms scheduler follows
scrolling, layout changes, row mapping, and filtering.

The vignette previously reran a full-window Cairo radial gradient while its
opacity breathed. It now caches the same pixels in a premultiplied texture,
rebuilding only for a changed size, scale, or theme color and releasing the
texture when unmapped. Gradient geometry, colors, resolution and opacity are
preserved. This adds one window-sized texture while that effect is visible;
there are no new dependencies.

The vignette's breathing and decorative flicker yield to scrolling, dragging
and typing. They resume after 350 ms without input or scroll movement. This
also covers keyboard and momentum scrolling. Existing effect preferences stay
intact; message, media and other functional animations continue normally.

An immediately ready backend event queue could run indefinitely on GTK's
thread during catch-up. Event delivery now yields through a 1 ms GLib timer
after 4 ms of processing. Every event still arrives in order. A regression
test queues twelve updates and verifies another UI task runs before the queue
finishes, without dropping or reordering any update.

## Frame reproduction

`examples/frame_perf_probe.rs` constructs 1,000 unread dialogs under a private
Wayland compositor, warms up for two seconds, then measures four seconds each
of idle rendering and actual sidebar scrolling. Its bundled fixture contains
only effect flags and compact-list layout. It never opens a real account.
The message pane is empty: this measures sidebar and overlay rendering, not
large message histories, media decoding, or network latency.

```sh
cargo build --offline --example frame_perf_probe
OMG_HEADLESS_SCALE=2 OMG_HEADLESS_WIDTH=2400 OMG_HEADLESS_HEIGHT=1600 \
  bin/headless ./target/debug/examples/frame_perf_probe all
```

Modes `none`, `badge`, `overlays`, and `all` isolate effect combinations. An
optional second argument specifies another settings fixture. `bin/headless`
retains its original default dimensions and scale; the overrides let the
compositor supply actual 2× Wayland scaling. Setting GDK_SCALE alone did not
produce a 2× surface in this environment.

With Cairo rendering at actual 2× scale, the original full-effects workload
painted 15.47 frames/s idle and 16.20 frames/s while scrolling, with a 68.29 ms
95th-percentile main-loop timer interval. Disabling all effects in the baseline
gave roughly 60 frames/s. The test records actual frame-clock paints and
process CPU time, rather than timing only adjustment callbacks.

Final Cairo results at the same 2× scale:

| Measurement | Before | After |
| --- | ---: | ---: |
| Idle paints/s | 15.47 | 34.00 |
| Scrolling paints/s | 16.20 | 50.56 |
| Scrolling timer interval, p95 | 68.29 ms | 26.52 ms |
| Scrolling process CPU, one core = 100% | 82.52% | 80.15% |

Earlier fixed runs scrolled at 46.97 frames/s. The final normal-scale Cairo
run reached 56.49 frames/s, and a 2× GL run reached 57.00 frames/s. The private
GL renderer uses software Mesa and emitted headless EGL driver-discovery
warnings; this is not a measurement of the user's hardware renderer. The
headline comparison uses Cairo for both before and after. Measurements are
retained in `target/lag-investigation/before-all.log`, `after-all-2x.log`,
`after-all-1x.log` and `after-all-gl-2x.log`.

The final probe also asserts that fewer than 40 of the 1,000 rows are eligible
to animate, that this set follows the viewport, that breathing reuses its
gradient texture, and that ambient effects pause and resume around input.
Machine load and renderer affect timings; these are synthetic reproduction
results, not a claim that every real-account interaction reaches 60 frames/s.

## Validation

All 106 unit/integration tests passed (79 library, 3 configuration, 20 mock
backend and 4 theme tests). The initial sandboxed test run could not access
the local HTTP socket, image-decoder D-Bus service, and recording helper; the
unchanged suite passed with the required access. Strict all-target Clippy,
the all-target check without calling support, and debug/example and optimized
release builds passed.

Native frame-probe assertions passed at 1× and 2×, with Cairo and GL. Debug
and optimized-release mock-chat screenshots were visually checked with the
enabled effects; both exited cleanly under `G_DEBUG=fatal-criticals`. They are
`target/lag-investigation/effects-chat.png` and `release-effects-2x.png`.

The release executable is 56,643,680 bytes, 22,304 bytes (0.0394%) larger than
0.1.3. Calling support remains enabled. The cached vignette adds one texture
while visible (about 12.1 MiB for a 1100×720 window at 2×), in exchange for
avoiding repeated gradient rasterization; it is released when unmapped.

The full `bin/gate` passed all twelve runs, with no GTK warnings or critical
errors. Logs are retained in `target/lag-investigation/gate/`. Every GUI check
ran through the private headless compositor. The user's active Telegram
process was not restarted. Fully quit and reopen the app to run 0.1.4;
closing its window only sends it to the background.

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

One standard run (`probe-6`) emitted four GStreamer stream-start/segment
warnings while starting voice playback immediately before logout. Playback,
logout and pipeline-cleanup assertions passed. Their cause has not been
isolated; the media implementation and warning handling were not changed or
suppressed in this performance patch.

## Files

- `Cargo.toml` — version 0.1.4.
- `Cargo.lock` — application version.
- `README.md` — viewport animation and input priority behavior.
- `bin/headless` — optional private compositor dimensions and scale.
- `examples/frame_perf_probe.rs` — frame/CPU reproduction and native assertions.
- `examples/fixtures/frame-effects.toml` — reproducible effect and layout flags.
- `src/theme/style.css` — stop animations on clipped chat badges.
- `src/ui/chatlist.rs` — track visible badges and sidebar scroll activity.
- `src/ui/messages.rs` — prioritize direct and momentum scrolling.
- `src/ui/anim/mod.rs` — input activity and visible-only badge effects.
- `src/ui/anim/overlays.rs` — cached vignette and ambient effect scheduling.
- `src/ui/anim/vignette.rs` — scale-aware cached gradient widget.
- `src/ui/event_loop.rs` — fair, ordered event dispatch and regression test.
- `src/ui/mod.rs` — event dispatch module registration.
- `src/ui/shell.rs` — dispatch integration and input capture.
- `specs/lag-and-animation-performance.md` — implementation and validation report.
