# Sidebar photos and intermittent flashes — 0.1.7

The reported whole-window flashing has two configured causes: Screen flicker
dims the entire window for two frames every six seconds, and Error static
draws a short noise burst over the window on errors. Both were enabled in the
local settings. Both are now disabled there and excluded from all presets;
their individual controls and previews remain available. Normal error feedback
is preserved.

Shared-photo tiles previously measured their placeholder label, while the image
was an unmeasured overlay. The grid also expanded one or two photos across the
whole row. Tiles now request their height from their width, crop with Cover,
clip to a square, and reserve three columns even for incomplete rows. This adds
no frame callbacks, dependencies, image copies, or changes to full-size viewing.

The profile-photo button is centered independently of the profile title. Avatar
refreshes retain an existing photo until its replacement is ready; changing
peers or removing a photo clears it immediately. Existing generation checks
reject outdated download/decode completions.

## Files changed for this fix

- `src/ui/square.rs`: small GTK height-for-width container for square thumbnails.
- `src/ui/mod.rs`: register the square container module.
- `src/ui/info_panel.rs`: square three-column photo tiles, centered profile button, native geometry diagnostics.
- `src/ui/avatar.rs`: preserve a loaded photo across same-peer refreshes.
- `src/ui/anim/mod.rs`: make whole-window flashes individual opt-ins and explain them in settings.
- `src/ui/shell.rs`: native regression checks for profile/thumbnail sizing, avatar refresh and effect presets.
- `Cargo.toml`: version 0.1.7.
- `Cargo.lock`: update the package version without dependency changes.
- `README.md`: document the opt-in flash effects.
- `specs/sidebar-flashing.md`: findings, implementation, and validation evidence.

Local preference update: only `animations.flicker` and `animations.staticerror`
changed to false in the existing settings file; other preferences preserved.
Version 0.1.7 also includes the preceding performance and Assistant changes.

## Validation

- Iteration gate: normal 267-step and auth 270-step traversals passed without warnings or criticals.
- Unit/integration suite: 112 tests passed. The initial restricted run failed three existing tests because local mock HTTP, media decoding, and recording lacked permission; the unchanged suite passed with the required access under `bin/headless`.
- Native coverage checks square profile buttons, loaded shared-photo allocations at several pane widths, partial rows, same-peer refresh, peer changes, photo removal, stale completions, individual flash effects and cleanup. Existing checks still open full-size profile photos and media.
- Before screenshot: `target/shots/sidebar-before-0.1.6.png`.
- Iteration screenshots: `target/sidebar-quick-shots/sidebar-{profile,photos-280,photos-360,photos-440}.png`.
- Final debug and release builds passed; Clippy `--all-targets -- -D warnings` passed.
- Exact launcher release: all 268 native probe steps passed with no warnings or criticals. `target/sidebar-release-shots/about.png` confirms 0.1.7; profile, shared-photo, full-size viewer, Assistant and summary screenshots are in the same directory.
- Release binary: 56,782,880 bytes (20,800 bytes more than 0.1.6); SHA-256 `411ceacdc31343d6968a56c2a3a69467849d05c7fa0af7265e782e733f703ec1`.
- Final logs: `target/sidebar-{build-final,release,clippy-final,tests-final,release-probe}.log`.
- Full 12-case gate passed without warnings or criticals (`target/sidebar-gate-final.log` and `target/sidebar-gate-final/`).

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

All GUI checks use a private headless compositor and offline fixtures. The
user's desktop app is not launched or restarted. No live-account messages or
photos are needed.
