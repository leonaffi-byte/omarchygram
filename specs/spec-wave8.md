# Wave 8 — Optimization (faster, smoother, lighter, fewer deps)

Decided 2026-09-04 (user: "the application should work faster, smoother, and
consume fewer resources… rewrite [libraries] specifically for what we need… take
up less space… depend on as few other libraries as possible"). Runs AFTER Wave 7
merges. Baseline in `specs/spec-wave8-baseline.md`.

Rule for the whole wave: **measure before and after every change**, against the
baseline commands. A change that does not move a baseline number (or that trades
a real behavior for a marginal one) is reverted. No feature regressions: the
full `bin/gate` must stay green after each step, and `cargo build` +
`cargo build --no-default-features` stay clean. Ordered by value / risk.

## 8A — Release profile (biggest size + speed win, zero risk)

`[profile.release]`: `lto = "thin"` (try "fat"), `codegen-units = 1`,
`panic = "abort"` (drops unwind tables; the app already treats panics as fatal),
`strip = "symbols"`, `opt-level = 3` (try "s"/"z" for size and re-measure the
gate wall-clock). Add `[profile.release.package."*"] opt-level = 3` if a hot dep
needs it. Measure: release binary size (stripped and not), build time, gate
wall-clock. Expected: markedly smaller binary; possibly slower clean build.

## 8B — Thread + runtime footprint (biggest idle-resource win)

Baseline: 75–80 threads, two `tokio::runtime::Runtime::new()` (multi-thread,
16 workers each) for the backend and local services, plus GStreamer, the Lottie
render thread, the calls thread and GTK. A personal 1:1 client does not need 32
tokio workers.

- Cap both tokio runtimes with `worker_threads(2)` (or evaluate a
  current-thread runtime for local services, which mostly awaits subprocesses).
- Audit always-on threads: the Lottie render thread should exit when no sticker
  is animating (verify it parks/exits, per the 6D review), GStreamer players
  should fully release pipelines when idle (the retained-MediaFile pool is the
  known exception — leave it). Measure the idle thread count and RSS with a
  chat that has no animation open (the baseline was measured WITH an animating
  sticker — record a no-animation idle baseline too).
- Measure idle CPU on a static chat: it must be ~0, not 8 %. If it is not, find
  the always-scheduled timer (a progress tick, a clock, an animation source
  that did not stop) and gate it on visibility/need.

## 8C — Dependency diet (fewer libs, smaller, faster compile)

Audit each direct dependency; narrow features with `default-features = false`
and the minimal feature set; drop anything unused. Candidates (verify usage
first with a code-scout / grok pass, then narrow — never remove a used API):
- `image` (0.25): we only decode webp (stickers) and jpg/png (photos, avatars)
  and encode png (maps). Turn off default features and enable only `jpeg`,
  `png`, `webp`. Big code-size lever.
- `reqwest` + `rustls`: used for AI providers and OSM map tiles. Already
  `default-features = false` with rustls+json+multipart — confirm each feature
  is needed (multipart only for uploads?). Consider gating AI providers behind
  an `ai` cargo feature so a minimal build drops reqwest/rustls entirely
  (map tiles could then use a smaller client, or the feature stays on by
  default like `calls`). Decide by measuring the size delta.
- `chrono`: ensure `default-features = false` with only `clock`/`std` as used.
- `libsql`: already `default-features = false, features = ["core"]` — leave
  (bundled SQLite is required; rusqlite collides with grammers).
- `gstreamer`, `gtk4`, `thorvg`, `grammers`, `serde`, `toml`, `dirs`, `libc`,
  `flate2`, `async-channel`, `tokio`: check for unused features only.

Deliverable: a table (dep, features before/after, size/compile delta) in the
final Wave 8 report. Target: fewer crates than the 357 baseline and a smaller
binary, with the gate still green.

## 8D — "Rewrite for our needs" — only where it clearly wins

The user asked whether heavy libraries can be replaced with something simpler
built for exactly our need. Evaluate, but only adopt a rewrite that removes a
real dependency AND keeps the gate green AND is small to maintain:
- Map tile stitching already lives in `src/tg/real.rs::download_map` using
  `image` for RGBA compositing — the compositing itself is a handful of pixel
  copies we could do by hand, but we still need `image` to DECODE the OSM PNG
  tiles, so removing `image` here saves nothing. NOT worth it.
- A webp decoder or a PNG encoder from scratch is a large, bug-prone surface
  for little gain — REJECT unless `image` proves to be the dominant size cost
  and a tiny single-purpose crate covers it.
- Do NOT reimplement crypto, TLS, SQLite, the Lottie renderer, or WebRTC.
Conclusion up front: the honest wins are 8A–8C (profile, threads, feature
narrowing), not hand-rolled replacements. Any rewrite must be justified by a
measured size/speed number in the report.

## 8E — Runtime smoothness

- Confirm animations respect the master toggle and pause off-screen (waves 4/6
  already do; re-verify after the thread changes).
- Frame-clock / redraw audit: nothing should invalidate a widget every frame
  when idle. Grep for `queue_draw`/`add_tick_callback`/`timeout_add` in hot
  paths and ensure each stops when its subject is off-screen or unchanged.

## Process

1. 8A first (isolated commit, measure).
2. 8B (measure idle thread/RSS/CPU with and without animation).
3. 8C: a code-scout/grok audit produces the feature-narrowing list; apply per
   dep, `cargo build` + gate after each, measure.
4. 8D only if 8C flags a dominant cost with a cheap replacement.
5. Final report: the baseline table filled with after-numbers, and the
   dependency-count / binary-size / thread / idle-CPU deltas.
