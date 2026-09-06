# Omarchygram 0.1.8 performance validation

Measured on the local Omarchy machine using private headless compositors, offline mock chats, and synthetic media. These are local UI and service measurements, not Telegram network response times or the physical display's FPS.

Machine and protocol: [the baseline environment](performance-audit-0.1.7.md#test-environment-and-interpretation), including the Core Ultra 9 285H, Intel ARL GPU, GTK 4.22.4 and Vulkan renderer. System power settings were unchanged.

The RAM requirement is below 200 MB while loaded. On 2026-09-06 the user accepted brief loading peaks up to 250 MB. MB below means decimal bytes; the original audit also reports MiB. Latency targets use medians, with cold samples and tails retained.

| Target | Baseline median | 0.1.8 median | Sample p95 / maximum | Required |
| --- | ---: | ---: | ---: | ---: |
| 50-message first paint | 131.42 ms | 27.25 ms | 85.00 / 85.00 ms | <40 ms |
| 500-message first screen | 1283.33 ms | 31.21 ms | 32.66 / 32.66 ms | <70 ms |
| Local search across 1,000 chats | Not equivalent | 27.63 ms | 29.29 / 32.06 ms | <50 ms |

Release executable: **39,954,296 bytes**, down from 56,782,880 bytes, with calls enabled. No extra application shared library is bundled. The build reuses ABI-checked system GLib/FFmpeg libraries; see [build requirements](../vendor/ntgcalls-sys/README.omarchy.md).

## What changed

- Message and chat lists construct full controls around the visible area. Offscreen text and inactive document controls can be reclaimed while retaining history, selections, spoilers, translations, download state and keyboard focus. Two recent jump destinations remain ready.
- Photos retain their original files and display quality. Distant decoded textures are released; an 8 MiB cache and nearby restoration avoid keeping every full image in RAM. Late offscreen download completions are reclaimed immediately.
- Local search renders immediately and recycles broad result lists. Remote Telegram search keeps its separate 300 ms debounce.
- Calls retain the pinned native engine and unchanged FFI. Linking against existing system libraries and packing relative relocations removes duplicate executable code.
- Jump preparation waits for GTK to allocate changed row geometry before prefetching from those bounds. A settled-viewport media check resumes visible animated stickers even if an earlier coalesced callback ran before layout.

## Validation and interpretation

All 113 application tests pass. Clippy passes with all targets/features and warnings denied. The full 12-case GUI gate passes: six normal traversals, three auth traversals, delayed responses, and both restored-layout seeds. The dedicated history-state checks cover focus, selection, spoilers, translations, edits and document download state; sidebar checks include 1,000 chats, broad search navigation, compact/collapsed modes and larger fonts.

Pixel checks compare full and viewport-limited render trees from the same frame; scanline checks compare cached and original drawing at fractional scales. Separate continuous-content checks require actual mapped message text and resident visible photos. The sticker navigation check exercises 24 offscreen-to-onscreen returns across short and 500-message histories, including changed wrapping. The local native call check completes three create/list/stop cycles; it does not establish real peer call quality.

The original search harness omitted Shell's result-rendering callback. The new harness attaches it and checks mapped result content, so the baseline 183.85 ms is not an equivalent search workload. Other fixtures retain the original comparison protocol. Startup and UI use a private 60 Hz output; scrolling uses 120 Hz, 1.6× scale, 1,000 chats and 500 group messages including 50 photos, 50 voice cards and 50 document cards. Video is not playing during scrolling.

CPU 100% means one logical CPU. Runs are sequential without simultaneous builds or GUI gates; unrelated desktop activity is not globally controlled. UI latency ends at GTK after-paint. Small samples are not stable estimates of long-run tails. Media fixtures use silent/fake sinks and measure decoding, not audible latency or A/V synchronization.

## Latency and local services

| Measurement | Baseline median, ms | 0.1.8 median, ms | 0.1.8 p95, ms | 0.1.8 max, ms | Samples |
| --- | ---: | ---: | ---: | ---: | ---: |
| history cache 50 write flush | 1.87 | 2.31 | 3.64 | 6.64 | 30 |
| history cache 50 read os warm | 0.078 | 0.070 | 0.289 | 0.473 | 30 |
| history cache miss | 0.014 | 0.013 | 0.024 | 0.190 | 30 |
| archive new database open | 12.32 | 12.92 | 12.92 | 12.92 | 1 |
| archive 1000 message batch and barrier | 41.89 | 26.51 | 28.77 | 28.77 | 10 |
| archive version lookup 10000 rows | 0.040 | 0.025 | 0.173 | 0.173 | 10 |
| archive mark 100 deleted | 160.89 | 149.94 | 149.94 | 149.94 | 1 |
| archive query 100 deleted | 0.225 | 0.304 | 0.475 | 0.575 | 30 |
| photo 12mp to 720px uncached decode | 72.65 | 65.25 | 128.78 | 128.78 | 15 |
| photo 720px memory cache hit | 0.0019 | 0.0015 | 0.0068 | 0.0068 | 15 |
| photo 12mp full texture decode | 27.57 | 22.15 | 23.82 | 23.82 | 15 |
| voice 30s opus first decoded buffer | 4.66 | 3.65 | 7.04 | 7.04 | 10 |
| voice 30s opus full decode unpaced | 42.62 | 41.04 | 42.92 | 42.92 | 10 |
| video 10s h264 720p30 first decoded buffer | 18.46 | 16.28 | 37.27 | 37.27 | 10 |
| video 10s h264 720p30 full decode unpaced | 90.75 | 91.33 | 108.73 | 108.73 | 10 |
| sticker 64px parse | 0.129 | 0.122 | 0.122 | 0.122 | 1 |
| sticker 64px frame raster | 0.0082 | 0.0085 | 0.011 | 0.040 | 120 |
| sticker 192px parse | 0.037 | 0.044 | 0.044 | 0.044 | 1 |
| sticker 192px frame raster | 0.020 | 0.021 | 0.029 | 0.032 | 120 |
| sticker 384px parse | 0.062 | 0.072 | 0.072 | 0.072 | 1 |
| sticker 384px frame raster | 0.046 | 0.046 | 0.060 | 0.087 | 120 |
| markup 2250 chars two spans sync | 0.0092 | 0.0100 | 0.011 | 0.012 | 30 |
| summary 2000 messages prepare sync | 1.30 | 1.29 | 1.68 | 2.43 | 30 |
| chat list 1000 first build to frame | 354.06 | 85.43 | 85.43 | 85.43 | 1 |
| chat list 1000 unchanged snapshot to frame | 15.76 | 16.09 | 18.09 | 18.09 | 4 |
| history 50 widget build sync | 7.06 | 13.28 | 16.56 | 16.56 | 7 |
| history 50 initial to frame | 131.42 | 27.25 | 85.00 | 85.00 | 7 |
| history 50 cached to frame | 131.84 | 26.14 | 27.26 | 27.26 | 7 |
| history 50 unchanged refresh to frame | 14.21 | 11.54 | 14.46 | 14.46 | 7 |
| history 500 widget build sync | 71.77 | 17.45 | 17.86 | 17.86 | 7 |
| history 500 initial to frame | 1283.33 | 31.21 | 32.66 | 32.66 | 7 |
| history 500 cached to frame | 1288.98 | 32.31 | 33.19 | 33.19 | 7 |
| history 500 unchanged refresh to frame | 22.22 | 11.45 | 11.70 | 11.70 | 7 |
| unchanged sidebar row update sync | 0.811 | 0.431 | 0.713 | 0.775 | 30 |
| incoming sidebar update to frame | 22.22 | 16.00 | 20.95 | 42.57 | 30 |
| local chat search 1000 to frame | — | 27.63 | 29.29 | 32.06 | 30 |
| composer update to frame | 16.01 | 11.04 | 13.18 | 13.79 | 30 |
| message jump to frame | 29.72 | 33.40 | 44.61 | 51.86 | 30 |
| broad chat search 1000 matches to frame | — | 17.91 | 28.57 | 34.56 | 20 |
| startup shell construct from main | 74.62 | 59.82 | 74.55 | 74.55 | 5 |
| startup mock chat list painted from main | 724.14 | 648.70 | 654.86 | 654.86 | 5 |
| shell mock first chat open to messages painted | 178.88 | 159.30 | 230.84 | 230.84 | 10 |
| shell mock reopen chat to messages painted | 33.30 | 32.80 | 76.79 | 77.67 | 30 |
| contacts dialog open to frame | 34.51 | 36.16 | 36.21 | 36.21 | 5 |

Cache reads are OS-warm. Storage services use private Btrfs fixtures; mock UI/startup data use temporary state. The baseline pooled three service runs; this validation has one fresh service run with repeated samples. The old archive-deletion stall was an unexplained outlier, not a verified defect fixed by these UI changes.

## Scrolling

FPS and CPU are medians across three runs. Tail columns show the worst per-run p99 GTK frame-work duration. First visits to unbuilt history require text shaping and have longer tails than the prebuilt baseline; these differences are reported rather than described as unchanged performance.

| Effects / phase | Baseline FPS | 0.1.8 FPS | Baseline CPU % | 0.1.8 CPU % | Baseline p99 work, ms | 0.1.8 p99 work, ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| none / sidebar scroll | 115.30 | 119.90 | 47.54 | 14.42 | 11.68 | 6.77 |
| none / message scroll | 119.70 | 117.90 | 47.80 | 14.63 | 6.35 | 11.59 |
| none / message scroll with info | 119.70 | 118.10 | 45.82 | 15.13 | 6.76 | 11.77 |
| current / sidebar scroll | 113.78 | 120.06 | 62.45 | 17.81 | 12.32 | 3.33 |
| current / message scroll | 119.57 | 117.80 | 59.92 | 21.10 | 8.28 | 12.31 |
| current / message scroll with info | 118.18 | 117.90 | 65.87 | 23.93 | 8.94 | 12.23 |
| all / sidebar scroll | 114.04 | 120.00 | 60.59 | 17.52 | 12.48 | 6.30 |
| all / message scroll | 119.60 | 117.80 | 61.80 | 20.45 | 8.06 | 11.62 |
| all / message scroll with info | 117.99 | 117.80 | 69.00 | 23.99 | 9.20 | 12.36 |

### Direct comparison with the preserved baseline binary

After the final suite, four additional runs used the verified original frame
probe and final frame probe in baseline/current/current/baseline order. Both
used identical current-effects settings and 10 seconds per phase; pixel
verification was disabled for both timing runs, with its separate validation
already passing. All four runs passed. Values below are medians of two runs;
p99 is the worse per-run value.

| Phase | Baseline FPS | 0.1.8 FPS | Baseline CPU % | 0.1.8 CPU % | Baseline p99 work, ms | 0.1.8 p99 work, ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Sidebar scroll | 118.08 | 118.89 | 40.42 | 22.34 | 8.72 | 8.99 |
| Message scroll | 120.00 | 117.50 | 37.75 | 21.52 | 4.68 | 10.44 |
| Message scroll with info | 119.99 | 117.85 | 44.24 | 25.27 | 7.38 | 12.23 |

This confirms a roughly 2% message-scrolling FPS reduction and longer cold
frame-work tails, alongside about 43% lower message-scrolling CPU. The new
build remains above the existing 100 FPS objective, but it is not a zero-cost
change to scrolling. Original-binary CPU differs from the earlier audit, so
the larger historical CPU difference must not all be attributed to the code.
Logs, hashes and results are in `release-validation-final-018/paired-frames/`
under the artifact root. The original frame-probe SHA-256 is
`39237f179c277e83ea11b2d0b06873da5dd017dada9a0629ac17417c71a1e0ff`;
the final probe is
`bbcb37b701c54c5ce23353766409c340ef42b969a20864803850da04a53d3dc1`.

## Memory and background operation

Extended run: 60 seconds per phase, with current effects. Process high-water RSS includes initial fixture construction and renderer setup.

| Phase | End RSS, MB | Process high-water RSS, MB | Presented FPS |
| --- | ---: | ---: | ---: |
| idle | 166.26 | 243.16 | 119.52 |
| sidebar scroll | 173.31 | 243.16 | 119.55 |
| message scroll | 193.02 | 243.16 | 118.03 |
| message scroll with info | 193.87 | 243.16 | 118.58 |

| Small mock session | Baseline max RSS, MB | 0.1.8 max RSS, MB | Baseline CPU % | 0.1.8 CPU % |
| --- | ---: | ---: | ---: | ---: |
| none / background idle | 69.17 | 78.41 | 0.000 | 0.000 |
| none / visible idle | 111.81 | 112.82 | 0.300 | 0.499 |
| current / background idle | 69.45 | 78.73 | 0.000 | 0.033 |
| current / visible idle | 115.78 | 116.30 | 4.625 | 4.760 |

These small idle-session figures are separate from the populated scrolling workload. A zero measured CPU delta means below counter resolution over the sampling interval, not proof of no CPU work.

## Reproduction and provenance

Build with the default features and follow the benchmark commands in the README. All GUI commands must use `bin/headless`. Validation logs and raw data are under `target/responsiveness-goal/release-validation-final-018/`; the complete benchmark manifest has 19 successful sequential runs. `validated-artifacts.json` records the debug and release hashes checked by the gate and benchmarks.

Release SHA-256: `52a5e3bfa2b7bc4d27e1951d6e98f7fec9ba9e820798a8bcc3fcfcd77c021c9a`. The benchmark provenance records base commit `0ddbe716d4b3d3997c837c2201f9fa032106a9f3` plus version 0.1.8; changes were measured before committing the release. The original 0.1.7 raw audit and verified baseline executable remain preserved.

Full GUI gate output:

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

## Files changed

- `.gitignore`: include the small patched native binding crate; keep native archives ignored.
- `Cargo.toml`: version 0.1.8, native binding patch, and call-probe feature requirement.
- `Cargo.lock`: lock the version and patched native binding dependency.
- `build.rs`: compact release ELF relative relocations on x86-64 GNU/Linux.
- `README.md`: explain bounded rendering, benchmarks, results and runtime requirements.
- `packaging/PKGBUILD`: declare the GLib/glibc minimum runtime versions.
- `src/theme/style.css`: preserve search-result styling with recycled GTK rows.
- `src/ui/chatlist.rs`: build nearby sidebar controls and recycle broad search results.
- `src/ui/messages.rs`: defer/reclaim controls, preserve state, restore nearby photos and settle media visibility.
- `src/ui/media_image.rs`: bound decoded previews and test exact small-image restoration.
- `src/ui/shell.rs`: include scroll and animation state in sticker-resume failure diagnostics.
- `examples/frame_perf_probe.rs`: record resources and verify actual visible message content.
- `examples/performance_audit.rs`: measure local services/UI and check deferred-history/navigation behavior.
- `examples/perf_support/mod.rs`: share timing statistics and process resource counters.
- `examples/native_call_probe.rs`: check local native call-engine lifecycle with calls enabled.
- `examples/fixtures/benchmark-current-effects.toml`: reproducible animation settings.
- `bin/measure-process`: isolate and measure background/visible release-process lifecycle.
- `bin/run-performance-audit`: run 19 benchmarks sequentially and record provenance.
- `bin/summarize-performance-audit`: regenerate only the preserved historical 0.1.7 audit.
- `specs/performance-audit-0.1.7.md`: preserve baseline measurements and correct the search comparison.
- `specs/performance-audit-0.1.8.md`: report final measurements, validation and tradeoffs.
- `specs/responsiveness-targets.md`: record user targets, the accepted brief RAM peak and acceptance evidence.
- `vendor/ntgcalls-sys/Cargo.toml`: add the system-library discovery build dependency.
- `vendor/ntgcalls-sys/Cargo.toml.orig`: preserve the upstream source manifest.
- `vendor/ntgcalls-sys/build.rs`: resolve compatible system libraries before the pinned native archive.
- `vendor/ntgcalls-sys/src/lib.rs`: preserve upstream FFI bindings unchanged.
- `vendor/ntgcalls-sys/README.md`: preserve the upstream crate documentation.
- `vendor/ntgcalls-sys/README.omarchy.md`: document link ordering, ABI requirements and verification limits.
