# Omarchygram build integration

This is `ntgcalls-sys` 3.0.0-rc01 from crates.io (original checksum
`a716760196dcb6af71fb0b2d63dc2bbd7e44a694dfe093b4701a403569945713`).
The FFI bindings are unchanged. The crate retains its LGPL-3.0-only license.

On Linux, `build.rs` resolves the pinned engine's public GLib and FFmpeg APIs
from the installed shared libraries before linking the original call-engine
archive. It requires GLib 2.88 or newer and FFmpeg ABI versions avformat 63,
avcodec 63, avutil 61, and swresample 7. A different FFmpeg ABI requires an
engine rebuild against matching headers; the build fails rather than guessing.
The pinned archive itself is unchanged and still comes from `bin/fetch-ntgcalls`.

`libomarchy_ntgcalls.so` in Cargo's build directory is a GNU linker script,
not a generated shared library. It places the system providers before the
archive despite Rust grouping static dependencies before dynamic dependencies.
It is consumed at link time and is neither installed nor needed at runtime.
OpenH264, Opus and WebRTC remain part of the pinned engine archive.

Changes from upstream: Linux link ordering and ABI checks in `build.rs`, plus
the `pkg-config` build dependency in `Cargo.toml`. Other platform linkage and
the explicit upstream `NTGCALLS_DYLIB` override retain their original behavior.

Verification: build the default feature set, inspect the executable's dynamic
dependencies, run `bin/headless target/release/examples/native_call_probe`,
and run the application gate. The native probe exercises local engine lifetime
without contacting Telegram or a call peer; it does not establish end-to-end
audio/video call quality.
