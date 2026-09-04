# Wave 8 baseline (measured 2026-09-04 on the wave-6 tree, commit 8623604)

Machine: 16-core Arch, Rust 1.98, GTK 4.22. Measurements are the reference
every Wave 8 change is compared against (same commands, same fixture).

| metric | value | how |
|---|---|---|
| crates in the dependency graph | 357 | `cargo tree -e normal --prefix none \| sort -u \| wc -l` |
| direct dependencies | 21 | Cargo.toml |
| release binary, default profile | 35.6 MB (27.9 MB stripped) | `cargo build --release`, `strip` |
| release build time (clean deps cached) | 2 m 25 s | idem |
| debug binary | 404 MB | `target/debug/omarchygram` |
| resident memory, smoke mode, "Media Lab" open (players + animating sticker) | 99 MB (VmHWM 101 MB) | `ps -o rss` at t = 5…30 s |
| threads | 75–80 | `ps -o nlwp` |
| CPU, lifetime average over the first 30 s of that screen | 8–13 % of one core | `ps -o pcpu` |
| probe traversal (212 steps) | ~50 s per run | `bin/gate` logs |

Known contributors (to verify by profiling before changing anything):
- Two tokio multi-thread runtimes (backend, local services) at the default 16
  workers each, plus GStreamer, the Lottie render thread and GTK's own threads
  → the thread count.
- `[profile.release]` is untouched: no LTO, 16 codegen units, no strip,
  panic = unwind.
- Heavy crates: reqwest + rustls (AI providers, map tiles), image (webp → png,
  map stitching), libsql (archive), thorvg (vendored C++), gstreamer.
