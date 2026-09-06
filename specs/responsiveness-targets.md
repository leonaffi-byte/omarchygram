# Responsiveness and footprint targets

User requirements, measured against the 0.1.7 performance audit:

| Requirement | Baseline | Required result |
| --- | ---: | ---: |
| 50-message view, initial GTK paint | 131.42 ms | <40 ms |
| 500-message view, first screen painted | 1,283.33 ms | <70 ms |
| Local search across 1,000 chats | Original harness was not equivalent | <50 ms |
| Loaded application RAM | 380 MiB RSS observed in real session; 351–381 MiB in rendering fixture | <200,000,000 bytes RSS |
| Release executable, all features | 56,782,880 bytes | <40,000,000 bytes |

Latency targets refer to the existing benchmark medians. Report sample p95 and
worst times too. RAM is RSS, not PSS. The user clarified on 2026-09-06 that brief
loading peaks up to 250,000,000 bytes are acceptable; loaded usage must remain
below 200,000,000 bytes. Use the existing populated rendering fixture
and repeated chat changes when checking loaded memory; a blank/background window
alone does not establish the RAM target. The already-running old executable is
not evidence for the updated build.

Preserve calls, media, avatars, message history, selection, keyboard navigation,
accessibility, animations, account isolation, and error handling. Do not count
moving code into additional bundled files as a size improvement. Do not achieve
smaller memory by removing functionality or making visible media lower quality.

Compare the existing startup, cache, decoding, scrolling FPS/tails, CPU and idle
benchmarks. All GUI runs remain private through `bin/headless`; run the full
`bin/gate` and application tests before completion. Fix any regressions rather
than weakening their checks. Keep the goal active until all targets are proven.

Baseline executable, benchmark binaries and raw measurements are preserved in
`target/responsiveness-goal/baseline/`. Profiling artifacts and candidate runs
belong under `target/responsiveness-goal/`; retain the original audit data.

## Validated results — 0.1.8

All five numeric targets pass on the final release build. The loaded-memory
result comes from four consecutive 60-second phases with 1,000 chats and
500 group messages, including 50 photos, 50 voice cards and 50 document cards.
This establishes the fixture result, not a universal RAM ceiling for every
account and workload. Transient setup memory is reported separately.

| Measurement | Final result | Requirement |
| --- | ---: | ---: |
| 50-message initial paint, median | 27.25 ms | <40 ms |
| 500-message first-screen paint, median | 31.21 ms | <70 ms |
| Local search, median | 27.63 ms | <50 ms |
| Maximum loaded phase-end RSS | 193,871,872 bytes | <200,000,000 bytes |
| Process high-water RSS, including setup | 243,159,040 bytes | ≤250,000,000 bytes |
| Release executable, calls enabled | 39,954,296 bytes | <40,000,000 bytes |

Cold samples remain included: the 50-message first-paint maximum/p95 is
85.00 ms; the 500-message maximum/p95 is 32.66 ms. Search p95 is 29.29 ms
and maximum 32.06 ms. The user targets are medians, not per-operation deadlines.

The original search harness did not attach Shell's result-rendering callback.
The corrected harness verifies actual mapped result content; its result must
not be presented as an equivalent speedup over the old 183.85 ms figure.

### Responsiveness and capability review

- Message scrolling delivers 117.80 FPS with current effects, versus 119.57
  in the original audit. Sidebar scrolling improves from 113.78 to 120.06 FPS.
  These are medians of three runs on a private 120 Hz output at 1.6× scale,
  not the physical screen's measured FPS. The existing 100 FPS objective is
  retained; no acceptance threshold was lowered.
- Current-effects message-scrolling CPU falls from 59.92% to 21.10% of one
  logical core. Frame-work p99 increases from 8.28 to 12.31 ms on first visits
  to history whose controls have not been built. Cold message jumps increase
  from 29.72 to 33.40 ms median, and from 32.41 to 44.61 ms sample p95. These
  are real tradeoffs of deferring work; this is not a claim that every metric
  improved. A direct baseline/current/current/baseline comparison confirms
  roughly 2% lower message-scrolling FPS (120.00 → 117.50) and about 43% lower
  CPU (37.75% → 21.52%). The full comparison is in the linked audit.
- Startup, repeated chat opening, composer updates, local caches and media
  decoding were remeasured. The report retains all results, including small
  slower measurements and uncontrolled storage/timing variability. Background
  RSS increases from about 69 MB to 79 MB; idle CPU remains near zero.
- History, selection, focus, spoilers, translations and document download state
  survive control reclamation. Nearby photo pixels are restored at their
  original preview dimensions, with unchanged source files. Continuous visible
  content, render-tree pixel comparisons and fractional scanline checks pass.
- All 113 application tests, Clippy, the full 12-case GUI gate, debug/release
  history-state checks, and debug/release sticker-navigation checks pass.
  The latter exercises 24 returns across short and 500-message histories.
  An intermittent sticker-resume regression discovered during the gate was
  fixed and the complete gate rerun; no check was weakened.
- Calls remain enabled. The unchanged FFI and pinned native engine use existing
  ABI-checked system GLib/FFmpeg libraries. No additional application library
  is bundled. Three local create/list/stop cycles pass; real peer audio/video
  quality and live Telegram or AI-provider latency remain unmeasured.

### Evidence and reproduction

See [the full 0.1.8 validation report](performance-audit-0.1.8.md) for all
measurements, exact gate summary lines, build hash and interpretation limits.
Raw evidence is under
`target/responsiveness-goal/release-validation-final-018/`: 19 successful
sequential benchmark runs, the extended rendering run, complete validation
logs, source-input hashes and `validated-artifacts.json`. The source-input
hashes were checked unchanged after validation. Earlier candidate runs are
retained separately and do not establish this build's acceptance.

All GUI checks used private headless compositors. The existing desktop process
was not restarted. Its launcher points at the repository's rebuilt release
executable; fully quit and reopen the app, then check **About → 0.1.8** to use
this build.
