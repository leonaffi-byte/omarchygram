# Media download completion and About — 0.1.1

The earlier media fixes passed mock UI checks but missed a failure in the real
download library. A headless diagnostic using the existing Telegram session
reproduced `download returned an empty file; try again` for three small avatars,
three full-size profile photos, a chat photo, and a voice message.

## Cause and correction

`grammers-client` 0.10.0 dropped its Tokio file immediately after `write_all`.
The final write could still be queued when Omarchygram checked and published
the file. Small downloads consequently looked empty; larger files could become
visible before their final write completed.

Both upstream file download paths now await `flush()` before returning. The
local Cargo patch contains the original 0.10.0 source and license texts, with
only these two executable-code additions. It preserves the parallel downloader,
chunk sizes, Telegram data-center migration and authentication, and existing
atomic cache publication. No additional runtime dependency was introduced.

See [the patch provenance](../vendor/grammers-client/OMARCHYGRAM-PATCH.md) and
[Tokio's file completion documentation](https://docs.rs/tokio/1.53.1/tokio/fs/struct.File.html).

## About

The main menu's About dialog displays **0.1.1** and a clickable **GitHub** link.
Both values come from Cargo package metadata, so the UI cannot retain a manually
copied old version. The native probe opens this dialog and checks the link
action without launching a browser. Version 0.1.1 identifies this source build;
this change does not publish a new package release.

## Verification

- The new regression runs the real SDK downloader and Omarchygram cache writer
  with embedded bytes, without opening a Telegram session. It failed against
  the original dependency, then passed after the patch. It checks immediate
  byte-for-byte completion, private permissions, and partial-file cleanup.
- The same real-account diagnostic passed after the patch: all eight downloads
  completed; avatars and images decoded using both Pixbuf and GDK Texture;
  GStreamer's `playbin3` decoded the voice message through end-of-stream.
- Live diagnostics ran through `bin/headless` after the app had quit. They did
  not send messages, mark messages read, display private images, or play audio
  through the user's speakers. Physical speaker output remains unverified.
- The About screenshot was captured and inspected in the headless compositor.

- `cargo test --offline --lib --tests`: **100 passed** (73 library, 3 config,
  20 mock backend, 4 theme).
- `cargo clippy --offline --all-targets -- -D warnings`: **passed**.
- `cargo check --offline --no-default-features --all-targets`: **passed**.
- Default debug and optimized release builds, including calling support:
  **passed**.
- Optimized executable: **56,501,440 bytes**, versus 56,491,648 previously:
  **+9,792 bytes (+0.0173%)**. Vendoring adds about 508 KB of dependency source
  and documentation to the repository, with no additional runtime library.

`GATE_LOGS=target/media-flush-gate OMG_PROBE_ARTIFACTS=target/media-flush-screenshots bin/gate`:
**PASS**. All twelve runs exited 0, with no GTK warnings or critical errors.

```text
ok   probe-1 (246 probe lines)
ok   probe-2 (246 probe lines)
ok   probe-3 (246 probe lines)
ok   probe-4 (246 probe lines)
ok   probe-5 (246 probe lines)
ok   probe-6 (246 probe lines)
ok   auth-1 (249 probe lines)
ok   auth-2 (249 probe lines)
ok   auth-3 (249 probe lines)
ok   latency (246 probe lines)
ok   seed-info (248 probe lines)
ok   seed-sidebar (247 probe lines)
gate: PASS
```

Local evidence: `target/media-probe-before.log`, `target/media-probe-after.log`,
`target/media-flush-regression-before.log`, `target/media-flush-regression-after.log`,
the `target/media-flush-*.log` build/check logs, and
`target/media-flush-screenshots/about.png`.

## Files

- `.gitignore` — track the patched SDK while keeping the large ntgcalls binaries ignored.
- `Cargo.toml` — version, repository metadata, and local SDK patch.
- `Cargo.lock` — lock the patched dependency and application version.
- `vendor/grammers-client/` — original SDK source and licenses; two download flushes and provenance note.
- `src/tg/real.rs` — regression through the actual download/cache path and patch explanation.
- `src/ui/shell.rs` — About dialog, link action check, and About screenshot capture.
- `examples/media_probe.rs` — bounded, headless, read-only real-media diagnostic with decoding checks.
- `README.md` — About and restart instructions.
- `specs/media-download-flush-fix.md` — reproduction, correction, and acceptance report.
