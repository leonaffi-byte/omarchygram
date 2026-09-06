# Rendering performance — 0.1.5

The optimized release reaches 117.68–120 FPS across chat-list and message
scrolling in the populated 1.6× GPU reproduction, with all effects enabled.
Exact pixel comparisons and all twelve final UI acceptance runs pass.

## Changes

The message list keeps its complete GTK layout/widget tree, including focus,
selection, accessibility, media and history. A GtkBox subclass skips snapshotting
rows wholly outside the scrolled viewport plus 128 logical pixels of overflow
allowance. Scroll and allocation changes invalidate that snapshot. It does not
remove messages, change paging, lower media resolution, or change animation
preferences.

With GTK's GPU renderers, scanlines use a cached, one-physical-pixel-wide Cairo raster stretched
horizontally by GTK. The existing color, opacity, line spacing and vertical
resolution remain unchanged. The cache replays the original Cairo render node
on the renderer's float pixel grid, including the widget's position below the
client-side title bar. This preserves recording-surface antialiasing at fractional
scales. Cache keys include window width/height, display scale, window-relative
vertical position and theme color; unmapping releases it. A surface-scale
notification invalidates drawing when moving between display scales, including
changes that leave GTK's rounded integer scale factor unchanged. At 1800 × 1100
logical pixels and 2× scaling the column is about 8.3 KiB. There are no new
application dependencies. Software Cairo rendering retains its original vector
path: stretching an image in software both costs more and rounds fractional clip
edges differently. The software path also passes the exact pixel comparisons;
the 100 FPS result is for the GPU renderers, not forced software rendering.

An early candidate retained full-width Cairo drawing at fractional scales. A
private 1.6× client exposed that this still dropped message scrolling to roughly
60 FPS, so that candidate is not shipped. The column cache now also covers
fractional scaling, with pixel equality checks at the actual window offset.

## Measurement

The previous reproduction used a 60 Hz private Weston output and a 16 ms scroll
timer. It could not establish a 100 FPS result. The improved harness allows a
120/144 Hz output and a GL compositor exposing this machine's actual Intel ARL
GPU buffers. The previous headless GL run used software Mesa instead.

The probe drives a fixed scroll speed from GTK's frame clock and retains
GdkFrameTimings until presentation feedback arrives. FPS counts unique completed
presentation timestamps per elapsed wall-clock second, not just adjustment or
paint callbacks. It also reports presentation interval p95/p99, main-loop timer
latency and process CPU. These are private compositor measurements on this
machine, not a live-account desktop recording. No desktop window, input, display
setting, real session, network message or user preference is changed.

The populated workload has 1,000 unread dialogs and 500 group messages with
wrapping, sender avatars, replies and reactions. Optional mixed media adds 50
distinct loaded 640 × 480 photo textures, 50 voice-message cards and 50 document cards. The info
phase scrolls that history with the native group details panel and its twelve
member rows open. Voice/video
playback and other whole-app interactions are checked by the separate full gate;
this scrolling benchmark does not measure simultaneous video decoding or network
latency.

The pre-change development-example populated text workload at 1800 × 1100 logical pixels, 2× scale,
120 Hz, GL measured 77.74 FPS with ambient effects, 113.44 FPS scrolling the
sidebar, and 76.69 FPS scrolling messages. Viewport culling alone measured
81.96 FPS scrolling messages (82.65 in an optimized example build). Profiling
identified full-window scanline Cairo clears/uploads as the remaining bottleneck.

## Final release results

On the Intel ARL GPU, GTK selected Vulkan automatically. With 1,000 chats,
500 group messages, 50 distinct loaded photos, 50 voice cards, 50 document cards,
all optional effects enabled, a 1800 × 1100 window, native 1.6× client scaling and
120 Hz private output, the optimized release produced these results. Each phase
lasted ten seconds, with no other test or build running:

| Scenario | Presented FPS | Presentation interval p95 | Process CPU (one core = 100%) |
| --- | ---: | ---: | ---: |
| Ambient effects, no scrolling | 118.80 | 8.334 ms | 39.02% |
| Chat-list scrolling | 117.68 | 10.214 ms | 39.89% |
| Mixed-media message scrolling | 119.88 | 8.334 ms | 40.13% |
| Message scrolling with details/members open | 120.00 | 8.334 ms | 43.83% |

The optimized 2× GL text reproduction measured 111.50 FPS for ambient effects,
116.75 FPS scrolling the sidebar, and 119.75 FPS scrolling messages. Message-scroll
CPU was 39.37% of one core with 8.334 ms p95/p99 presentation intervals. This is
the same geometry, renderer and text workload as the earlier development-example
baseline; build profiles differ, so it is not an isolated compiler-independent
speedup measurement. Log: `target/fps-100/shipping-2x-text-final.log`.

Both 1.6× message-scrolling phases also had 8.334 ms p99 presentation intervals. The
four sustained phases exceed the 100 FPS target. This does not promise a minimum
of 100 FPS for every individual frame, hardware configuration, media decoder,
network operation, or real-account interaction. It is a private GPU reproduction,
not a measurement from the user's open desktop session.

The final release also passed all nine exact pixel comparisons at native 1.6×.
Earlier GL runs passed the same comparisons and reached about 120 FPS while
scrolling messages. Software Cairo retains the original drawing and passed the
same visual checks; the large software-rendered workload remains below 100 FPS.

With optional effects off, a separate ten-second idle measurement used 4.75% of
one CPU core with the same loaded mock history. Scrolling measured 118.70/119.48
FPS for the sidebar/messages and 31.10%/25.73% CPU respectively. That measurement
ran alongside the functional gate and includes the probe's 4 ms diagnostic timer;
it is not a battery-life or background-process measurement. FPS establishes
smoothness in this workload; idle CPU and memory are separate efficiency metrics.

Logs: `target/fps-100/shipping-fractional-final.log`,
`shipping-fractional-unique-media-under-gate.log`, `final-debug-fractional-gl.log`,
`shipping-cairo-visuals.log`, and `shipping-idle-default-effects.log`.

## Validation and footprint

All 106 tests passed: 79 library, 3 configuration, 20 mock backend and 4 theme
tests. Strict all-target Clippy, the all-target check without calling support,
Python syntax checks, shell syntax checks, and debug/release builds passed.
Default calling support remains enabled. There are no new runtime dependencies.

The full, unchanged `bin/gate` passed all twelve cases on the final code with
`OMG_HEADLESS_RENDERER=gl GSK_RENDERER=vulkan`, exercising the GPU cache. Every run retained `G_DEBUG=fatal-criticals` and
the minimum traversal requirement. No warnings or critical errors appeared in
these final gate logs. Media, avatars/profile actions, notification pictures,
selection/history, settings, authentication, delayed responses, and saved-layout
flows retain their existing checks. An earlier full Cairo gate also passed;
separate final software-renderer pixel checks cover the retained fallback.

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

The release executable is 56,660,768 bytes: 17,088 bytes (0.0302%) larger than
0.1.4. The scanline pixel payload is 6,792 bytes at the tested 1.6× layout
(8,488 bytes at 2×), rather than a full-window raster. GPU allocation overhead is
renderer-dependent; total process RAM was not measured in this change. The
larger cached vignette from 0.1.4 is unchanged.

The existing desktop launcher already points to `target/release/omarchygram`.
The release binary was rebuilt; no desktop window was opened or restarted.
Fully quit and reopen the app, then check **About → 0.1.5**.

## Reproduce

```sh
cargo build --offline --release --example frame_perf_probe
OMG_HEADLESS_REFRESH=120000 OMG_HEADLESS_SCALE=2 \
OMG_HEADLESS_WIDTH=3600 OMG_HEADLESS_HEIGHT=2400 \
OMG_HEADLESS_RENDERER=gl GSK_RENDERER=gl \
OMG_PERF_WIDTH=1800 OMG_PERF_HEIGHT=1100 OMG_PERF_MESSAGES=500 \
OMG_PERF_MEDIA=1 OMG_PERF_INFO=1 OMG_PERF_SECONDS=10 OMG_PERF_VERIFY=1 \
bin/headless ./target/release/examples/frame_perf_probe all
```

For a real fractional-resolution client buffer, add the proxy:

```sh
OMG_HEADLESS_REFRESH=120000 OMG_HEADLESS_SCALE=2 \
OMG_HEADLESS_WIDTH=3600 OMG_HEADLESS_HEIGHT=2400 \
OMG_HEADLESS_RENDERER=gl OMG_PROXY_SCALE=192 OMG_PERF_EXPECT_SCALE=1.6 \
OMG_PERF_WIDTH=1800 OMG_PERF_HEIGHT=1100 OMG_PERF_MESSAGES=500 \
OMG_PERF_MEDIA=1 OMG_PERF_INFO=1 OMG_PERF_SECONDS=10 OMG_PERF_VERIFY=1 \
bin/headless env -u GSK_RENDERER python3 examples/fractional_scale_proxy.py \
  ./target/release/examples/frame_perf_probe all
```

The proxy advertises the fractional-scale protocol, forwarding the actual
Wayland surface, GPU buffers and presentation feedback to Weston. GTK confirms
`surface_scale=1.6`; the integer `scale_factor=2` alone would not establish this.
The compositor still has an integer-scale output and scales the client buffers
through its real viewporter. This is an isolated rendering reproduction, not
an actual Hyprland desktop capture. The proxy refuses desktop display names and
commands other than the mock frame probe. It needs only Python's standard library
and is not part of the application runtime.

Omit `OMG_PERF_MEDIA` for text history. The `none` mode disables optional effects.
To test GTK's automatic renderer selection, put `env -u GSK_RENDERER` between
`bin/headless` and the example. All GUI runs must stay inside `bin/headless`.
`OMG_HEADLESS_LOG` can retain the private compositor log and GPU identity.

`OMG_PERF_VERIFY=1` compares full and culled message snapshots captured in the
same GTK frame at five scroll positions, including both ends. It compares every
rendered byte at the output scale. Scanline checks compare original and shipping
paths at 1×, 1.25×, 1.6× and 2×, including the client-side title-bar offset.
The comparison uses integer framebuffer bounds so render_texture does not
introduce an extra resize after applying the requested fractional scale.
These checks complement the full functional gate; they do not constitute an
exhaustive pixel comparison of every possible conversation and theme.

GTK presentation timing semantics: [FrameTimings.get_presentation_time](https://docs.gtk.org/gdk4/method.FrameTimings.get_presentation_time.html).
The private output refresh override follows [Weston's headless options](https://cgit.freedesktop.org/wayland/weston/tree/man/weston.man).

## Files

- `Cargo.toml` — version 0.1.5.
- `Cargo.lock` — application version.
- `README.md` — rendering behavior and reproduction link.
- `bin/headless` — optional refresh rate, compositor renderer and retained log.
- `examples/frame_perf_probe.rs` — populated/media workloads, presentation timing, exact pixel checks and profiler control.
- `examples/fractional_scale_proxy.py` — isolated fractional-scale protocol adapter for reproducible native 1.6× client buffers.
- `src/ui/viewport.rs` — viewport-aware GtkBox snapshotting and comparison capture.
- `src/ui/messages.rs` — message-list integration and probe hooks.
- `src/ui/mod.rs` — viewport module registration.
- `src/ui/anim/scanlines.rs` — cached scanline column, pixel-grid replay and scale lifecycle.
- `src/ui/anim/overlays.rs` — scanline widget integration.
- `src/ui/anim/mod.rs` — scanline module and visual-check hook.
- `specs/100-fps-performance.md` — implementation, measurement and validation report.
