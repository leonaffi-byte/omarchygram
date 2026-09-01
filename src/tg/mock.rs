//! Offline stand-in backend (--smoke mode). Orchestrator-owned.
//!
//! Behaves identically to the real backend so the UI cannot tell them apart.
//! Lets every feature (media, replies, edits, reactions, typing, pagination)
//! be exercised and screenshotted without Telegram credentials.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Local};
use tokio::sync::mpsc;

use super::{paths, AuthState, BackendFlags, ChatSummary, Command, Event, MediaKind, Msg, MsgVersion, Reaction};

fn t(minutes_ago: i64) -> DateTime<Local> {
    Local::now() - Duration::minutes(minutes_ago)
}

fn title(chat_id: i64) -> &'static str {
    match chat_id {
        1 => "Marta",
        2 => "Deni",
        3 => "Mom",
        4 => "Arch Linux ARM",
        _ => "Someone",
    }
}

fn sample_image() -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = std::fs::read_dir("/usr/share/omarchy/themes")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("preview.png"))
        .filter(|p| p.exists())
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// A real on-disk file for mock document downloads, so open-with-default works.
fn sample_document(name: &str) -> Option<PathBuf> {
    let dir = paths::media_dir();
    std::fs::create_dir_all(&dir).ok()?;
    // Basename only — never let a name segment escape the cache dir.
    let name = std::path::Path::new(name).file_name()?.to_string_lossy().to_string();
    let path = dir.join(format!("mock_{name}"));
    if !path.exists() {
        std::fs::write(&path, "omarchygram mock file: build log excerpt\nall green\n").ok()?;
    }
    Some(path)
}

fn msg(id: i32, chat_id: i64, sender: &str, text: &str, ts: DateTime<Local>, outgoing: bool) -> Msg {
    Msg {
        id,
        chat_id,
        chat_title: title(chat_id).to_string(),
        sender: sender.to_string(),
        sender_id: Some(if outgoing { 424242 } else { chat_id }),
        text: text.to_string(),
        ts,
        outgoing,
        media: None,
        doc_name: None,
        reply_to: None,
        reactions: vec![],
        edited: false,
        deleted: false,
    }
}

struct MockState {
    auth: AuthState,
    next_id: i32,
    unread: HashMap<i64, i32>,
    history: HashMap<i64, Vec<Msg>>,
    /// Files "sent" from this session, so download_media returns the original.
    sent_files: HashMap<i32, PathBuf>,
    flags: BackendFlags,
    /// Archived-deleted messages per chat (served when anti_delete is on).
    deleted: HashMap<i64, Vec<Msg>>,
    versions: HashMap<(i64, i32), Vec<MsgVersion>>,
}

impl MockState {
    fn new() -> Self {
        let mut history = HashMap::new();
        history.insert(1, vec![
            msg(101, 1, "Marta", "did you see the fog this morning", t(95), false),
            msg(102, 1, "You", "yeah, rode through it on the way to work", t(93), true),
            Msg { media: Some(MediaKind::Photo), ..msg(103, 1, "Marta", "", t(91), false) },
            Msg {
                reactions: vec![Reaction { emoji: "👍".into(), count: 1 }],
                ..msg(104, 1, "Marta", "send pics next time", t(90), false)
            },
            Msg { reply_to: Some(103), ..msg(105, 1, "You", "that one's from the pass", t(88), true) },
            Msg { media: Some(MediaKind::Sticker), ..msg(106, 1, "Marta", "", t(85), false) },
            msg(107, 1, "Marta", "also are we still on for thursday?", t(12), false),
        ]);
        history.insert(2, vec![
            msg(201, 2, "Deni", "the build is green again", t(340), false),
            msg(202, 2, "You", "what was it in the end?", t(338), true),
            Msg { edited: true, ..msg(203, 2, "Deni", "stale lockfile. always the lockfile", t(335), false) },
            Msg {
                media: Some(MediaKind::Document),
                doc_name: Some("ci-log.txt".into()),
                ..msg(204, 2, "Deni", "", t(330), false)
            },
        ]);
        history.insert(3, vec![
            Msg { media: Some(MediaKind::Voice), ..msg(301, 3, "Mom", "", t(1502), false) },
            msg(302, 3, "Mom", "call me when you're free", t(1500), false),
            msg(303, 3, "You", "will do, after dinner", t(1440), true),
        ]);
        history.insert(4, vec![
            msg(401, 4, "Arch Linux ARM", "linux 7.1.9-arch1-2 has landed in core", t(2100), false),
        ]);
        // Deni already deleted one message and edited another — anti-delete
        // and edit-history have something to show without a live event.
        let mut deleted = HashMap::new();
        deleted.insert(2, vec![Msg { deleted: true, ..msg(205, 2, "Deni", "never mind, wrong chat", t(329), false) }]);
        let mut versions = HashMap::new();
        versions.insert(
            (2, 203),
            vec![MsgVersion { text: "stale lockfile again".into(), replaced_at: t(336) }],
        );
        MockState {
            auth: AuthState::NeedCredentials,
            next_id: 1000,
            unread: HashMap::from([(1, 1), (2, 0), (3, 0), (4, 3)]),
            history,
            sent_files: HashMap::new(),
            flags: BackendFlags::default(),
            deleted,
            versions,
        }
    }

    fn preview(m: &Msg) -> String {
        if !m.text.is_empty() {
            return m.text.clone();
        }
        match m.media {
            Some(MediaKind::Photo) => "[photo]".into(),
            Some(MediaKind::Sticker) => "[sticker]".into(),
            Some(MediaKind::Voice) => "[voice message]".into(),
            Some(MediaKind::Document) => "[file]".into(),
            None => String::new(),
        }
    }
}

pub async fn run(mut cmds: mpsc::UnboundedReceiver<Command>, events: async_channel::Sender<Event>) {
    // Shared with spawned tasks (delayed replies, downloads). Never held
    // across an await.
    let st = Arc::new(Mutex::new(MockState::new()));

    while let Some(cmd) = cmds.recv().await {
        // Mirror the real backend: every command runs concurrently, so the
        // UI's ordering rules get exercised offline too. OMG_MOCK_LATENCY_MS
        // adds a delay before each data command to force out-of-order
        // completions in tests.
        let st = st.clone();
        let events = events.clone();
        tokio::spawn(async move { handle(cmd, st, events).await });
    }
}

async fn handle(cmd: Command, st: Arc<Mutex<MockState>>, events: async_channel::Sender<Event>) {
    if !matches!(cmd, Command::Start(_) | Command::SubmitPhone(..) | Command::SubmitCode(..) | Command::SubmitPassword(..)) {
        if let Some(ms) = std::env::var("OMG_MOCK_LATENCY_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }
    {
        match cmd {
            Command::Start(tx) => {
                // OMG_MOCK_AUTH=1 lets the auth screens be walked offline.
                let auth = if std::env::var("OMG_MOCK_AUTH").is_ok_and(|v| !v.is_empty()) {
                    AuthState::NeedPhone
                } else {
                    AuthState::Ready
                };
                st.lock().unwrap().auth = auth;
                let _ = tx.send(Ok(auth));
            }
            Command::SubmitPhone(_, tx) => {
                st.lock().unwrap().auth = AuthState::NeedCode;
                let _ = tx.send(Ok(AuthState::NeedCode));
            }
            Command::SubmitCode(code, tx) => {
                // "2fa" exercises the password screen; anything else signs straight in.
                let auth = if code == "2fa" { AuthState::NeedPassword } else { AuthState::Ready };
                st.lock().unwrap().auth = auth;
                let _ = tx.send(Ok(auth));
            }
            Command::SubmitPassword(_, tx) => {
                st.lock().unwrap().auth = AuthState::Ready;
                let _ = tx.send(Ok(AuthState::Ready));
            }
            Command::GetDialogs(tx) => {
                let st = st.lock().unwrap();
                let mut out: Vec<ChatSummary> = st
                    .history
                    .iter()
                    .map(|(&chat_id, msgs)| {
                        let last = msgs.last();
                        ChatSummary {
                            id: chat_id,
                            title: title(chat_id).to_string(),
                            last_message: last.map(MockState::preview).unwrap_or_default(),
                            last_time: last.map(|m| m.ts),
                            unread: *st.unread.get(&chat_id).unwrap_or(&0),
                        }
                    })
                    .collect();
                out.sort_by_key(|c| std::cmp::Reverse(c.last_time));
                let _ = tx.send(Ok(out));
            }
            Command::GetHistory { chat_id, before_id, respond } => {
                let st = st.lock().unwrap();
                let msgs = st.history.get(&chat_id).cloned().unwrap_or_default();
                let mut result = match before_id {
                    None => msgs,
                    Some(before) => {
                        // Fabricate one older page so pagination can be exercised, then stop.
                        if chat_id == 1 && msgs.first().is_some_and(|m| m.id == before) {
                            vec![
                                msg(90, 1, "Marta", "older message from last week", t(10000), false),
                                msg(91, 1, "You", "yep, scroll-back works", t(9990), true),
                            ]
                        } else {
                            vec![]
                        }
                    }
                };
                if st.flags.anti_delete {
                    // Same window as the real backend: [oldest returned, before_id).
                    let min_id = result.first().map(|m| m.id).unwrap_or(1);
                    let max_id = before_id.map(|b| b - 1).unwrap_or(i32::MAX);
                    if let Some(del) = st.deleted.get(&chat_id) {
                        for d in del.iter().filter(|d| d.id >= min_id && d.id <= max_id) {
                            if !result.iter().any(|m| m.id == d.id) {
                                result.push(d.clone());
                            }
                        }
                        result.sort_by_key(|m| m.id);
                    }
                }
                let _ = respond.send(Ok(result));
            }
            Command::DownloadMedia { chat_id, msg_id, respond } => {
                // Spawned: downloads must never block other commands.
                let (media, doc_name, sent_path) = {
                    let st = st.lock().unwrap();
                    let found = st
                        .history
                        .get(&chat_id)
                        .and_then(|msgs| msgs.iter().find(|m| m.id == msg_id));
                    (
                        found.and_then(|m| m.media),
                        found.and_then(|m| m.doc_name.clone()),
                        st.sent_files.get(&msg_id).cloned(),
                    )
                };
                tokio::spawn(async move {
                    // Simulate network so loading placeholders are visible.
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let path = if let Some(p) = sent_path {
                        Some(p)
                    } else {
                        match media {
                            Some(MediaKind::Photo | MediaKind::Sticker) => sample_image(),
                            Some(MediaKind::Document) => {
                                sample_document(doc_name.as_deref().unwrap_or("file.txt"))
                            }
                            // Voice playback files are not mocked; UI shows "unavailable".
                            _ => None,
                        }
                    };
                    let _ = respond.send(Ok(path));
                });
            }
            Command::SendText { chat_id, text, reply_to, respond } => {
                let sent = {
                    let mut st = st.lock().unwrap();
                    st.next_id += 1;
                    let sent = Msg { reply_to, ..msg(st.next_id, chat_id, "You", &text, t(0), true) };
                    st.history.entry(chat_id).or_default().push(sent.clone());
                    st.next_id += 1; // reserve the reply's id
                    sent
                };
                // Whole-word triggers only ("editor"/"undeleted" don't count).
                let words: Vec<String> = text.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).collect();
                let demo = if words.iter().any(|w| w == "delete") {
                    Demo::DeleteReply
                } else if words.iter().any(|w| w == "edit") {
                    Demo::EditReply
                } else {
                    Demo::None
                };
                schedule_reply(st.clone(), &events, chat_id, sent.id + 1, demo);
                let _ = respond.send(Ok(sent));
            }
            Command::SendFile { chat_id, path, caption, respond } => {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let is_image = ["png", "jpg", "jpeg", "webp"].iter().any(|ext| {
                    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
                });
                let sent = Msg {
                    media: Some(if is_image { MediaKind::Photo } else { MediaKind::Document }),
                    doc_name: if is_image { None } else { Some(name) },
                    ..msg(st.next_id, chat_id, "You", &caption, t(0), true)
                };
                st.sent_files.insert(sent.id, path);
                st.history.entry(chat_id).or_default().push(sent.clone());
                let _ = respond.send(Ok(sent));
            }
            Command::EditText { chat_id, msg_id, text, respond } => {
                let result = {
                    let mut st = st.lock().unwrap();
                    st.history
                        .get_mut(&chat_id)
                        .and_then(|msgs| msgs.iter_mut().find(|m| m.id == msg_id))
                        .map(|m| {
                            let old = std::mem::replace(&mut m.text, text);
                            m.edited = true;
                            (m.clone(), old)
                        })
                        .map(|(m, old)| {
                            st.versions
                                .entry((chat_id, msg_id))
                                .or_default()
                                .push(MsgVersion { text: old, replaced_at: t(0) });
                            m
                        })
                        .ok_or_else(|| format!("no message {msg_id} in chat {chat_id}"))
                };
                let _ = respond.send(result);
            }
            Command::DeleteMessage { chat_id, msg_id, respond } => {
                if let Some(msgs) = st.lock().unwrap().history.get_mut(&chat_id) {
                    msgs.retain(|m| m.id != msg_id);
                }
                let _ = respond.send(Ok(()));
            }
            Command::MarkRead { chat_id, up_to: _, respond } => {
                let mut st = st.lock().unwrap();
                if !st.flags.ghost_mode {
                    st.unread.insert(chat_id, 0);
                }
                let _ = respond.send(Ok(()));
            }
            Command::SetFlags(flags, tx) => {
                st.lock().unwrap().flags = flags;
                let _ = tx.send(Ok(()));
            }
            Command::GetHistoryAtDate { chat_id, date, respond } => {
                let st = st.lock().unwrap();
                let mut msgs: Vec<Msg> = st
                    .history
                    .get(&chat_id)
                    .map(|v| v.iter().filter(|m| m.ts <= date).cloned().collect())
                    .unwrap_or_default();
                let keep = msgs.len().saturating_sub(50);
                msgs.drain(..keep);
                let _ = respond.send(Ok(msgs));
            }
            Command::GetEditHistory { chat_id, msg_id, respond } => {
                let v = st.lock().unwrap().versions.get(&(chat_id, msg_id)).cloned().unwrap_or_default();
                let _ = respond.send(Ok(v));
            }
        }
    }
}

/// Simulate the other side: a typing signal, then an incoming reply — persisted
/// to history and unread state BEFORE the event, so reopening the chat agrees
/// with what the UI displayed live.
#[derive(Clone, Copy, PartialEq)]
enum Demo {
    None,
    /// The mock reply gets deleted 2.5s after arriving (anti-delete demo).
    DeleteReply,
    /// The mock reply gets edited 2.5s after arriving (edit-history demo).
    EditReply,
}

fn schedule_reply(
    st: Arc<Mutex<MockState>>,
    events: &async_channel::Sender<Event>,
    chat_id: i64,
    reply_id: i32,
    demo: Demo,
) {
    let events = events.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let _ = events
            .send(Event::Typing { chat_id, name: title(chat_id).to_string() })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let reply = msg(reply_id, chat_id, title(chat_id), "(mock reply) got it", t(0), false);
        {
            let mut st = st.lock().unwrap();
            st.history.entry(chat_id).or_default().push(reply.clone());
            *st.unread.entry(chat_id).or_insert(0) += 1;
        }
        let _ = events.send(Event::NewMessage(reply.clone())).await;
        match demo {
            Demo::None => {}
            Demo::DeleteReply => {
                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                {
                    let mut st = st.lock().unwrap();
                    if let Some(msgs) = st.history.get_mut(&chat_id) {
                        msgs.retain(|m| m.id != reply_id);
                    }
                    let mut gone = reply.clone();
                    gone.deleted = true;
                    st.deleted.entry(chat_id).or_default().push(gone);
                }
                let _ = events
                    .send(Event::MessageDeleted { chat_id, msg_ids: vec![reply_id] })
                    .await;
            }
            Demo::EditReply => {
                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                let edited = {
                    let mut st = st.lock().unwrap();
                    st.versions
                        .entry((chat_id, reply_id))
                        .or_default()
                        .push(MsgVersion { text: reply.text.clone(), replaced_at: t(0) });
                    let mut e = reply.clone();
                    e.text = "(mock reply) got it — actually, make that thursday".into();
                    e.edited = true;
                    if let Some(msgs) = st.history.get_mut(&chat_id) {
                        if let Some(m) = msgs.iter_mut().find(|m| m.id == reply_id) {
                            *m = e.clone();
                        }
                    }
                    e
                };
                let _ = events.send(Event::MessageChanged(edited)).await;
            }
        }
    });
}
