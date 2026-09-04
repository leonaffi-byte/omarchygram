# Wave 8 — Optimization report

Done 2026-09-04. Goal (user): "the application should work faster, smoother, and
consume fewer resources… take up less space… depend on as few other libraries as
possible." Every change was measured against `specs/spec-wave8-baseline.md` and
kept only if it moved a number without regressing the 12-run gate.

## Results (shipped default build, calls on)

| metric | baseline (pre-8A) | after Wave 8 | delta |
|---|---|---|---|
| release binary (shipped, stripped) | 73.9 MB | 53.0 MB | −28% |
| crates in dependency graph | 359 | 295 | −64 |
| idle threads (sidebar, no chat) | 64 | 22 | −66% |
| idle RSS | 87 MB | 85 MB | ~flat |
| idle CPU (30 s average) | 1.5% | 1.4% | ~flat (already low) |
| Media Lab threads (every media kind open) | 174 | 37 | −79% |
| Media Lab RSS | 302 MB | 93 MB | −69% |
| Media Lab CPU (headless cairo, software render) | 30.7% | 9.8% | −68% |

no-default-features build (calls off): 293 crates, 17.5 MB release binary — so the
vendored ntgcalls/WebRTC static lib accounts for ~35 MB of the 53 MB default
binary. Users who don't want voice calls can `cargo build --no-default-features`
for a 17.5 MB client.

The idle CPU was already ~1.5%, not the 8–13% the baseline saw — that figure was
measured with an animating sticker on screen; a static chat was already near-idle.
The Media Lab numbers are dominated by the per-row GStreamer pipeline pool (the
deliberate wave-6A retained-MediaFile leak) and are inflated further by headless
software rendering, so treat them as directional, not absolute; the idle row is
the solid resource number.

## What changed

**8A — release profile** (`Cargo.toml [profile.release]`, commit 048aedb).
`codegen-units = 1`, `opt-level = 3`, `strip = "symbols"`. Whole-crate optimization
plus a stripped binary. Shipped binary 73.9 → 60.8 MB.
- `lto` was tried and reverted: the thin-LTO link over the vendored 128 MB
  ntgcalls/WebRTC static lib is RAM-heavy and OOM-killed the linker repeatedly,
  for a ~1.5 MB win (the binary is dominated by that prebuilt blob, which LTO
  cannot touch).
- `panic = "abort"` was tried and reverted: in a release build it made GTK abort
  at startup on a `g_object_bind_property_full` GObject CRITICAL (fatal under the
  gate's `G_DEBUG=fatal-criticals`); the debug build and the default-profile
  release build are both clean. Not worth the small unwind-table saving.

**8B — tokio runtime footprint** (`src/tg/mod.rs`, `src/local/mod.rs`, commit 2a9fcdd).
Both runtimes defaulted to one worker per core (16 here). A personal 1:1 client is
network- and subprocess-bound, not CPU-bound. Capped both at `worker_threads(2)`:
idle threads 64 → 22, no throughput change (data commands still spawn freely;
blocking work uses tokio's separate blocking pool). The call actor was already a
single-thread current-thread runtime; the Lottie render thread is single.

**8C — dependency diet** (`Cargo.toml`, commit b4b2849). 359 → 295 crates, −5.3 MB.
- `image`: `default-features = false`, only `jpeg`/`png`/`webp`. The crate only
  decodes webp stickers + png OSM tiles and encodes png; photos/avatars go through
  GdkPixbuf, not this crate. Dropped gif/tiff/bmp/ico/tga/dds/exr/pnm/hdr/qoi.
- `chrono`: `default-features = false`, `clock` only. No chrono serde anywhere;
  dropped wasm-bindgen/js-sys/tzdata.
- Everything else (`reqwest`, `tokio`, `serde`, `toml`, `dirs`, `libc`,
  `async-channel`, `flate2`, `libsql`) audited and left as-is — already minimal or
  every feature load-bearing.

**8D — hand-rolled rewrites**: none. As the spec predicted, the honest wins are
8A–8C. Map compositing still needs `image` to decode the PNG tiles, so replacing
it saves nothing; a from-scratch webp/png codec or WebRTC/crypto/SQLite rewrite is
a large bug surface for no real gain. Rejected.

**8E — runtime smoothness**: audited (read-only), no code change. Every repeating
timer and animation in `src/ui/` was classified. 30+ sources gate correctly —
they `Break` when their subject is off-screen, unmapped, finished, or the master
animations toggle is off (the animation system in `src/ui/anim/mod.rs` centralizes
`!is_mapped()` checks; media/story/call/live-location timers all stop when their
pipeline/list/timer empties). The only always-on work when the app sits idle on a
static chat:
- two per-frame frame-clock ticks — `layout_tick` (`src/ui/shell.rs:1924`) and
  `pane_width_tick` (`src/ui/messages.rs:1145`) — that do a width/state comparison
  and early-return without any `queue_draw`/`queue_resize` when nothing changed.
  No idle redraws, but they keep the frame clock awake at ~60 fps.
- one 1-second timer updating the header clock label (`src/ui/shell.rs:1859`).

Converting the two ticks to event-driven (resize/notify signals) would let the
frame clock sleep and shave the ~1.4% idle CPU toward zero. Rejected for now:
they were made tick-driven on purpose because signal-based resize is unreliable
under the Wayland compositor (the probe itself can only inject a width, not
resize), so the gate cannot verify interactive resize — the exact behavior this
refactor would put at risk. A ~1% idle-CPU gain does not justify a change the
automated gate cannot cover. Revisit if laptop battery drain is ever measured.

## Verification

Full 12-run HARD gate green after each step; `cargo test` (70 tests) green;
`cargo build` and `cargo build --no-default-features` both warning-free. The 8A
release binary was additionally validated with 5 probe runs against
`target/release` (3 default + incoming-call + auth), exit 0, zero GTK criticals,
because the debug gate does not exercise the release profile.
