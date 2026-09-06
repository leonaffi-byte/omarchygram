# Omarchygram 0.1.7 performance measurements

Recorded 2026-09-06 on the installed release from commit `0ddbe71`.

Measurement correction: the original standalone search harness counted matching chats without attaching Shell's result-rendering callback. Its 183.85 ms measures query processing and a frame, not fully rendered search results. The newer harness attaches that callback and checks mapped result content; do not compare the two search numbers as equivalent workloads.

The audit separates scrolling smoothness, interaction latency, backend/local service work, and resource use. High scrolling FPS does not mean that opening a chat is fast.

## Findings

- Local archive deletion bookkeeping: marking 100 entries deleted took 30.64, 0.150, 0.161 seconds in the recorded service runs (median 160.89 ms). Source inspection shows batched upserts but individual deletion updates in `src/tg/archive.rs::mark_deleted`. Grouping deletion updates is a profiling/optimization candidate; the precise cause of any storage stalls was not traced.
- The local message-view path is the main measured interaction bottleneck: rendering 50 messages took 131.42 ms; the 500-message stress case took 1283.33 ms. Reading a 50-message history page from the warm cache fixture on storage-backed Btrfs took 0.078 ms. This points to view construction/layout/painting as the next profiling target; it does not exclude network delays in the real account.
- Local search across 1,000 chats took 183.85 ms including debounce. Updating the composer after search settled took 16.01 ms. These are distinct delays.
- A first 12 MP image preview took 72.65 ms; decoded-cache reuse took 0.0019 ms. Prioritizing visible previews and retaining bounded decoded textures remains valuable.
- Current-effect scrolling medians were 113.78 FPS for the chat list, 119.57 FPS for messages, and 118.18 FPS with the info sidebar open, on a private 120 Hz output. That is smooth frame delivery in this fixture, with room to reduce rendering CPU work.
- Effects increased the small visible mock session's idle CPU from 0.300% to 4.62% of one core. Both hidden-background cases consumed less than the counter resolution, approximately 0.04% of one core. The loaded rendering fixture has separate, much higher resource costs; it is not the small idle session.
- These results support prioritizing archive deletion stalls, chat layout, search response and animated idle work. They do not establish real Telegram delivery speed, Groq response time, or the physical display's FPS.

## Test environment and interpretation

- CPU: Intel(R) Core(TM) Ultra 9 285H; 16 logical CPUs; 30.71 GiB RAM.
- Linux 7.1.9-arch1-2; GTK 4.22.4; GStreamer 1.28.6; Mesa 26.2.1; Intel ARL integrated GPU (i915).
- CPU governor `powersave`, energy preference `performance`, observed from sysfs; neither was changed.
- Optimized release builds; all application source is unchanged from 0.1.7. Only benchmark tools were added/extended.
- Timed workloads ran sequentially without concurrent builds or gates. The existing desktop session remained running; other machine activity was not globally controlled.
- All launched UI runs used `bin/headless`, offline mock Telegram, and private state. No real chat content, outgoing messages, calls, microphone/camera recording, or AI-provider requests were used.
- UI/startup: 1100×720 requested window, 1× scale, private 60 Hz output, Vulkan. UI latency ends at GTK after-paint, not photons on the user's physical display.
- Scrolling: 1800×1100 logical window, verified native 1.6× client scale, private 120 Hz output, Vulkan; 1,000 chats, 500 group messages, 50 loaded photos, 50 voice cards and 50 document cards. Video was not playing during scrolling.
- Three 10-second repetitions per rendering mode/phase. Presentation feedback supplies FPS. The 4 ms diagnostic timer adds overhead: use the separate release-process measurements for idle efficiency.
- CPU 100% means one fully occupied logical CPU, not the whole 16-CPU machine. RSS includes shared mappings; PSS apportions them. GPU allocations overlap other memory accounting and must not be added to RSS/PSS.
- Fresh processes/profiles were measured, but the OS disk cache was not dropped. File reads are OS-warm. `uncached decode` means the app's decoded-image cache was cleared.
- Service fixtures use storage-backed Btrfs. UI/startup profiles and rendering fixtures use RAM-backed `/tmp`; those mock startup numbers do not include a real account's cold storage load. The service run uses private synthetic files, not the user's cache/archive.
- 3 fresh service process(es) supply the pooled service samples below. Outliers are retained. An earlier tmpfs reference, when present, is preserved separately in the combined JSON and excluded from the final service timing distributions.
- Local p95 uses the nearest-rank sample percentile. Rendering percentiles use the probe's lower order statistic at floor((n−1)×p). Medians use the middle value or mean of the two middle values. Small sample counts are not stable estimates of long-run tails. No error rate can be inferred from a handful of successful runs.

## Startup, UI, caches, decoding and preparation

`sync` means elapsed wall time around the synchronous function, not exclusive thread CPU time. Widget build excludes the following layout/paint; frame timings include it. Local search waits for matching results, including GtkSearchEntry's debounce. Composer timing starts after search reset settles. The 500-message cached-view measurement directly stresses the view's cached-content path; the actual persistent cache stores only 50 messages per page.

Shell first-open and reopen distributions pool two different small mock chats across five fresh processes. They describe that mixed fixture, not every real chat. Native UI history rows use synthetic text-only group messages; the scrolling fixture below also includes loaded media.

| Measurement | Median (ms) | Sample p95 (ms) | Min–max (ms) | Samples |
| --- | ---: | ---: | ---: | ---: |
| history cache 50 write flush | 1.87 | 6.05 | 1.47–19.48 | 90 |
| history cache 50 read os warm | 0.078 | 0.542 | 0.062–0.777 | 90 |
| history cache miss | 0.014 | 0.374 | 0.0079–0.594 | 90 |
| archive new database open | 12.32 | 23.76 | 11.29–23.76 | 3 |
| archive 1000 message batch and barrier | 41.89 | 71.40 | 29.76–71.97 | 30 |
| archive prior-version lookup (empty result; growing database) | 0.040 | 0.668 | 0.019–0.729 | 30 |
| archive mark 100 deleted | 160.89 | 30638.49 | 149.66–30638.49 | 3 |
| archive query 100 deleted | 0.225 | 0.466 | 0.175–0.850 | 90 |
| photo 12mp to 720px uncached decode | 72.65 | 106.90 | 65.80–128.10 | 45 |
| photo 720px memory cache hit | 0.0019 | 0.0097 | 0.0010–0.018 | 45 |
| photo 12mp full texture decode | 27.57 | 36.03 | 22.72–38.97 | 45 |
| voice 30s opus first decoded buffer | 4.66 | 9.90 | 3.16–10.77 | 30 |
| voice 30s opus full decode unpaced | 42.62 | 48.98 | 38.48–50.68 | 30 |
| video 10s h264 720p30 first decoded buffer | 18.46 | 58.28 | 15.58–62.52 | 30 |
| video 10s h264 720p30 full decode unpaced | 90.75 | 149.03 | 82.36–165.10 | 30 |
| sticker 64px parse | 0.129 | 0.141 | 0.105–0.141 | 3 |
| sticker 64px frame raster | 0.0082 | 0.011 | 0.0059–0.046 | 360 |
| sticker 192px parse | 0.037 | 0.046 | 0.032–0.046 | 3 |
| sticker 192px frame raster | 0.020 | 0.027 | 0.016–0.037 | 360 |
| sticker 384px parse | 0.062 | 0.084 | 0.061–0.084 | 3 |
| sticker 384px frame raster | 0.046 | 0.061 | 0.036–0.072 | 360 |
| markup 2250 chars two spans sync | 0.0092 | 0.012 | 0.0085–0.013 | 90 |
| summary 2000 messages prepare sync | 1.30 | 1.66 | 1.20–2.16 | 90 |
| chat list 1000 first build to frame | 354.06 | — | 354.06–354.06 | 1 |
| chat list 1000 unchanged snapshot to frame | 15.76 | 16.87 | 12.14–16.87 | 4 |
| history 50 widget build sync | 7.06 | 8.10 | 6.44–8.10 | 7 |
| history 50 initial to frame | 131.42 | 213.06 | 128.13–213.06 | 7 |
| history 50 cached to frame | 131.84 | 140.51 | 131.30–140.51 | 7 |
| history 50 unchanged refresh to frame | 14.21 | 23.28 | 12.88–23.28 | 7 |
| history 500 widget build sync | 71.77 | 78.69 | 69.69–78.69 | 7 |
| history 500 initial to frame | 1283.33 | 1348.12 | 1270.24–1348.12 | 7 |
| history 500 cached to frame | 1288.98 | 1326.35 | 1276.92–1326.35 | 7 |
| history 500 unchanged refresh to frame | 22.22 | 25.00 | 20.87–25.00 | 7 |
| unchanged sidebar row update sync | 0.811 | 1.29 | 0.689–1.59 | 30 |
| incoming sidebar update to frame | 22.22 | 26.30 | 16.21–75.14 | 30 |
| local chat search 1000 to frame | 183.85 | 197.62 | 176.97–211.03 | 30 |
| composer update to frame | 16.01 | 18.40 | 12.96–44.04 | 30 |
| message jump to frame | 29.72 | 32.41 | 26.42–33.11 | 30 |
| startup shell construct from main | 74.62 | 86.12 | 67.99–86.12 | 5 |
| startup mock chat list painted from main | 724.14 | 776.89 | 694.08–776.89 | 5 |
| shell mock first chat open to messages painted | 178.88 | 266.08 | 89.52–266.08 | 10 |
| shell mock reopen chat to messages painted | 33.30 | 103.15 | 27.40–106.01 | 30 |
| contacts dialog open to frame | 34.51 | 36.73 | 34.38–36.73 | 5 |

Media decoding uses generated JPEG/Opus/H.264 fixtures and silent fake sinks. First buffer is decoder readiness, not audible sound or display latency. Unpaced full decode runs as fast as possible; it is not realtime playback or A/V synchronization. The image fixture is 4000×3000 and the preview is 720×540. Sticker raster timings use the bundled 60-frame fire animation, not every possible sticker.

Archive batches include an ordered query barrier so completion includes the writes, rather than just enqueuing them. This and cache flush measure completion of the application write path, not an fsync durability guarantee. Cache reads include the actual worker handoff and JSON load. Archive version lookups return no prior edits while the messages table grows from 1,000 to 10,000 rows; retrieving a large edit history was not measured. Summary preparation covers 2,000 synthetic text messages; it excludes network inference and voice transcription.

## Scrolling and frame delivery

Values are median across three runs; parentheses show the FPS range. Tail columns show the worst per-run p99. Idle presentations measure drawing activity, not a smoothness target: an unchanged screen need not draw continuously. `current` uses the captured animation toggles; `all` uses the bundled all-effects fixture.

| Effects / phase | Presentations/s (range) | CPU % | Frame interval p99 (ms) | GTK frame work p99 (ms) | 4 ms timer interval p99 (ms) | Intervals >16.8 ms | Max end RSS / PSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none / idle | 17.00 (17.00–17.20) | 8.19 | 60.11 | 7.35 | 8.85 | 35/509 | 351.4 / 301.3 |
| none / sidebar scroll | 115.30 (114.50–116.40) | 47.54 | 15.11 | 11.68 | 13.19 | 5/3459 | 351.3 / 301.3 |
| none / message scroll | 119.70 (119.70–119.79) | 47.80 | 8.33 | 6.35 | 9.62 | 0/3589 | 351.3 / 301.3 |
| none / message scroll with info | 119.70 (119.69–119.70) | 45.82 | 8.33 | 6.76 | 9.78 | 0/3588 | 351.4 / 301.3 |
| current / idle | 116.39 (115.20–117.70) | 62.04 | 16.67 | 8.63 | 11.89 | 11/3490 | 380.9 / 330.7 |
| current / sidebar scroll | 113.78 (113.36–114.38) | 62.45 | 14.91 | 12.32 | 14.68 | 3/3413 | 380.8 / 330.6 |
| current / message scroll | 119.57 (119.06–119.80) | 59.92 | 10.43 | 8.28 | 10.35 | 1/3582 | 380.8 / 330.5 |
| current / message scroll with info | 118.18 (117.85–118.79) | 65.87 | 11.19 | 8.94 | 10.98 | 0/3546 | 380.9 / 330.7 |
| all / idle | 117.00 (114.10–117.50) | 65.21 | 16.67 | 8.91 | 12.15 | 12/3483 | 381.2 / 330.8 |
| all / sidebar scroll | 114.04 (113.07–114.10) | 60.59 | 15.84 | 12.48 | 14.65 | 4/3410 | 381.1 / 330.8 |
| all / message scroll | 119.60 (119.40–119.75) | 61.80 | 10.28 | 8.06 | 10.40 | 0/3585 | 381.2 / 330.8 |
| all / message scroll with info | 117.99 (117.38–118.00) | 69.00 | 10.96 | 9.20 | 11.00 | 0/3531 | 381.2 / 330.8 |

The raw JSON also contains completed/posted frame counts, counts above 33.4 ms, phase duration, memory high-water marks, minor/major faults, context switches, file descriptors, threads, and block I/O counters. These are available for each individual run; RSS high-water marks can include earlier phases in that process. End resource snapshots follow the 150 ms presentation-feedback wait; the CPU percentage uses the timed workload window itself.

The all-effects first run passed five exact viewport pixel comparisons and four scanline scale comparisons (1×, 1.25×, 1.6×, 2×).

## Whole release process and background behavior

Each idle window is 30 seconds after settling. These are the exact installed executable with the small mock account; the visible case has no chat open. Accessibility was disabled only in the isolated single-instance lifecycle test to avoid desktop service activation. It remains enabled in UI/gate runs.

| Effects / state | CPU % of one core | RSS range (MiB) | PSS range (MiB) | End threads / FDs | Block bytes read / written during sample |
| --- | ---: | ---: | ---: | ---: | ---: |
| none / background idle | <0.04 (counter resolution) | 65.9–66.0 | 28.9–29.0 | 16 / 26 | 0 / 0 |
| none / visible idle | 0.300 | 106.5–106.6 | 50.2–50.4 | 21 / 39 | 0 / 0 |
| current / background idle | <0.04 (counter resolution) | 66.1–66.2 | 29.2–29.3 | 16 / 26 | 0 / 0 |
| current / visible idle | 4.62 | 110.3–110.4 | 53.5–53.8 | 21 / 39 | 0 / 0 |

| Lifecycle action | Effects off (ms) | Current effects (ms) |
| --- | ---: | ---: |
| process to dbus registered | 32.74 | 32.31 |
| single instance activation roundtrip | 33.29 | 33.49 |
| explicit quit complete | 31.87 | 35.10 |

D-Bus registration is not chat-list readiness. Activation measures the secondary process round trip, not focus-to-pixel latency. Explicit quit includes termination of the primary mock process.

## Injected delay and repeated chat rebuilds

The separate 400 ms mock-delay run tests a deliberate delay, not Telegram RPC latency.

| Action with injected delay | Median (ms) | Min–max (ms) | Samples |
| --- | ---: | ---: | ---: |
| startup shell construct from main | 82.61 | 82.61–82.61 | 1 |
| startup mock chat list painted from main | 750.32 | 750.32–750.32 | 1 |
| shell mock first chat open to messages painted | 533.90 | 431.39–636.40 | 2 |
| shell mock reopen chat to messages painted | 33.35 | 32.80–42.11 | 6 |
| contacts dialog open to frame | 33.19 | 33.19–33.19 | 1 |

Across 60 additional 50-message chat rebuilds, RSS changed from 271.11 to 271.02 MiB, PSS from 224.82 to 224.73 MiB, and file descriptors from 36 to 36. This short test showed no retained-memory growth; it cannot rule out long-term leaks.

## Existing real session: passive observations

The already-running process matched the installed 0.1.7 binary SHA-256. Over 30.0s it averaged **0.57% of one core**, with **380.0 MiB RSS**, **329.0 MiB PSS**, 24 threads and 56 file descriptors. User activity/visibility was not controlled; this is not a live scrolling or cold-start measurement.

The separate 10.0s DRM observation recorded 0.000% additional render-engine utilization and 159.7 MiB allocated GPU buffers, including 79.4 MiB shared buffers. These are integrated-GPU allocation counters, not dedicated VRAM or extra RAM to add to PSS.

## Footprint and limits

- Installed release: **56,782,880 bytes = 54.15 MiB**. ELF size reports 55,577,079 text/readonly bytes, 1,198,216 initialized-data bytes, and 3,700,160 BSS bytes (BSS is runtime virtual storage, not file size). There are 15 direct shared-library dependencies; system libraries are not bundled in this number.
- Existing cache, including mock fixtures: 64.60 MiB in 673 files; existing local app data: 2.15 MiB in 6 files. Only sizes/counts were read.
- Limits read from source, not benchmark outcomes: history cache 24 pages × up to 50 messages, at most 512 KiB/page; avatar texture cache 16 MiB/128 entries with four decode slots; chat-preview cache 32 MiB/96 entries with two decode slots.
- Media network transfers: four download slots plus two avatar slots. Retained media wrappers: 32. Lottie: six simultaneous animations, 64 first-frame cache entries, minimum 16 ms render interval. Auto-transcription: one active job and up to 64 queued jobs. Summary input: 2,000 messages / one million characters, 12,000-character chunks.

## Measurable categories requiring additional live/instrumented runs

| Category | Metrics | Why no measured value here |
| --- | --- | --- |
| Telegram transport | Authorization/reconnection time, RPC p50/p95/p99, download/upload throughput, first media byte, send acknowledgement, incoming-delivery latency | This run was local/offline. Existing mock delays are not network measurements. |
| Notifications/presence | Incoming update to desktop notification; avatar wait; notification loss; server-visible online/offline delay | Requires controlled sender/observer and desktop delivery measurement; this audit does not send notifications to the desktop. |
| AI/transcription | Provider response time, tokens/s, rate-limit frequency, voice transcription realtime factor, end-to-end selected-range summary duration | Synthetic preparation was measured; actual provider inference and speech recognition were not invoked. First-token UI latency is not exposed by the current non-streaming response UI. |
| Calls | Setup time, audio/video latency, jitter, RTT, packet loss, bandwidth, A/V sync, dropped frames | Needs a consenting call peer and call telemetry. No call was initiated. |
| Live playback/recording | Speaker latency, seek-to-visible latency, sustained dropped video frames, webcam/microphone capture latency | Silent local decoding was measured. Devices and real playback were not driven. |
| Other local UI flows | Settings/theme switching, profile and image-viewer opening, pin expansion, sticker/reaction pickers, forwarding, avatar decode and cache reuse | The functional gate exercises these paths, but separate timing probes were not added for every action. Photo timings above measure chat previews/full textures, not avatar-specific decoding. |
| Sustained update bursts | Incoming messages to painted rows, queue depth, backlog recovery, UI latency under simultaneous downloads/transcriptions | Single sidebar updates and steady scrolling were measured; a controlled multi-service burst workload was not run. |
| Long-running operation | Runtime cache hit ratios, queue depths under bursts, memory growth over hours/days, reconnect/error frequency | Short controlled samples and 60 chat rebuilds cannot establish long-term leak/error rates. |
| Energy/system load | Per-app watts, battery drain, thermal throttling, GPU busy percentage during every stress phase | Requires suitable energy/GPU telemetry and a longer controlled system baseline. Passive DRM allocation/idle engine statistics are included above. |

## Validation and reproduction

Build with `cargo build --offline --release --example performance_audit --example frame_perf_probe`, then run `python3 bin/run-performance-audit`. The required captured animation fixture is `target/performance-audit/current-effects.toml`; it contains only animation toggles and is also embedded in the combined JSON. The runner requires Weston, D-Bus tools, ffmpeg and the existing fractional-scale proxy dependencies. After any isolated UI remeasurement, run `python3 bin/summarize-performance-audit`.

This report generator uses the environment, build-provenance, disk-footprint, live-process and live-GPU snapshots in the same output directory, captured separately for this audit. Those observations must be refreshed for a new machine/session. The timing runner does not capture real-session observations or query Telegram.

Installed executable SHA-256: `411ceacdc31343d6968a56c2a3a69467849d05c7fa0af7265e782e733f703ec1`. The combined JSON preserves the final native-UI and rendering executable hashes, plus the final Btrfs service executable hash. The isolated UI remeasurement supersedes the first native-UI log; its timing code waits for search teardown before measuring typing. 3 storage-backed service run(s) supply the final service samples. Medians were normalized from raw samples when producing the report.

- Raw combined data: `target/performance-audit/measurements.json`.
- CSV: `target/performance-audit/measurements.csv`.
- Individual logs and compositor identity: `target/performance-audit/`.
- Benchmark validation and full gate results are recorded separately in that directory.

Files added/changed:

- `examples/frame_perf_probe.rs`: additional rendering/resource counters.
- `examples/perf_support/mod.rs`: shared sampling helpers.
- `examples/performance_audit.rs`: UI/local-service timing harness.
- `bin/measure-process`: release lifecycle/resource sampler.
- `bin/run-performance-audit`: sequential benchmark runner.
- `bin/summarize-performance-audit`: report and data export.
- `specs/performance-audit-0.1.7.md`: this measurement report.

No application implementation or user preference was changed.

Full headless gate output:

```text
ok   probe-1 (268 probe lines)
ok   probe-2 (268 probe lines)
ok   probe-3 (268 probe lines)
ok   probe-4 (268 probe lines)
ok   probe-5 (268 probe lines)
ok   probe-6 (268 probe lines)
ok   auth-1 (271 probe lines)
ok   auth-2 (271 probe lines)
ok   auth-3 (271 probe lines)
ok   latency (268 probe lines)
ok   seed-info (270 probe lines)
ok   seed-sidebar (269 probe lines)
gate: PASS
```

Automated tests: 112 passed, 0 failed. The final run used `bin/headless cargo test --offline`; the initial sandbox-only run could not access the local test server/media IPC.

Clippy passed with `cargo clippy --offline --all-targets -- -D warnings`.

PASS: 43 timing distributions, 36 rendering samples, 364 CSV rows; 19 primary runs plus isolated UI and three Btrfs service runs. Aggregates, units, frame timing, release identity and all 12 gate scenarios validated.
