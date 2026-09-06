# Showcase validation — 7 September 2026

The README showcase uses Omarchygram 0.1.8 with two small code changes: an
optional mock-only artwork directory, and notification registration checks
that prevent a delayed avatar lookup from sending a notification after app
shutdown. Incoming-call notification send/withdraw paths use the same guard.
The live-message recording exposed the shutdown assertion with
`G_DEBUG=fatal-criticals`; the corrected release recording completes cleanly.

## Checks

- All **113 tests pass** (86 library, 3 markup, 20 mock-backend, 4 theme).
- Clippy passes for all targets/features with warnings denied.
- Release and debug builds pass with calls enabled.
- The full 12-case GUI gate passes, always through `bin/headless`.
- Capture script compiles; README local links and `git diff --check` pass.
- Four 1220 × 921 screenshots were visually reviewed, including the light theme.
- GIF and MP4 dimensions, durations, hashes and sizes were checked with FFprobe.
  Encoded durations match capture timestamps within 0.1 seconds. Each GIF is
  under 1.2 MB; the three GIFs total about 3.0 MB.
- Sampled frames confirm all four live themes actually appear. Reducing the
  theme capture rate to 12 samples/second leaves GTK time to repaint between
  full-window snapshots; a 30-sample attempt retained stale colors under
  recording load. This is a capture setting, not an app performance change.

The initial sandboxed test attempt could not use the local HTTP socket and
media IPC. The complete suite was rerun successfully outside that sandbox,
with GUI access still isolated by `bin/headless`. No tests were skipped or
weakened. Captures and the GUI gate did not open windows on the user's desktop.

| Asset | Dimensions | Duration | Size |
| --- | --- | ---: | ---: |
| `speed.gif` | 1000 × 802 | 11.48 s | 0.76 MB |
| `speed.mp4` | 1000 × 802 | 11.48 s | 0.42 MB |
| `animations.gif` | 1000 × 802 | 12.00 s | 1.14 MB |
| `animations.mp4` | 1000 × 802 | 12.00 s | 0.46 MB |
| `themes.gif` | 1000 × 802 | 12.40 s | 1.10 MB |
| `themes.mp4` | 1000 × 802 | 12.40 s | 0.39 MB |

Screenshots and videos use offline dummy content. Numeric performance claims
come from the [separate 0.1.8 benchmark report](../../specs/performance-audit-0.1.8.md),
not the recording pipeline. These were not new performance benchmark runs.

## Build provenance

- Release executable: **39,954,744 bytes**.
- Release SHA-256: `76b1ba2627c5c7137fb50bde2fa916a2f9207262346676000d9dedad9dee5533`.
- Debug/gate SHA-256: `eda4e0b1b2b3953cca25be9ee9e144cccf6dde20a0b4186516a9735dff47be5f`.

Local raw evidence: `target/showcase-validation/` and
`target/readme-showcase/`. Reproduction settings and per-clip hashes are in
[the capture documentation](README.md) and the adjacent JSON manifests.

## Full GUI gate

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

- `README.md`: measured speed and footprint, new demos, feature overview, and accurate prebuilt/source version distinction.
- `bin/capture-showcase`: isolated offline captures, real-time encoding, private artwork mapping, and reproducible manifests.
- `src/tg/mock.rs`: optional `OMG_MOCK_IMAGE_DIR` artwork lookup; ordinary fixtures retain their fallback.
- `src/ui/shell.rs`: guard message and call notifications after GApplication unregisters.
- `docs/showcase/README.md`: recording method, requirements, timing limits, and artwork prompts.
- `docs/showcase/validation.md`: this validation report.
- `docs/showcase/animations.gif`: reviewed showcase asset or recording manifest.
- `docs/showcase/animations.json`: reviewed showcase asset or recording manifest.
- `docs/showcase/animations.mp4`: reviewed showcase asset or recording manifest.
- `docs/showcase/artwork/group.svg`: dummy artwork.
- `docs/showcase/artwork/marta.png`: dummy artwork.
- `docs/showcase/artwork/ridge.png`: dummy artwork.
- `docs/showcase/artwork/robin.svg`: dummy artwork.
- `docs/showcase/artwork/trail.svg`: dummy artwork.
- `docs/showcase/catppuccin.png`: reviewed showcase asset or recording manifest.
- `docs/showcase/gruvbox.png`: reviewed showcase asset or recording manifest.
- `docs/showcase/hero.png`: reviewed showcase asset or recording manifest.
- `docs/showcase/light.png`: reviewed showcase asset or recording manifest.
- `docs/showcase/speed.gif`: reviewed showcase asset or recording manifest.
- `docs/showcase/speed.json`: reviewed showcase asset or recording manifest.
- `docs/showcase/speed.mp4`: reviewed showcase asset or recording manifest.
- `docs/showcase/themes.gif`: reviewed showcase asset or recording manifest.
- `docs/showcase/themes.json`: reviewed showcase asset or recording manifest.
- `docs/showcase/themes.mp4`: reviewed showcase asset or recording manifest.
