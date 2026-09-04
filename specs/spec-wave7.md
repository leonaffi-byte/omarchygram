# Wave 7 — Voice calls (video calls as a follow-up spike)

Decided 2026-09-04 (orchestrator; the user accepted the ntgcalls/LGPL trade-off
by ordering the wave). Research: the ntgcalls Rust crate (`ntgcalls = "=3.0.0-rc01"`,
LGPL-3.0) wraps a prebuilt 128 MB static library that does the DH key exchange,
the MTProto-style signaling encryption, WebRTC transport and audio capture/
playback (PulseAudio through PipeWire's compat layer, ALSA fallback) itself.
Our job is only: the Telegram signaling (grammers `phone.*`), a call state
machine that drives ntgcalls, and the UI.

This spec is the design authority; the §0 rules of specs/spec-wave6.md apply
verbatim (touch only listed files, headless gate must pass, no launchers from
probe code, colors from theme tokens only, do not commit).

Scope: 1:1 VOICE calls end to end (place, receive, decline, hang up, mute,
the 4-emoji verification, in-call timer, device pick in settings). Video calls
are explicitly OUT — a later spike; the backend contract leaves room for them
(`video` fields default false) but no video path is built.

---

## 1. Backend contract (orchestrator implements; UI compiles against this)

Everything in §1 EXISTS on `main` when the UI package starts. The mock backend
drives the whole call flow so the UI and the probe work WITHOUT ntgcalls; the
real ntgcalls integration is `#[cfg(feature = "calls")]` inside `src/tg/`.

### 1.1 Build

- Cargo feature `calls` (DEFAULT ON). With it, `dep:ntgcalls` is linked. Without
  it (`--no-default-features`), the app builds with no call library and the real
  backend's `call_*` commands return `Err("voice calls are not built in")`; the
  UI, the mock call flow and the probe are unaffected (they never touch ntgcalls).
- The 128 MB static lib is NOT in git. `bin/fetch-ntgcalls` downloads the pinned
  release asset (`v3.0.0-rc01`, linux-x86_64 static, sha256 pinned in the script)
  into `vendor/ntgcalls/lib/libntgcalls.a` (git-ignored); `.cargo/config.toml`
  sets `NTGCALLS_LIB_DIR` to it so the build never fetches anything itself.
  Binary cost measured 2026-09-04: debug 54 MB (was 40 MB), the lib is
  self-contained and dlopens libpulse at runtime (no new ldd entries).
- README gets a NOTICE: ntgcalls is LGPL-3.0; the app is source-available on
  GitHub, satisfying the relink obligation.

### 1.2 Types (`src/tg/mod.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    /// Outgoing: request sent, waiting for the other side to pick up (ringing there).
    Requesting,
    /// Incoming: the other side is calling; we are ringing, not yet accepted.
    Incoming,
    /// Keys are being exchanged (both directions) after accept/confirm.
    Exchanging,
    /// connect_p2p is running; media not yet flowing.
    Connecting,
    /// Connected: audio is flowing.
    Active,
    /// Over. `CallInfo::end_reason` says why.
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEndReason {
    /// One side hung up a connected call.
    Hangup,
    /// Not answered in time.
    Missed,
    /// The callee was busy / declined.
    Declined,
    /// A transport or crypto failure (details logged, not shown raw).
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallInfo {
    /// Telegram phone-call id; 0 before the server assigns one (outgoing pre-request).
    pub id: i64,
    /// The other party (Bot-API user id).
    pub peer_id: i64,
    pub peer_name: String,
    /// True when we placed the call.
    pub outgoing: bool,
    pub phase: CallPhase,
    pub muted: bool,
    /// The 4-emoji key verification, e.g. "🐴🍎🚗🌍". Empty until `Active`.
    pub emojis: String,
    /// When `Active` began (for the in-call timer). None before that.
    pub connected_at: Option<DateTime<Local>>,
    /// Set only in `Ended`.
    pub end_reason: Option<CallEndReason>,
}
```

There is at most ONE call at a time (Telegram 1:1). Placing or accepting a call
while one is active is refused with an error.

### 1.3 Tg methods (`src/tg/mod.rs`)

```rust
/// Place a 1:1 voice call to a user (Bot-API id). Fails if a call is active,
/// the peer is not a user, or calls are not built in. The call's progress
/// arrives as Event::CallChanged.
pub async fn call_start(&self, user_id: i64) -> Result<(), TgError>;
/// Accept the current incoming call.
pub async fn call_accept(&self) -> Result<(), TgError>;
/// Decline an incoming call, or hang up an outgoing/active one. Idempotent.
pub async fn call_hang_up(&self) -> Result<(), TgError>;
/// Mute/unmute the microphone on the active call.
pub async fn call_set_muted(&self, muted: bool) -> Result<(), TgError>;
/// Audio devices for the settings page: (id, human name). `id` goes back in
/// set_call_devices verbatim; the first entry of each is the system default.
pub async fn call_devices(&self) -> Result<CallDevices, TgError>;

pub struct CallDevices {
    pub input: Vec<CallDevice>,   // microphones
    pub output: Vec<CallDevice>,  // speakers
}
pub struct CallDevice { pub id: String, pub name: String }
```

Device selection reads `settings.calls.input_device` / `output_device`
(empty = system default) at call start; no separate command is needed.

### 1.4 Events (`src/tg/mod.rs`)

```rust
/// The one active/ringing call changed (new incoming, phase advanced, ended).
/// The UI keeps a single call surface and updates it; on `Ended` it shows the
/// end state briefly then clears.
CallChanged(CallInfo),
```

An incoming call arrives as `CallChanged(CallInfo { phase: Incoming, .. })`.
After `Ended` the backend sends no further events for that call.

### 1.5 Settings (`src/settings.rs`, hot-reloaded)

```toml
[calls]
input_device = ""      # "" = system default; else a CallDevice.id
output_device = ""
ringtone = true        # play a ringtone on incoming calls (mock: a beep via the player)
```

### 1.6 Real backend — a single-owner CallManager actor (`src/tg/calls.rs`, feature `calls`)

Hardened after the spec review (2026-09-04). A call is a concurrency hazard:
RPC completions, raw `updatePhoneCall*`, ntgcalls C++ callbacks and timers all
arrive independently. **Everything funnels through ONE tokio task** (the
`CallManager`) that owns all call state; nothing else touches it. Binding design:

- **One actor, one channel.** The backend spawns a `CallManager` task at
  connect. It owns a `CallState` (Idle / Outgoing / Incoming / Exchanging /
  Connecting / Active / Ending) plus a monotonic `generation: u64`. Its inbox is
  one mpsc of messages: a UI command + reply, a raw call update, a
  `(gen, signaling bytes)` from the C++ callback, a `(gen, ConnectionState)`, a
  `(gen, timer)`. ONLY this task mutates state or emits `Event::CallChanged`.
- **Synchronous slot reservation.** `call_start`/`call_accept` are accepted only
  in Idle; the actor bumps `generation` and moves to Outgoing/Exchanging BEFORE
  any await, then does the RPCs. Start/accept in any non-Idle state returns
  `Err`. "One call at a time" is enforced by the actor being single-threaded.
- **Idempotent, generation-keyed reducer.** A `PhoneCall` object (RPC-returned
  OR from an update) is applied by one function that ignores anything whose
  call id != the current call and anything for a stale generation; applying the
  same phase twice is a no-op, so duplicate/overtaking updates are safe.
- **Stale completions only compensate.** Every awaited RPC captures `generation`
  before awaiting; if it changed by the time it returns, the result is dropped
  (and if it created a server-side call, a compensating `phone.discardCall` is
  sent) — it never advances state or emits UI.
- **Signaling bridge.** `on_signaling_data`/`on_connection_change` are registered
  BEFORE `connect_p2p`. C++ callbacks only `try_send((gen, bytes))` into the
  actor's bounded channel (never touch state); the actor drains them into
  `phone.sendSignalingData`. Inbound `updatePhoneCallSignalingData` for the
  current call is fed to `ntg.send_signaling_data`, bounded-buffered until the
  instance is ready, dropped for a stale/ended call.
- **Incoming collision.** A `phoneCallRequested` while the slot is not Idle is
  answered `phone.discardCall(reason: Busy)` for that new id and does NOT
  replace the current call. Updates whose id is not the tracked call are ignored.
- **Teardown is atomic and single-Ended.** Any of {local hang up, remote
  `phoneCallDiscarded`, a timeout, a Connection Failed/Timeout/Closed, an
  ntgcalls/crypto error} transitions to Ending exactly once (a second trigger is
  a no-op), then: cancel timers, detach ntgcalls callbacks, stop media +
  `ntg.stop`, send ONE `phone.discardCall` (only for a LOCAL hang up / failure;
  a remote discard sends none), drop the per-call ntgcalls handle, emit exactly
  one `CallChanged(Ended)`, return to Idle. A bounded terminal tombstone
  (last call id + gen) makes late updates/callbacks/timers drop silently.
- **Logout / shutdown.** Log-out and command-channel close first tell the
  CallManager to end any call (atomic Ending, await the stop), THEN sign out /
  delete the session; the call session generation is invalidated so no
  old-session update or callback reaches a later login. `NTgCalls` is dropped on
  backend shutdown.
- **Mandatory key verification.** BOTH roles pass the fingerprint from the
  `phoneCall` object to `exchange_keys`; ntgcalls verifies it against the derived
  auth key. Any `CryptoError` (or a g_a_hash mismatch on the callee) discards the
  call, drops key material without logging it, and ends the call Failed — it is
  NEVER connected or shown Active unverified.
- **Timeouts (generation-scoped monotonic timers).** Unanswered Outgoing (ring)
  and Incoming after the call-config timeout -> `Ended(Missed)`; a setup/connect
  timeout after accept/confirm -> `Ended(Failed)`. Use the call-config values
  with documented fallbacks (ring 60 s, connect 30 s).
- **Signaling protocol version.** Send the `protocol` from
  `NTgCalls::get_protocol()` verbatim in requestCall/acceptCall/confirmCall.
- **Security / logging.** Never log DH values, the auth key, the fingerprint,
  raw signaling bytes, or device ids. A persisted device id absent from the
  current enumeration falls back visibly to the system default.

Signaling maps to ntgcalls (research 2026-09-04): outgoing = getDhConfig ->
create_p2p_call + init_exchange (g_a_hash) -> requestCall -> on phoneCallAccepted
exchange_keys(g_b) -> confirmCall -> on phoneCall connect_p2p ->
set_stream_sources. Incoming = phoneCallRequested -> create_p2p_call +
init_exchange(g_a_hash) -> receivedCall (ring) -> on accept acceptCall(g_b) ->
on phoneCall exchange_keys(g_a, fingerprint) -> connect_p2p -> set_stream_sources.
The `phone.*` TL functions and `updatePhoneCall*` are in grammers-tl-types 0.10;
`consume_updates`/`event_from_raw` route them into the CallManager.

Without the `calls` feature this module is a stub whose command entry points
return `Err("voice calls are not built in")` and whose update hooks are no-ops.

### 1.7 Mock backend (`src/tg/mock.rs`) — drives the UI and the probe

- `call_start(user_id)`: emits `CallChanged(Requesting)` at once, then after
  ~700 ms `Exchanging`, ~700 ms `Connecting`, ~700 ms `Active` with
  `emojis: "🐴🍎🚗🌍"` and `connected_at = now`. `call_hang_up` → `Ended(Hangup)`.
  `call_set_muted` flips `muted` and re-emits. Calling a bot user id
  ("Omarchy Bot") is refused with an error, like Telegram.
- An INCOMING call fixture: env hook `OMG_MOCK_INCOMING_CALL=<seconds>` (default
  off) makes the mock emit `CallChanged(Incoming, peer = Marta)` that many
  seconds after start; `OMG_MOCK_INCOMING_CALL=0` fires it immediately (probe
  uses this). `call_accept` advances Incoming → Exchanging → … → Active;
  `call_hang_up` on Incoming → `Ended(Declined)`.
- `call_devices` returns two mock microphones and two speakers.
- The mock uses the SAME state-machine shape: one call slot, a `call_gen`
  bumped on every start/accept/hang-up, and a generation-checked cancellable
  advance task that never emits `Active` after a hang up or a newer call; an
  incoming request while a call is active is refused (busy); calling a bot is
  refused. `OMG_MOCK_FAIL_ONCE` names: `CallStart`, `CallAccept`.
- Adversarial cases the probe exercises: hang up during `Requesting` (no later
  `Active`); decline an incoming call; a second `call_start` while active errors.

### 1.8 Probe steps (added by the UI package to `run_probe`)

`call button present`, `call outgoing ringing`, `call outgoing connects`,
`call mute`, `call hang up`, `call incoming rings`, `call incoming accept`,
`call incoming decline`. Under the probe the ringtone is silent (fakesink, like
the players) and no device is opened.

---

## 2. UI package — call surfaces

Files: NEW `src/ui/call.rs` (the incoming banner, the in-call pane, the small
call button state); hooks in `src/ui/messages.rs` (a call button in the chat
header for 1:1 user chats, next to info/more — `icons::PHONE`), `src/ui/shell.rs`
(a shell overlay for the active/incoming call above the message pane, the
`Event::CallChanged` handler, the Esc chain entry, actions), `src/ui/mod.rs`,
`src/ui/settings_view.rs` (a "Calls" section: input/output dropdowns from
`call_devices`, a ringtone switch), `src/theme/style.css` (one `wave 7` block,
tokens only).

### 2.1 Chat header call button

- In a 1:1 `ChatKind::User` chat (not Bot, not Saved, not group/channel), the
  header shows a `PHONE` button before `header_info`. Click → `call_start(peer)`.
  Hidden in every other chat kind and in virtual chats. A new `ChatAction::Call`
  variant, handled in the shell.

### 2.2 Incoming call banner + in-call pane (one overlay, `omg-call`)

Design: a centered card over the message pane (like the image viewer's backdrop
but not full-bleed — a 320 px card, flat, tokens only), driven entirely by the
current `CallInfo`:

- **Incoming** (`phase == Incoming`, not yet accepted): avatar (reuse
  `avatar.rs` initials), peer name, "Incoming voice call", two buttons — Accept
  (accent, PHONE) → `call_accept`, Decline (red, CLOSE) → `call_hang_up`. The
  ringtone plays if `settings.calls.ringtone` (a looping beep via a muted-safe
  player; silent under probe).
- **Requesting / Exchanging / Connecting**: avatar, name, a status line
  ("Calling…", "Exchanging keys…", "Connecting…"), one Hang up button (red).
- **Active**: avatar, name, the 4-emoji verification row (`omg-call-emojis`,
  large, with a one-line "Compare these emoji with the person you are calling"
  caption — never assert the call IS secure; the emoji derive only from the
  validated key), a running
  timer `MM:SS` from `connected_at` (a 1 s `glib::timeout`, dropped on close),
  Mute toggle (MIC / a muted variant) → `call_set_muted`, Hang up (red).
- **Ended**: the reason for ~1.5 s ("Call ended", "Declined", "Missed",
  "Call failed"), then the overlay closes and the surface clears. Esc hangs up
  an active/outgoing call and declines an incoming one (top of the Esc chain,
  before the viewers).

Only one call surface exists at a time. Opening any chat does not dismiss it.
A desktop notification fires for an incoming call while the window is not
focused (reuse the `notify` path), titled the caller's name.

### 2.3 Settings "Calls" section

`add_section("Calls")` with: an input-device dropdown and an output-device
dropdown populated from `call_devices()` (first item "System default"), and a
"Ringtone for incoming calls" switch. Selections persist to `settings.calls`.

### 2.4 Acceptance

- `cargo build` (default features) and `cargo build --no-default-features`
  both clean, no warnings; `cargo test` green.
- `bin/gate` PASS with the §1.8 probe steps present and asserting real state
  (the button appears only in user chats; an outgoing call reaches Active with
  the emoji row; mute toggles; hang up clears the surface; an incoming call
  rings, accepts to Active, and declines to cleared).
- `bin/shot 7-call-active Marta 3` (drive it via a new `OMG_SMOKE_CALL=out`
  hook that starts a mock outgoing call after the chat opens) and
  `bin/shot 7-call-incoming Marta 3` (`OMG_SMOKE_CALL=in`).

---

## 3. Video calls — later spike (NOT this wave)

ntgcalls exposes `on_frames` / `send_external_frame` and camera device
descriptions, so video is feasible on the same signaling with `video: true` in
requestCall and a camera `MediaDescription`. Deferred until voice is proven on a
real call; the contract's `outgoing`/phase model already covers it.
