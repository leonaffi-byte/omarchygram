//! Voice-call state machine. Orchestrator-owned (security-critical: DH keys,
//! e2e verification). One single-owner actor task owns all call state; the
//! backend command loop and the update loop only *send* it messages, so
//! nothing races (specs/spec-wave7.md §1.6).
//!
//! Without the `calls` feature this is a stub: commands return
//! "voice calls are not built in" and update hooks are no-ops, so the app
//! still builds and the mock/UI/probe work unchanged.

use grammers_tl_types as tl;
use tokio::sync::{mpsc, oneshot};

use super::{CallDevices, CallEndReason, CallInfo, CallPhase, Event, TgError};

/// A UI call command handed to the actor.
#[derive(Debug)]
#[cfg_attr(not(feature = "calls"), allow(dead_code))]
pub(super) enum CallCommand {
    Start(i64),
    Accept,
    HangUp,
    SetMuted(bool),
    SetDevices { call_id: i64, input: String, output: String },
}

/// A plain (feature-independent) connection state, mapped from ntgcalls at the
/// callback boundary so the inbox never carries a library type.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(feature = "calls"), allow(dead_code))]
enum ConnKind {
    Connected,
    Connecting,
    Failed,
}

/// The actor's single inbox. All variants use plain types.
#[cfg_attr(not(feature = "calls"), allow(dead_code))]
enum CallMsg {
    Command(CallCommand, oneshot::Sender<Result<(), TgError>>),
    Update(tl::enums::PhoneCall),
    Signaling { call_id: i64, data: Vec<u8> },
    /// From the ntgcalls signaling callback (its own thread): bytes to send.
    OutSignaling(u64, Vec<u8>),
    /// From the ntgcalls connection callback.
    Connection(u64, ConnKind),
    /// A ring/connect timeout for a generation fired.
    Timer(u64, u64),
    #[cfg(feature = "calls")]
    DhReady(u64, Result<tl::enums::messages::DhConfig, String>),
    #[cfg(feature = "calls")]
    RpcReady(u64, RpcKind, Result<tl::enums::phone::PhoneCall, String>),
    Shutdown(oneshot::Sender<()>),
}

#[cfg(feature = "calls")]
#[derive(Clone, Copy)]
enum RpcKind { Request, Accept, Confirm }

/// A phase change invalidates every previously armed deadline, even within
/// the same call. An active call never owns a setup deadline.
#[cfg(any(feature = "calls", test))]
#[derive(Default)]
struct Deadline { serial: u64 }

#[cfg(any(feature = "calls", test))]
impl Deadline {
    fn invalidate(&mut self) { self.serial = self.serial.wrapping_add(1); }
    fn accepts(&self, serial: u64, phase: CallPhase) -> bool {
        self.serial == serial && matches!(phase, CallPhase::Requesting | CallPhase::Incoming | CallPhase::Exchanging | CallPhase::Connecting)
    }
}

/// Handle the backend keeps once connected. Cheap to clone.
#[derive(Clone)]
pub(super) struct CallHandle {
    inner: Option<mpsc::UnboundedSender<CallMsg>>,
    stopped: Option<async_channel::Receiver<()>>,
}

impl CallHandle {
    /// A handle attached to no actor (calls not built, or not connected yet).
    pub(super) fn disabled() -> CallHandle {
        CallHandle { inner: None, stopped: None }
    }

    pub(super) async fn command(&self, cmd: CallCommand) -> Result<(), TgError> {
        let Some(tx) = &self.inner else {
            return Err("voice calls are not built in".to_string());
        };
        let (rtx, rrx) = oneshot::channel();
        tx.send(CallMsg::Command(cmd, rtx)).map_err(|_| "the call service is gone".to_string())?;
        rrx.await.map_err(|_| "the call service dropped the request".to_string())?
    }

    /// Feed a raw `updatePhoneCall` in (from the update loop). Never blocks.
    pub(super) fn feed_update(&self, call: tl::enums::PhoneCall) {
        if let Some(tx) = &self.inner {
            let _ = tx.send(CallMsg::Update(call));
        }
    }

    /// Feed inbound signaling data in.
    pub(super) fn feed_signaling(&self, call_id: i64, data: Vec<u8>) {
        if let Some(tx) = &self.inner {
            let _ = tx.send(CallMsg::Signaling { call_id, data });
        }
    }

    /// End any active call and wait for teardown (before sign-out / delete).
    pub(super) async fn shutdown(&self) {
        if let Some(tx) = &self.inner {
            let (stx, srx) = oneshot::channel();
            if tx.send(CallMsg::Shutdown(stx)).is_ok() {
                let _ = srx.await;
            }
        }
        if let Some(stopped) = &self.stopped {
            let _ = stopped.recv().await;
        }
    }
}

/// Builds the `CallInfo` the UI sees.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(feature = "calls"), allow(dead_code))]
fn info(
    id: i64,
    peer_id: i64,
    peer_name: &str,
    outgoing: bool,
    phase: CallPhase,
    muted: bool,
    emojis: &str,
    connected_at: Option<chrono::DateTime<chrono::Local>>,
    end_reason: Option<CallEndReason>,
) -> CallInfo {
    CallInfo {
        id,
        peer_id,
        peer_name: peer_name.to_string(),
        outgoing,
        phase,
        muted,
        emojis: emojis.to_string(),
        connected_at,
        end_reason,
        error: None,
    }
}

// ===================== device listing (no actor needed) =====================

#[cfg(feature = "calls")]
pub(super) fn devices() -> Result<CallDevices, TgError> {
    use super::CallDevice;
    let d = ntgcalls::NTgCalls::get_media_devices().map_err(|e| format!("audio devices unavailable: {e:?}"))?;
    let map = |list: Vec<ntgcalls::DeviceInfo>| -> Vec<super::CallDevice> {
        std::iter::once(CallDevice { id: String::new(), name: "System default".to_string() })
            .chain(list.into_iter().map(|d| CallDevice { id: d.metadata, name: d.name }))
            .collect()
    };
    Ok(CallDevices { input: map(d.microphone), output: map(d.speaker) })
}

#[cfg(not(feature = "calls"))]
pub(super) fn devices() -> Result<CallDevices, TgError> {
    Err("voice calls are not built in".to_string())
}

// ===================== spawn =====================

#[cfg(not(feature = "calls"))]
pub(super) fn spawn(
    _client: grammers_client::Client,
    _ctx: std::sync::Arc<super::real::Ctx>,
    _events: async_channel::Sender<Event>,
) -> CallHandle {
    CallHandle::disabled()
}

#[cfg(feature = "calls")]
pub(super) fn spawn(
    client: grammers_client::Client,
    ctx: std::sync::Arc<super::real::Ctx>,
    events: async_channel::Sender<Event>,
) -> CallHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (stopped_tx, stopped_rx) = async_channel::bounded::<()>(1);
    let self_tx = tx.clone();
    // The ntgcalls handle is !Sync (its async methods hold &self across an
    // internal spawn_blocking), so the actor future is not Send and cannot run
    // on the multi-thread backend runtime. Give it its own thread with a
    // current-thread runtime, like the local-services thread.
    std::thread::Builder::new()
        .name("omg-calls".to_string())
        .spawn(move || {
            // The actor's acknowledgement precedes NTgCalls destruction.
            // Signal completion only after the actor and runtime are dropped.
            let _stopped = stopped_tx;
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("omarchygram: call runtime failed: {e}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let mut actor = real::Actor::new(client, ctx, events, self_tx);
                actor.run(rx).await;
            });
        })
        .expect("spawn calls thread");
    CallHandle { inner: Some(tx), stopped: Some(stopped_rx) }
}

#[cfg(feature = "calls")]
mod real {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{DateTime, Local};
    use grammers_client::Client;
    use grammers_tl_types as tl;
    use ntgcalls::{AudioDescription, ConnectionState, DhConfig, MediaDescription, MediaSource, NTgCalls, RTCServer, StreamMode};
    use tokio::sync::mpsc;

    use super::super::real::Ctx;
    use super::{info, CallCommand, CallMsg, ConnKind, Deadline, RpcKind};
    use crate::tg::{CallEndReason, CallPhase, Event, TgError};

    const RING_TIMEOUT: Duration = Duration::from_secs(60);
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

    enum State {
        Idle,
        Outgoing(Call),
        Incoming(Call),
        Connecting(Call),
        Active(Call),
        Ending,
    }

    struct Call {
        generation: u64,
        id: i64,
        access_hash: i64,
        peer_id: i64,
        peer_name: String,
        outgoing: bool,
        muted: bool,
        emojis: String,
        connected_at: Option<DateTime<Local>>,
        keys_exchanged: bool,
        transport_started: bool,
    }

    pub(super) struct Actor {
        client: Client,
        ctx: Arc<Ctx>,
        events: async_channel::Sender<Event>,
        ntg: NTgCalls,
        state: State,
        generation: u64,
        self_tx: mpsc::UnboundedSender<CallMsg>,
        /// Incoming call's (id, g_a_hash) until we accept.
        incoming_g_a: Option<(i64, Vec<u8>)>,
        deadline: Deadline,
        rpc_tasks: Vec<tokio::task::JoinHandle<()>>,
        pending_updates: Vec<tl::enums::PhoneCall>,
        audio_devices: Option<(String, String)>,
    }

    impl Actor {
        pub(super) fn new(client: Client, ctx: Arc<Ctx>, events: async_channel::Sender<Event>, self_tx: mpsc::UnboundedSender<CallMsg>) -> Actor {
            Actor {
                client,
                ctx,
                events,
                ntg: NTgCalls::new(),
                state: State::Idle,
                generation: 0,
                self_tx,
                incoming_g_a: None,
                deadline: Deadline::default(),
                rpc_tasks: Vec::new(),
                pending_updates: Vec::new(),
                audio_devices: None,
            }
        }

        pub(super) async fn run(&mut self, mut rx: mpsc::UnboundedReceiver<CallMsg>) {
            while let Some(msg) = rx.recv().await {
                self.rpc_tasks.retain(|task| !task.is_finished());
                match msg {
                    CallMsg::Command(cmd, reply) => {
                        let r = self.on_command(cmd).await;
                        let _ = reply.send(r);
                    }
                    CallMsg::Update(call) => self.on_update(call).await,
                    CallMsg::Signaling { call_id, data } => self.on_signaling(call_id, data).await,
                    CallMsg::OutSignaling(g, bytes) => self.out_signaling(g, bytes).await,
                    CallMsg::Connection(g, kind) => self.on_connection(g, kind).await,
                    CallMsg::Timer(g, serial) => self.on_timer(g, serial).await,
                    CallMsg::DhReady(g, result) => self.on_dh(g, result).await,
                    CallMsg::RpcReady(g, kind, result) => self.on_rpc(g, kind, result).await,
                    CallMsg::Shutdown(done) => {
                        self.end(CallEndReason::Hangup, true).await;
                        for task in self.rpc_tasks.drain(..) { task.abort(); }
                        let _ = done.send(());
                        break;
                    }
                }
            }
            self.end(CallEndReason::Hangup, true).await;
        }

        fn current(&self) -> Option<&Call> {
            match &self.state {
                State::Outgoing(c) | State::Incoming(c) | State::Connecting(c) | State::Active(c) => Some(c),
                State::Idle | State::Ending => None,
            }
        }

        fn current_mut(&mut self) -> Option<&mut Call> {
            match &mut self.state {
                State::Outgoing(c) | State::Incoming(c) | State::Connecting(c) | State::Active(c) => Some(c),
                State::Idle | State::Ending => None,
            }
        }

        fn phase(&self) -> CallPhase {
            match &self.state {
                State::Outgoing(_) => CallPhase::Requesting,
                State::Incoming(_) => CallPhase::Incoming,
                State::Connecting(_) => CallPhase::Connecting,
                State::Active(_) => CallPhase::Active,
                State::Idle | State::Ending => CallPhase::Ended,
            }
        }

        fn emit(&self, phase: CallPhase, end_reason: Option<CallEndReason>) {
            if let Some(c) = self.current() {
                let _ = self.events.try_send(Event::CallChanged(info(
                    c.id, c.peer_id, &c.peer_name, c.outgoing, phase, c.muted, &c.emojis, c.connected_at, end_reason,
                )));
            }
        }

        async fn on_command(&mut self, cmd: CallCommand) -> Result<(), TgError> {
            match cmd {
                CallCommand::Start(user_id) => self.start(user_id).await,
                CallCommand::Accept => self.accept().await,
                CallCommand::HangUp => {
                    self.end(CallEndReason::Hangup, true).await;
                    Ok(())
                }
                CallCommand::SetMuted(m) => self.set_muted(m).await,
                CallCommand::SetDevices { call_id, input, output } => self.change_devices(call_id, input, output).await,
            }
        }

        async fn start(&mut self, user_id: i64) -> Result<(), TgError> {
            if !matches!(self.state, State::Idle) {
                return Err("a call is already in progress".to_string());
            }
            let peer = self.ctx.peer(user_id)?;
            if peer.id.kind() != grammers_client::session::types::PeerKind::User {
                return Err("you can only call a user".to_string());
            }
            let name = self.ctx.title_of(user_id);
            self.generation += 1;
            let generation = self.generation;
            self.state = State::Outgoing(Call {
                generation,
                id: 0,
                access_hash: 0,
                peer_id: user_id,
                peer_name: name,
                outgoing: true,
                muted: false,
                emojis: String::new(),
                connected_at: None,
                keys_exchanged: false,
                transport_started: false,
            });
            self.emit(CallPhase::Requesting, None);
            self.arm_timeout(generation, RING_TIMEOUT);
            self.fetch_dh(generation);
            Ok(())
        }

        fn fetch_dh(&mut self, generation: u64) {
            let client = self.client.clone();
            let tx = self.self_tx.clone();
            self.rpc_tasks.push(tokio::spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(15), client.invoke(
                    &tl::functions::messages::GetDhConfig { version: 0, random_length: 256 },
                )).await.map_err(|_| "Call setup timed out".to_string())
                    .and_then(|r| r.map_err(|_| "Could not prepare call encryption".to_string()));
                let _ = tx.send(CallMsg::DhReady(generation, result));
            }));
        }

        async fn accept(&mut self) -> Result<(), TgError> {
            let generation = match &self.state {
                State::Incoming(c) => c.generation,
                _ => return Err("no incoming call to accept".to_string()),
            };
            self.move_connecting(generation);
            self.arm_timeout(generation, CONNECT_TIMEOUT);
            self.fetch_dh(generation);
            Ok(())
        }

        async fn on_dh(&mut self, generation: u64, result: Result<tl::enums::messages::DhConfig, String>) {
            if self.current().map(|c| c.generation) != Some(generation) { return; }
            let dh = match result {
                Ok(tl::enums::messages::DhConfig::Config(c)) => DhConfig { g: c.g, p: c.p, random: c.random },
                _ => { self.fail(generation, "Call encryption unavailable".into()).await; return; }
            };
            let (user_id, outgoing) = self.current().map(|c| (c.peer_id, c.outgoing)).unwrap();
            // BOTH directions need a native call before initializing its keys.
            if self.ntg.create_p2p_call(user_id).await.is_err() {
                self.fail(generation, "Could not initialize the call".into()).await; return;
            }
            let hash = if outgoing { None } else { self.incoming_g_a.as_ref().map(|(_, h)| h.as_slice()) };
            let exchange = match self.ntg.init_exchange(user_id, &dh, hash).await {
                Ok(exchange) => exchange,
                Err(_) => { self.fail(generation, "Key setup failed".into()).await; return; }
            };
            let protocol = match protocol() {
                Ok(protocol) => protocol,
                Err(error) => { self.fail(generation, error).await; return; }
            };
            let client = self.client.clone();
            let tx = self.self_tx.clone();
            if outgoing {
                let user = match self.input_user(user_id) {
                    Ok(user) => user,
                    Err(_) => { self.fail(generation, "Call recipient unavailable".into()).await; return; }
                };
                self.rpc_tasks.push(tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(15), client.invoke(&tl::functions::phone::RequestCall {
                        video: false, user_id: user, random_id: rand_i32(), g_a_hash: exchange, protocol,
                    })).await.map_err(|_| "Call request timed out".to_string())
                        .and_then(|r| r.map_err(|_| "Call request failed".to_string()));
                    let _ = tx.send(CallMsg::RpcReady(generation, RpcKind::Request, result));
                }));
            } else {
                let peer = match self.input_phone_call() { Ok(peer) => peer, Err(_) => return };
                self.rpc_tasks.push(tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(15), client.invoke(&tl::functions::phone::AcceptCall {
                        peer, g_b: exchange, protocol,
                    })).await.map_err(|_| "Accepting call timed out".to_string())
                        .and_then(|r| r.map_err(|_| "Could not accept call".to_string()));
                    let _ = tx.send(CallMsg::RpcReady(generation, RpcKind::Accept, result));
                }));
            }
        }

        async fn on_rpc(&mut self, generation: u64, kind: RpcKind, result: Result<tl::enums::phone::PhoneCall, String>) {
            if self.current().map(|c| c.generation) != Some(generation) {
                // A server may create the call just after Hang up. Discard the
                // late result instead of leaving the remote peer ringing.
                if let Ok(tl::enums::phone::PhoneCall::Call(call)) = result {
                    let pair = match call.phone_call {
                        tl::enums::PhoneCall::Waiting(c) => Some((c.id,c.access_hash)),
                        tl::enums::PhoneCall::Call(c) => Some((c.id,c.access_hash)),
                        tl::enums::PhoneCall::Accepted(c) => Some((c.id,c.access_hash)),
                        _ => None,
                    };
                    if let Some((id, hash)) = pair { self.discard_id(id, hash, 0, discard_reason(CallEndReason::Hangup)).await; }
                }
                return;
            }
            match result {
                Ok(tl::enums::phone::PhoneCall::Call(call)) => {
                    self.remember_call(&call.phone_call, generation);
                    if matches!(kind, RpcKind::Confirm) { self.move_connecting(generation); }
                    self.on_update(call.phone_call).await;
                    if matches!(kind, RpcKind::Request) {
                        for update in std::mem::take(&mut self.pending_updates) {
                            self.on_update(update).await;
                        }
                    }
                }
                Err(error) => self.fail(generation, error).await,
            }
        }

        async fn set_muted(&mut self, muted: bool) -> Result<(), TgError> {
            let user_id = match self.current() {
                Some(c) if matches!(self.state, State::Active(_)) => c.peer_id,
                _ => return Err("The call is not connected yet".into()),
            };
            let result = if muted { self.ntg.mute(user_id).await } else { self.ntg.unmute(user_id).await };
            result.map_err(|_| "Could not change microphone state; try again".to_string())?;
            if let Some(c) = self.current_mut() { c.muted = muted; }
            self.emit(self.phase(), None);
            Ok(())
        }

        // ---- updates ----

        async fn on_update(&mut self, call: tl::enums::PhoneCall) {
            // Updates can arrive before RequestCall's response. Only that RPC
            // may establish the outgoing ID; a previous call to the same peer
            // must never take ownership of a fresh attempt.
            if self.current().is_some_and(|c| c.outgoing && c.id == 0)
                && !matches!(call, tl::enums::PhoneCall::Requested(_)) {
                if self.pending_updates.len() < 64 { self.pending_updates.push(call); }
                return;
            }
            match call {
                tl::enums::PhoneCall::Requested(r) => self.on_requested(r).await,
                tl::enums::PhoneCall::Accepted(a) => self.on_accepted(a).await,
                tl::enums::PhoneCall::Call(c) => self.on_confirmed(c).await,
                tl::enums::PhoneCall::Discarded(d) => self.on_discarded(d).await,
                tl::enums::PhoneCall::Waiting(w) => {
                    if self.current().is_some_and(|c| c.outgoing && c.peer_id == w.participant_id && c.id == w.id) {
                        self.remember_call_id(w.id, w.access_hash, self.generation);
                    }
                },
                tl::enums::PhoneCall::Empty(_) => {}
            }
        }

        async fn on_requested(&mut self, r: tl::types::PhoneCallRequested) {
            if self.current().is_some_and(|c| c.id == r.id) { return; }
            if !matches!(self.state, State::Idle) {
                self.discard_id(r.id, r.access_hash, 0, tl::enums::PhoneCallDiscardReason::Busy).await;
                return;
            }
            let user_id = r.admin_id;
            let name = self.ctx.title_of(user_id);
            self.generation += 1;
            let generation = self.generation;
            self.state = State::Incoming(Call {
                generation,
                id: r.id,
                access_hash: r.access_hash,
                peer_id: user_id,
                peer_name: name,
                outgoing: false,
                muted: false,
                emojis: String::new(),
                connected_at: None,
                keys_exchanged: false,
                transport_started: false,
            });
            self.incoming_g_a = Some((r.id, r.g_a_hash.clone()));
            let peer = self.input_phone_call().unwrap_or_else(|_| empty_input());
            let client = self.client.clone();
            self.rpc_tasks.push(tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(10), client.invoke(&tl::functions::phone::ReceivedCall { peer })).await;
            }));
            self.emit(CallPhase::Incoming, None);
            self.arm_timeout(generation, RING_TIMEOUT);
        }

        async fn on_accepted(&mut self, a: tl::types::PhoneCallAccepted) {
            let generation = match &self.state {
                State::Outgoing(c) if c.peer_id == a.participant_id && c.id == a.id => c.generation,
                _ => return,
            };
            let user_id = self.current().map(|c| c.peer_id).unwrap_or(0);
            self.remember_call_id(a.id, a.access_hash, generation);
            let auth = match self.ntg.exchange_keys(user_id, &a.g_b, 0).await {
                Ok(a) => a,
                Err(_) => {
                    self.fail(generation, "call encryption failed".to_string()).await;
                    return;
                }
            };
            if self.generation != generation {
                return;
            }
            if let Some(c) = self.current_mut() { c.keys_exchanged = true; }
            self.move_connecting(generation);
            self.arm_timeout(generation, CONNECT_TIMEOUT);
            let peer = self.input_phone_call().unwrap_or_else(|_| empty_input());
            let client = self.client.clone();
            let tx = self.self_tx.clone();
            let protocol = match protocol() {
                Ok(protocol) => protocol,
                Err(error) => { self.fail(generation, error).await; return; }
            };
            self.rpc_tasks.push(tokio::spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(15), client.invoke(&tl::functions::phone::ConfirmCall {
                    peer, g_a: auth.g_a_or_b, key_fingerprint: auth.key_fingerprint, protocol,
                })).await.map_err(|_| "Confirming call timed out".to_string())
                    .and_then(|r| r.map_err(|_| "Could not confirm call".to_string()));
                let _ = tx.send(CallMsg::RpcReady(generation, RpcKind::Confirm, result));
            }));
        }

        async fn on_confirmed(&mut self, c: tl::types::PhoneCall) {
            let generation = match &self.state {
                State::Connecting(cc) if cc.id == c.id && cc.peer_id == (if cc.outgoing { c.participant_id } else { c.admin_id }) => cc.generation,
                _ => return,
            };
            if self.current().is_some_and(|call| !call.outgoing && !call.keys_exchanged) {
                let user_id = self.current().map(|cc| cc.peer_id).unwrap_or(0);
                if self.ntg.exchange_keys(user_id, &c.g_a_or_b, c.key_fingerprint).await.is_err() {
                    self.fail(generation, "call encryption failed".to_string()).await;
                    return;
                }
                if let Some(call) = self.current_mut() { call.keys_exchanged = true; }
                self.move_connecting(generation);
            }
            self.connect(&c, generation).await;
        }

        async fn on_discarded(&mut self, d: tl::types::PhoneCallDiscarded) {
            if self.current().is_some_and(|c| c.id == d.id) {
                let reason = match d.reason {
                    Some(tl::enums::PhoneCallDiscardReason::Busy) => CallEndReason::Declined,
                    Some(tl::enums::PhoneCallDiscardReason::Missed) => CallEndReason::Missed,
                    _ => CallEndReason::Hangup,
                };
                self.end(reason, false).await;
            }
        }

        async fn on_signaling(&mut self, call_id: i64, data: Vec<u8>) {
            if self.current().is_some_and(|c| c.id == call_id) {
                let user_id = self.current().map(|c| c.peer_id).unwrap_or(0);
                if self.ntg.send_signaling_data(user_id, &data).await.is_err() {
                    self.fail(self.generation, "Call signaling failed; try calling again".into()).await;
                }
            }
        }

        // ---- connect + callbacks ----

        async fn connect(&mut self, pc: &tl::types::PhoneCall, generation: u64) {
            if self.current().is_none_or(|c| c.generation != generation || c.transport_started || !c.keys_exchanged) { return; }
            if let Some(c) = self.current_mut() { c.transport_started = true; }
            let user_id = self.current().map(|c| c.peer_id).unwrap_or(0);
            let servers = rtc_servers(&pc.connections);
            let tl::enums::PhoneCallProtocol::Protocol(p) = &pc.protocol;
            let versions = p.library_versions.clone();
            self.install_callbacks(generation);
            if self.ntg.connect_p2p(user_id, &servers, &versions, pc.p2p_allowed, None).await.is_err() {
                self.fail(generation, "call connection failed".to_string()).await;
                return;
            }
            let prefs = crate::settings::load().calls;
            match self.set_stream_sources(user_id, &prefs.input_device, &prefs.output_device).await {
                Ok(()) => self.audio_devices = Some((prefs.input_device, prefs.output_device)),
                Err(error) => self.fail(generation, error).await,
            }
        }

        fn install_callbacks(&mut self, generation: u64) {
            let tx = self.self_tx.clone();
            self.ntg.on_signaling_data(move |_uid, bytes| {
                let _ = tx.send(CallMsg::OutSignaling(generation, bytes));
            });
            let tx = self.self_tx.clone();
            self.ntg.on_connection_change(move |_uid, conn| {
                let kind = match conn.state {
                    ConnectionState::Connected => ConnKind::Connected,
                    ConnectionState::Failed | ConnectionState::Timeout | ConnectionState::Closed => ConnKind::Failed,
                    ConnectionState::Connecting => ConnKind::Connecting,
                };
                let _ = tx.send(CallMsg::Connection(generation, kind));
            });
        }

        async fn change_devices(&mut self, call_id: i64, input: String, output: String) -> Result<(), TgError> {
            let (user_id, muted, generation) = match &self.state {
                State::Active(call) if call.id == call_id => (call.peer_id, call.muted, call.generation),
                _ => return Err("The call is no longer connected".into()),
            };
            let previous = self.audio_devices.clone().unwrap_or_default();
            if let Err(error) = self.set_stream_sources(user_id, &input, &output).await {
                if self.set_stream_sources(user_id, &previous.0, &previous.1).await.is_err() {
                    self.fail(generation, "Audio devices could not be restored; check your audio settings".into()).await;
                } else if muted && self.ntg.mute(user_id).await.is_err() {
                    self.fail(generation, "Could not preserve microphone mute after restoring devices".into()).await;
                }
                return Err(error);
            }
            self.audio_devices = Some((input, output));
            // Replacing capture must never silently unmute the microphone.
            if muted && self.ntg.mute(user_id).await.is_err() {
                self.fail(generation, "Could not preserve microphone mute after switching devices".into()).await;
                return Err("Could not preserve microphone mute".into());
            }
            Ok(())
        }

        async fn set_stream_sources(&mut self, user_id: i64, input: &str, output: &str) -> Result<(), TgError> {
            let audio = |input: String| AudioDescription {
                media_source: MediaSource::Device,
                sample_rate: 48_000,
                channel_count: 1,
                input,
                keep_open: false,
            };
            self
                .ntg
                .set_stream_sources(
                    user_id,
                    StreamMode::Capture,
                    &MediaDescription { microphone: Some(audio(input.to_owned())), speaker: None, camera: None, screen: None },
                )
                .await.map_err(|_| "Could not open the selected audio device".to_string())?;
            self
                .ntg
                .set_stream_sources(
                    user_id,
                    StreamMode::Playback,
                    &MediaDescription { microphone: None, speaker: Some(audio(output.to_owned())), camera: None, screen: None },
                )
                .await.map_err(|_| "Could not open the selected audio device".to_string())?;
            Ok(())
        }

        async fn on_connection(&mut self, generation: u64, kind: ConnKind) {
            if self.generation != generation {
                return;
            }
            match kind {
                ConnKind::Connected => self.go_active(generation).await,
                ConnKind::Failed => self.fail(generation, "call disconnected".to_string()).await,
                ConnKind::Connecting => {
                    if matches!(self.state, State::Active(_)) {
                        self.move_connecting(generation);
                        self.arm_timeout(generation, CONNECT_TIMEOUT);
                    }
                }
            }
        }

        async fn go_active(&mut self, generation: u64) {
            if !matches!(&self.state, State::Connecting(c) if c.generation == generation) {
                return;
            }
            let user_id = self.current().map(|c| c.peer_id).unwrap_or(0);
            let emojis = match self.ntg.get_emojis_fingerprint(user_id).await {
                Ok(value) if !value.is_empty() => value,
                _ => { self.fail(generation, "Could not verify call encryption".into()).await; return; }
            };
            let taken = std::mem::replace(&mut self.state, State::Idle);
            if let State::Connecting(mut c) = taken {
                c.connected_at.get_or_insert_with(Local::now);
                c.emojis = emojis;
                self.state = State::Active(c);
                self.deadline.invalidate();
                self.emit(CallPhase::Active, None);
            }
        }

        async fn out_signaling(&mut self, generation: u64, bytes: Vec<u8>) {
            if self.generation != generation {
                return;
            }
            if let Ok(peer) = self.input_phone_call() {
                let client = self.client.clone();
                let tx = self.self_tx.clone();
                self.rpc_tasks.push(tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(10), client.invoke(&tl::functions::phone::SendSignalingData { peer, data: bytes })).await;
                    if !matches!(result, Ok(Ok(_))) { let _ = tx.send(CallMsg::Connection(generation, ConnKind::Failed)); }
                }));
            }
        }

        // ---- teardown ----

        async fn fail(&mut self, generation: u64, why: String) {
            if self.current().map(|c| c.generation) != Some(generation) {
                return;
            }
            eprintln!("omarchygram: voice call failed"); // never log details (may carry key material)
            self.end_with_error(CallEndReason::Failed, true, Some(why)).await;
        }

        async fn end(&mut self, reason: CallEndReason, local: bool) {
            self.end_with_error(reason, local, None).await;
        }

        async fn end_with_error(&mut self, reason: CallEndReason, local: bool, error: Option<String>) {
            let (id, access_hash, user_id, duration) = match self.current() {
                Some(c) => (c.id, c.access_hash, c.peer_id, c.connected_at.map(|t| (Local::now() - t).num_seconds().clamp(0, i32::MAX as i64) as i32).unwrap_or(0)),
                None => return,
            };
            let emit = self.current().map(|c| {
                info(c.id, c.peer_id, &c.peer_name, c.outgoing, CallPhase::Ended, c.muted, &c.emojis, c.connected_at, Some(reason))
            });
            self.deadline.invalidate();
            self.state = State::Ending;
            if let Some(mut info) = emit {
                info.error = error;
                let _ = self.events.try_send(Event::CallChanged(info));
            }
            let _ = self.ntg.stop(user_id).await;
            if local && id != 0 {
                self.discard_id(id, access_hash, duration, discard_reason(reason)).await;
            }
            self.incoming_g_a = None;
            self.pending_updates.clear();
            self.audio_devices = None;
            self.state = State::Idle;
        }

        async fn discard_id(&self, id: i64, access_hash: i64, duration: i32, reason: tl::enums::PhoneCallDiscardReason) {
            let peer = tl::enums::InputPhoneCall::Call(tl::types::InputPhoneCall { id, access_hash });
            let _ = tokio::time::timeout(Duration::from_secs(3), self.client.invoke(&tl::functions::phone::DiscardCall { video: false, peer, duration, reason, connection_id: 0 })).await;
        }

        // ---- helpers ----

        fn move_connecting(&mut self, generation: u64) {
            let taken = std::mem::replace(&mut self.state, State::Idle);
            match taken {
                State::Outgoing(c) | State::Incoming(c) | State::Connecting(c) | State::Active(c) if c.generation == generation => {
                    self.state = State::Connecting(c);
                    self.emit(CallPhase::Connecting, None);
                }
                other => self.state = other,
            }
        }

        fn remember_call(&mut self, pc: &tl::enums::PhoneCall, generation: u64) {
            match pc {
                tl::enums::PhoneCall::Waiting(w) => self.remember_call_id(w.id, w.access_hash, generation),
                tl::enums::PhoneCall::Call(c) => self.remember_call_id(c.id, c.access_hash, generation),
                tl::enums::PhoneCall::Accepted(a) => self.remember_call_id(a.id, a.access_hash, generation),
                _ => {}
            }
        }

        fn remember_call_id(&mut self, id: i64, access_hash: i64, generation: u64) {
            if let Some(c) = self.current_mut()
                && c.generation == generation && (c.id == 0 || c.id == id) {
                    c.id = id;
                    c.access_hash = access_hash;
                }
        }

        fn input_phone_call(&self) -> Result<tl::enums::InputPhoneCall, TgError> {
            let c = self.current().ok_or_else(|| "no call".to_string())?;
            Ok(tl::enums::InputPhoneCall::Call(tl::types::InputPhoneCall { id: c.id, access_hash: c.access_hash }))
        }

        fn input_user(&self, user_id: i64) -> Result<tl::enums::InputUser, TgError> {
            match self.ctx.peer(user_id)?.into() {
                tl::enums::InputPeer::User(u) => Ok(tl::enums::InputUser::User(tl::types::InputUser { user_id: u.user_id, access_hash: u.access_hash })),
                _ => Err("not a user".to_string()),
            }
        }

        fn arm_timeout(&mut self, generation: u64, after: Duration) {
            self.deadline.invalidate();
            let serial = self.deadline.serial;
            let tx = self.self_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                let _ = tx.send(CallMsg::Timer(generation, serial));
            });
        }

        async fn on_timer(&mut self, generation: u64, serial: u64) {
            if self.current().map(|c| c.generation) != Some(generation) || !self.deadline.accepts(serial, self.phase()) {
                return;
            }
            let reason = match self.state {
                State::Outgoing(_) | State::Incoming(_) => CallEndReason::Missed,
                _ => CallEndReason::Failed,
            };
            self.end(reason, true).await;
        }
    }

    fn discard_reason(reason: CallEndReason) -> tl::enums::PhoneCallDiscardReason {
        match reason {
            CallEndReason::Declined => tl::enums::PhoneCallDiscardReason::Busy,
            CallEndReason::Missed => tl::enums::PhoneCallDiscardReason::Missed,
            CallEndReason::Failed => tl::enums::PhoneCallDiscardReason::Disconnect,
            CallEndReason::Hangup => tl::enums::PhoneCallDiscardReason::Hangup,
        }
    }

    fn protocol() -> Result<tl::enums::PhoneCallProtocol, TgError> {
        let p = NTgCalls::get_protocol().map_err(|_| "Call transport unavailable".to_string())?;
        Ok(tl::enums::PhoneCallProtocol::Protocol(tl::types::PhoneCallProtocol {
            udp_p2p: p.udp_p2p, udp_reflector: p.udp_reflector,
            min_layer: p.min_layer, max_layer: p.max_layer, library_versions: p.library_versions,
        }))
    }

    fn rtc_servers(connections: &[tl::enums::PhoneConnection]) -> Vec<RTCServer> {
        connections
            .iter()
            .map(|c| match c {
                tl::enums::PhoneConnection::Webrtc(w) => RTCServer {
                    id: w.id as u64,
                    ipv4: w.ip.clone(),
                    ipv6: if w.ipv6.is_empty() { w.ip.clone() } else { w.ipv6.clone() },
                    port: w.port as u16,
                    username: w.username.clone(),
                    password: w.password.clone(),
                    turn: w.turn,
                    stun: w.stun,
                    tcp: false,
                    peer_tag: Vec::new(),
                },
                tl::enums::PhoneConnection::Connection(c) => RTCServer {
                    id: c.id as u64,
                    ipv4: c.ip.clone(),
                    ipv6: if c.ipv6.is_empty() { c.ip.clone() } else { c.ipv6.clone() },
                    port: c.port as u16,
                    username: String::new(),
                    password: String::new(),
                    turn: false,
                    stun: false,
                    tcp: c.tcp,
                    peer_tag: c.peer_tag.clone(),
                },
            })
            .collect()
    }

    fn empty_input() -> tl::enums::InputPhoneCall {
        tl::enums::InputPhoneCall::Call(tl::types::InputPhoneCall { id: 0, access_hash: 0 })
    }

    fn rand_i32() -> i32 {
        (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) as i32) ^ (std::process::id() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::{Deadline, CallPhase};
    #[test]
    fn phase_changes_and_active_calls_reject_stale_deadlines() {
        let mut deadline = Deadline::default();
        let ringing = deadline.serial;
        assert!(deadline.accepts(ringing, CallPhase::Incoming));
        deadline.invalidate();
        assert!(!deadline.accepts(ringing, CallPhase::Connecting));
        let connecting = deadline.serial;
        assert!(deadline.accepts(connecting, CallPhase::Connecting));
        assert!(!deadline.accepts(connecting, CallPhase::Active));
        assert!(!deadline.accepts(connecting, CallPhase::Ended));
        deadline.invalidate();
        assert!(!deadline.accepts(connecting, CallPhase::Requesting));
    }
}
