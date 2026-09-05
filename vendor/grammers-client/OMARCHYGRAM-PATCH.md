# Local download completion fix

This is the crates.io `grammers-client` 0.10.0 source package, upstream revision
`5c6d44ff30e02d6c9295bcf1fcb51403ad77c981`, under its original MIT/Apache-2.0
licenses. Only `src/client/files.rs` differs in executable code: both file
download paths await `file.flush()` before returning success.

Original crates.io package SHA-256:
`0f330139772e71b5e104f5a7bbf43bbda92fd8a734b4cf9c57839e04e949cf9b`.

Tokio `File::write_all()` can return with its blocking disk write still queued.
Dropping the file does not await that write. Omarchygram immediately validates
and atomically publishes downloaded files, so small images, avatars and voice
messages were consistently rejected as empty. Large transfers could publish
before the final chunk finished writing too.

The patch preserves the upstream parallel downloader, chunk sizes, migration
and authorization handling. It adds no dependency or alternate download code.
Remove this override once an upstream release includes both flushes.

Regression: `cargo test --lib small_download_is_fully_written_before_cache_publication`
uses the real downloader with embedded bytes and no network/session. It failed
on the unmodified package with `download returned an empty file; try again`.

The packaged Cargo.lock and registry installation marker were omitted; the
application's Cargo.lock remains authoritative. License texts were retrieved
from the upstream repository. Trailing whitespace in the examples README was
normalized for the repository's whitespace check.

References:

- https://codeberg.org/Lonami/grammers
- https://docs.rs/tokio/1.53.1/tokio/fs/struct.File.html
