//! Offline stand-in backend (--smoke mode). Orchestrator-owned; UI workers
//! may extend the FIXTURES (see specs/spec-wave5.md §7) but not the
//! command semantics.
//!
//! Behaves identically to the real backend so the UI cannot tell them apart.
//! Lets every feature (media, replies, edits, reactions, typing, pagination,
//! search, folders, stickers, presence, read state…) be exercised and
//! screenshotted without Telegram credentials.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Local};
use tokio::sync::mpsc;

use super::{
    AuthState, BackendFlags, ChatInfo, ChatKind, ChatSummary, Command, Contact, Event, Folder, Gif,
    Me, MediaKind, Member, MemberRole, Msg, MsgVersion, MuteMode, Presence, Reaction, SharedKind,
    Span, SpanKind, Sticker, StickerPack, WebPreview, parse_markdown, paths, reject, to_markdown,
};

pub const ME_ID: i64 = 424242;

fn t(minutes_ago: i64) -> DateTime<Local> {
    Local::now() - Duration::minutes(minutes_ago)
}

/// Theme preview images double as photos/avatars; `n` picks a different
/// one per chat so avatars are distinguishable.
fn sample_image_n(n: usize) -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = std::fs::read_dir("/usr/share/omarchy/themes")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("preview.png"))
        .filter(|p| p.exists())
        .collect();
    hits.sort();
    if hits.is_empty() {
        return None;
    }
    Some(hits[n % hits.len()].clone())
}

/// A real on-disk file for mock document downloads, so open-with-default works.
fn sample_document(name: &str) -> Option<PathBuf> {
    let dir = paths::media_dir();
    std::fs::create_dir_all(&dir).ok()?;
    // Basename only — never let a name segment escape the cache dir.
    let name = std::path::Path::new(name)
        .file_name()?
        .to_string_lossy()
        .to_string();
    let path = dir.join(format!("mock_{name}"));
    if !path.exists() {
        std::fs::write(
            &path,
            "omarchygram mock file: build log excerpt\nall green\n",
        )
        .ok()?;
    }
    Some(path)
}

struct MockChat {
    id: i64,
    title: String,
    kind: ChatKind,
    username: String,
    pinned: bool,
    muted: bool,
    archived: bool,
    unread_mark: bool,
    presence: Presence,
    has_photo: bool,
    draft: String,
    about: String,
    members: Option<i32>,
    mentions: i32,
    phone: String,
    is_contact: bool,
}

fn chat(id: i64, title: &str, kind: ChatKind) -> MockChat {
    MockChat {
        id,
        title: title.to_string(),
        kind,
        username: String::new(),
        pinned: false,
        muted: false,
        archived: false,
        unread_mark: false,
        presence: Presence::Unknown,
        has_photo: false,
        draft: String::new(),
        about: String::new(),
        members: None,
        mentions: 0,
        phone: String::new(),
        is_contact: false,
    }
}

fn msg(
    id: i32,
    chat_id: i64,
    chat_title: &str,
    sender: &str,
    text: &str,
    ts: DateTime<Local>,
    outgoing: bool,
) -> Msg {
    Msg {
        id,
        chat_id,
        chat_title: chat_title.to_string(),
        sender: sender.to_string(),
        sender_id: Some(if outgoing { ME_ID } else { chat_id }),
        text: text.to_string(),
        markdown: text.to_string(),
        ts,
        outgoing,
        ..Msg::default()
    }
}

/// Message whose text carries Telegram-style markers.
fn fmt_msg(
    id: i32,
    chat_id: i64,
    chat_title: &str,
    sender: &str,
    markdown: &str,
    ts: DateTime<Local>,
    outgoing: bool,
) -> Msg {
    let (text, spans) = parse_markdown(markdown);
    Msg {
        text,
        spans,
        markdown: markdown.to_string(),
        ..msg(id, chat_id, chat_title, sender, "", ts, outgoing)
    }
}

fn group_msg(id: i32, sender: &str, sender_id: i64, text: &str, ts: DateTime<Local>) -> Msg {
    Msg {
        sender_id: Some(sender_id),
        ..fmt_msg(id, 4, "Arch Linux ARM", sender, text, ts, false)
    }
}

fn older_marta_messages() -> Vec<Msg> {
    vec![
        msg(
            90,
            1,
            "Marta",
            "Marta",
            "older message from last week",
            t(10_000),
            false,
        ),
        msg(
            91,
            1,
            "Marta",
            "You",
            "yep, scroll-back works",
            t(9_990),
            true,
        ),
    ]
}

struct MockState {
    auth: AuthState,
    next_id: i32,
    next_chat_id: i64,
    chats: BTreeMap<i64, MockChat>,
    unread: HashMap<i64, i32>,
    history: HashMap<i64, Vec<Msg>>,
    /// Files "sent" from this session, so download_media returns the original.
    sent_files: HashMap<i32, PathBuf>,
    flags: BackendFlags,
    /// Archived-deleted messages per chat (served when anti_delete is on).
    deleted: HashMap<i64, Vec<Msg>>,
    versions: HashMap<(i64, i32), Vec<MsgVersion>>,
    read_outbox: HashMap<i64, i32>,
    read_inbox: HashMap<i64, i32>,
    pinned_msg: HashMap<i64, i32>,
    contacts: Vec<Contact>,
    folders: Vec<Folder>,
    /// Commands that already failed once (OMG_MOCK_FAIL_ONCE).
    failed_once: HashSet<&'static str>,
}

impl MockState {
    fn new() -> Self {
        let mut chats = BTreeMap::new();
        let mut c = chat(1, "Marta", ChatKind::User);
        c.presence = Presence::Online;
        c.has_photo = true;
        c.username = "marta_k".into();
        c.about = "photographer, cyclist".into();
        c.phone = "+1 555 0142".into();
        c.is_contact = true;
        chats.insert(1, c);
        let mut c = chat(2, "Deni", ChatKind::User);
        c.presence = Presence::LastSeen(t(140));
        c.muted = true;
        c.draft = "let me check".into();
        c.username = "deni".into();
        c.is_contact = true;
        chats.insert(2, c);
        let mut c = chat(3, "Mom", ChatKind::User);
        c.presence = Presence::LastWeek;
        c.pinned = true;
        c.phone = "+1 555 0101".into();
        c.is_contact = true;
        chats.insert(3, c);
        let mut c = chat(4, "Arch Linux ARM", ChatKind::Group);
        c.members = Some(42);
        c.mentions = 1;
        c.has_photo = true;
        c.about = "Kernel and userland for ARM boards. Be nice.".into();
        chats.insert(4, c);
        let mut c = chat(5, "Omarchy News", ChatKind::Channel);
        c.username = "omarchy_news".into();
        c.members = Some(1830);
        c.about = "Release notes and tips for Omarchy.".into();
        chats.insert(5, c);
        chats.insert(ME_ID, chat(ME_ID, "Saved Messages", ChatKind::Saved));
        let mut c = chat(7, "Old project", ChatKind::Group);
        c.archived = true;
        c.members = Some(4);
        chats.insert(7, c);

        let mut history = HashMap::new();
        history.insert(
            1,
            vec![
                Msg {
                    media: Some(MediaKind::Photo),
                    photo_size: Some((1024, 768)),
                    ..msg(100, 1, "Marta", "Marta", "fog on the ridge", t(96), false)
                },
                msg(
                    101,
                    1,
                    "Marta",
                    "Marta",
                    "did you see the fog this morning",
                    t(95),
                    false,
                ),
                msg(
                    102,
                    1,
                    "Marta",
                    "You",
                    "yeah, rode through it on the way to work",
                    t(93),
                    true,
                ),
                Msg {
                    media: Some(MediaKind::Photo),
                    photo_size: Some((1280, 800)),
                    ..msg(103, 1, "Marta", "Marta", "", t(91), false)
                },
                Msg {
                    reactions: vec![Reaction {
                        emoji: "👍".into(),
                        count: 1,
                        chosen: false,
                    }],
                    ..msg(
                        104,
                        1,
                        "Marta",
                        "Marta",
                        "send pics next time",
                        t(90),
                        false,
                    )
                },
                Msg {
                    reply_to: Some(103),
                    ..msg(
                        105,
                        1,
                        "Marta",
                        "You",
                        "that one's from the pass",
                        t(88),
                        true,
                    )
                },
                Msg {
                    media: Some(MediaKind::Sticker),
                    sticker_emoji: Some("😀".into()),
                    ..msg(106, 1, "Marta", "Marta", "", t(85), false)
                },
                fmt_msg(
                    107,
                    1,
                    "Marta",
                    "Marta",
                    "also are we still on for **thursday**?",
                    t(12),
                    false,
                ),
            ],
        );
        history.insert(2, vec![
            msg(201, 2, "Deni", "Deni", "the build is green again", t(340), false),
            msg(202, 2, "Deni", "You", "what was it in the end?", t(338), true),
            Msg { edited: true, ..msg(203, 2, "Deni", "Deni", "stale lockfile. always the lockfile", t(335), false) },
            Msg {
                media: Some(MediaKind::Document),
                doc_name: Some("ci-log.txt".into()),
                doc_size: Some(18_432),
                ..msg(204, 2, "Deni", "Deni", "", t(330), false)
            },
            fmt_msg(
                206, 2, "Deni", "Deni",
                "**fixed** the __flaky__ test — run `cargo test -- --nocapture`, details at [the PR](https://github.com/omarchy/omarchygram/pull/12)",
                t(300), false,
            ),
            fmt_msg(207, 2, "Deni", "You", "```rust\nfn main() {\n    println!(\"hi\");\n}\n```", t(298), true),
            fmt_msg(208, 2, "Deni", "Deni", "the answer is ||42|| by the way ~~not 41~~", t(296), false),
        ]);
        history.insert(
            3,
            vec![
                Msg {
                    media: Some(MediaKind::Voice),
                    duration: Some(12),
                    ..msg(301, 3, "Mom", "Mom", "", t(1502), false)
                },
                msg(
                    302,
                    3,
                    "Mom",
                    "Mom",
                    "call me when you're free",
                    t(1500),
                    false,
                ),
                msg(303, 3, "Mom", "You", "will do, after dinner", t(1440), true),
            ],
        );
        history.insert(
            4,
            vec![
                group_msg(
                    401,
                    "Robin",
                    4001,
                    "linux 7.1.9-arch1-2 has landed in core",
                    t(2100),
                ),
                group_msg(
                    402,
                    "Ada",
                    4002,
                    "anyone tried it on the rpi5 yet?",
                    t(2050),
                ),
                Msg {
                    spans: vec![Span {
                        start: 0,
                        end: 6,
                        kind: SpanKind::Mention(4001),
                    }],
                    ..group_msg(403, "Kai", 4003, "@robin does it need a new dtb?", t(2000))
                },
                Msg {
                    media: Some(MediaKind::Video),
                    doc_name: Some("boot-demo.mp4".into()),
                    doc_size: Some(5_400_000),
                    duration: Some(34),
                    ..group_msg(404, "Ada", 4002, "", t(1990))
                },
                Msg {
                    media: Some(MediaKind::Gif),
                    doc_name: Some("it-works.mp4".into()),
                    ..group_msg(405, "Robin", 4001, "", t(1980))
                },
                group_msg(406, "Robin", 4001, "no, the old one boots fine", t(1970)),
            ],
        );
        history.insert(5, vec![
            Msg {
                views: Some(1204),
                pinned: true,
                ..fmt_msg(501, 5, "Omarchy News", "Omarchy News", "**Omarchy 3.2** is out: new themes, faster startup, a fresh walker.", t(3000), false)
            },
            Msg {
                views: Some(880),
                forwarded_from: Some("Arch Linux News".into()),
                ..msg(502, 5, "Omarchy News", "Omarchy News", "The kernel 7.1 series is now in [core].", t(2900), false)
            },
            Msg {
                views: Some(640),
                webpage: Some(WebPreview {
                    url: "https://omarchy.org/blog/theme-engine".into(),
                    site_name: "omarchy.org".into(),
                    title: "How the theme engine works".into(),
                    description: "One colors.toml, every app retinted. A tour of the pipeline behind omarchy-theme-set.".into(),
                }),
                ..fmt_msg(503, 5, "Omarchy News", "Omarchy News", "Read about the theme engine: https://omarchy.org/blog/theme-engine", t(600), false)
            },
        ]);
        history.insert(
            ME_ID,
            vec![
                msg(
                    601,
                    ME_ID,
                    "Saved Messages",
                    "You",
                    "todo: renew the domain",
                    t(4000),
                    true,
                ),
                Msg {
                    media: Some(MediaKind::Document),
                    doc_name: Some("notes.md".into()),
                    doc_size: Some(2_048),
                    ..msg(602, ME_ID, "Saved Messages", "You", "", t(3990), true)
                },
            ],
        );
        history.insert(
            7,
            vec![
                msg(
                    701,
                    7,
                    "Old project",
                    "Sam",
                    "archiving this, thanks everyone",
                    t(20_000),
                    false,
                ),
                Msg {
                    media: Some(MediaKind::Audio),
                    doc_name: Some("outro.mp3".into()),
                    doc_size: Some(3_100_000),
                    duration: Some(183),
                    ..msg(702, 7, "Old project", "Sam", "", t(19_990), false)
                },
                Msg {
                    media: Some(MediaKind::VideoNote),
                    duration: Some(9),
                    ..msg(703, 7, "Old project", "Sam", "", t(19_980), false)
                },
                Msg {
                    media: Some(MediaKind::Unsupported),
                    ..msg(704, 7, "Old project", "Sam", "", t(19_970), false)
                },
            ],
        );
        // Deni already deleted one message and edited another — anti-delete
        // and edit-history have something to show without a live event.
        let mut deleted = HashMap::new();
        deleted.insert(
            2,
            vec![Msg {
                deleted: true,
                ..msg(
                    205,
                    2,
                    "Deni",
                    "Deni",
                    "never mind, wrong chat",
                    t(329),
                    false,
                )
            }],
        );
        let mut versions = HashMap::new();
        versions.insert(
            (2, 203),
            vec![MsgVersion {
                text: "stale lockfile again".into(),
                replaced_at: t(336),
            }],
        );
        let contacts = vec![
            Contact {
                user_id: 1,
                name: "Marta".into(),
                username: "marta_k".into(),
                phone: "+1 555 0142".into(),
                presence: Presence::Online,
                has_photo: true,
            },
            Contact {
                user_id: 2,
                name: "Deni".into(),
                username: "deni".into(),
                phone: String::new(),
                presence: Presence::LastSeen(t(140)),
                has_photo: false,
            },
            Contact {
                user_id: 3,
                name: "Mom".into(),
                username: String::new(),
                phone: "+1 555 0101".into(),
                presence: Presence::LastWeek,
                has_photo: false,
            },
            Contact {
                user_id: 5005,
                name: "Sam Rivera".into(),
                username: "samr".into(),
                phone: "+1 555 0177".into(),
                presence: Presence::Recently,
                has_photo: false,
            },
            Contact {
                user_id: 5006,
                name: "Yuki Tanaka".into(),
                username: String::new(),
                phone: "+81 90 0000 0000".into(),
                presence: Presence::LastMonth,
                has_photo: false,
            },
        ];
        let folders = vec![
            Folder {
                id: 1,
                title: "Work".into(),
                chats: vec![2, 4],
            },
            Folder {
                id: 2,
                title: "Family".into(),
                chats: vec![3],
            },
        ];
        MockState {
            auth: AuthState::NeedCredentials,
            next_id: 1000,
            next_chat_id: 100,
            chats,
            unread: HashMap::from([(1, 1), (2, 0), (3, 0), (4, 3), (5, 0), (ME_ID, 0), (7, 0)]),
            history,
            sent_files: HashMap::new(),
            flags: BackendFlags::default(),
            deleted,
            versions,
            read_outbox: HashMap::from([(1, 102), (2, 207), (3, 303)]),
            read_inbox: HashMap::new(),
            pinned_msg: HashMap::from([(5, 501)]),
            contacts,
            folders,
            failed_once: HashSet::new(),
        }
    }

    fn preview(m: &Msg) -> String {
        if !m.text.is_empty() {
            return m.text.clone();
        }
        match m.media {
            Some(MediaKind::Photo) => "[photo]".into(),
            Some(MediaKind::Sticker) => {
                format!("{} [sticker]", m.sticker_emoji.clone().unwrap_or_default())
                    .trim()
                    .to_string()
            }
            Some(MediaKind::Voice) => "[voice message]".into(),
            Some(MediaKind::Document) => "[file]".into(),
            Some(MediaKind::Video) => "[video]".into(),
            Some(MediaKind::Gif) => "[GIF]".into(),
            Some(MediaKind::Audio) => "[audio]".into(),
            Some(MediaKind::VideoNote) => "[video message]".into(),
            Some(MediaKind::Unsupported) => "[unsupported]".into(),
            None => String::new(),
        }
    }

    fn chat_title(&self, chat_id: i64) -> String {
        self.chats
            .get(&chat_id)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| "Someone".into())
    }

    fn summary(&self, c: &MockChat) -> ChatSummary {
        let msgs = self.history.get(&c.id);
        let last = msgs.and_then(|m| m.last());
        let last_sender = match (c.kind, last) {
            (ChatKind::Group, Some(m)) => {
                if m.outgoing {
                    "You".to_string()
                } else {
                    m.sender.split_whitespace().next().unwrap_or("").to_string()
                }
            }
            _ => String::new(),
        };
        ChatSummary {
            id: c.id,
            title: c.title.clone(),
            kind: c.kind,
            username: c.username.clone(),
            last_message: last.map(MockState::preview).unwrap_or_default(),
            last_sender,
            last_time: last.map(|m| m.ts),
            last_msg_id: last.map(|m| m.id).unwrap_or(0),
            last_outgoing: last.is_some_and(|m| m.outgoing),
            unread: *self.unread.get(&c.id).unwrap_or(&0),
            mentions: c.mentions,
            unread_mark: c.unread_mark,
            read_inbox_max_id: *self.read_inbox.get(&c.id).unwrap_or(&0),
            read_outbox_max_id: *self.read_outbox.get(&c.id).unwrap_or(&0),
            pinned: c.pinned,
            muted: c.muted,
            archived: c.archived,
            presence: c.presence,
            has_photo: c.has_photo,
            draft: c.draft.clone(),
        }
    }

    fn find(&self, chat_id: i64, msg_id: i32) -> Option<Msg> {
        self.history
            .get(&chat_id)
            .and_then(|v| v.iter().find(|m| m.id == msg_id))
            .or_else(|| {
                self.deleted
                    .get(&chat_id)
                    .and_then(|v| v.iter().find(|m| m.id == msg_id))
            })
            .cloned()
            .or_else(|| {
                (chat_id == 1)
                    .then(older_marta_messages)
                    .into_iter()
                    .flatten()
                    .find(|message| message.id == msg_id)
            })
    }

    fn push(&mut self, chat_id: i64, m: Msg) {
        self.history.entry(chat_id).or_default().push(m);
    }

    fn all_messages_newest_first(&self) -> Vec<Msg> {
        let mut all: Vec<Msg> = self.history.values().flatten().cloned().collect();
        all.sort_by_key(|m| std::cmp::Reverse((m.ts, m.id)));
        all
    }
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

pub async fn run(mut cmds: mpsc::UnboundedReceiver<Command>, events: async_channel::Sender<Event>) {
    // Shared with spawned tasks (delayed replies, downloads). Never held
    // across an await.
    let st = Arc::new(Mutex::new(MockState::new()));

    // Marta goes offline/online every 20s so presence rendering is exercised.
    {
        let st = st.clone();
        let events = events.clone();
        tokio::spawn(async move {
            let mut online = true;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                online = !online;
                let presence = if online {
                    Presence::Online
                } else {
                    Presence::LastSeen(Local::now())
                };
                if let Some(c) = st.lock().unwrap().chats.get_mut(&1) {
                    c.presence = presence;
                }
                if events
                    .send(Event::Presence {
                        user_id: 1,
                        presence,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
    }

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

/// The other side "reads" my message 2s after it is sent.
fn schedule_read(
    st: Arc<Mutex<MockState>>,
    events: &async_channel::Sender<Event>,
    chat_id: i64,
    msg_id: i32,
) {
    let events = events.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        {
            let mut st = st.lock().unwrap();
            let e = st.read_outbox.entry(chat_id).or_insert(0);
            if *e < msg_id {
                *e = msg_id;
            }
        }
        let _ = events
            .send(Event::ReadOutbox {
                chat_id,
                max_id: msg_id,
            })
            .await;
    });
}

async fn handle(cmd: Command, st: Arc<Mutex<MockState>>, events: async_channel::Sender<Event>) {
    if !matches!(
        cmd,
        Command::Start(_)
            | Command::SubmitCredentials { .. }
            | Command::SubmitPhone(..)
            | Command::SubmitCode(..)
            | Command::SubmitPassword(..)
    ) {
        if let Some(ms) = std::env::var("OMG_MOCK_LATENCY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }
    let env_on = |n: &str| std::env::var(n).is_ok_and(|v| !v.is_empty());
    // Test hooks: OMG_MOCK_SLOW=Cmd,Cmd delays those commands 1.5s;
    // OMG_MOCK_FAIL_ONCE=Cmd,Cmd fails the first call of each listed command.
    let listed = |var: &str, name: &str| {
        std::env::var(var).is_ok_and(|v| v.split(',').any(|x| x.trim() == name))
    };
    let name = cmd.name();
    if listed("OMG_MOCK_SLOW", name) {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    if listed("OMG_MOCK_FAIL_ONCE", name) && st.lock().unwrap().failed_once.insert(name) {
        reject(cmd, "mock: transient failure");
        return;
    }
    match cmd {
        Command::Start(tx) => {
            // OMG_MOCK_NEED_CREDS=1 starts at the credentials form;
            // OMG_MOCK_AUTH=1 lets the auth screens be walked offline.
            let auth = if env_on("OMG_MOCK_NEED_CREDS") {
                AuthState::NeedCredentials
            } else if env_on("OMG_MOCK_AUTH") {
                AuthState::NeedPhone
            } else {
                AuthState::Ready
            };
            st.lock().unwrap().auth = auth;
            let _ = tx.send(Ok(auth));
        }
        Command::SubmitCredentials {
            api_id,
            api_hash,
            respond,
        } => {
            // Validated like the real backend, but never written to disk.
            let r = if api_id <= 0 {
                Err("API ID must be a positive number".to_string())
            } else if !crate::config::valid_api_hash(api_hash.trim()) {
                Err("API hash must be 32 hexadecimal characters".to_string())
            } else {
                st.lock().unwrap().auth = AuthState::NeedPhone;
                Ok(AuthState::NeedPhone)
            };
            let _ = respond.send(r);
        }
        Command::SubmitPhone(_, tx) => {
            st.lock().unwrap().auth = AuthState::NeedCode;
            let _ = tx.send(Ok(AuthState::NeedCode));
        }
        Command::SubmitCode(code, tx) => {
            // "2fa" exercises the password screen; anything else signs straight in.
            let auth = if code == "2fa" {
                AuthState::NeedPassword
            } else {
                AuthState::Ready
            };
            st.lock().unwrap().auth = auth;
            let _ = tx.send(Ok(auth));
        }
        Command::SubmitPassword(_, tx) => {
            st.lock().unwrap().auth = AuthState::Ready;
            let _ = tx.send(Ok(AuthState::Ready));
        }
        Command::LogOut(tx) => {
            st.lock().unwrap().auth = AuthState::NeedPhone;
            let _ = tx.send(Ok(AuthState::NeedPhone));
        }
        Command::GetMe(tx) => {
            let _ = tx.send(Ok(Me {
                id: ME_ID,
                name: "Leo Test".into(),
                username: "leotest".into(),
                phone: "+1 555 0100".into(),
                has_photo: false,
            }));
        }
        Command::GetDialogs(tx) => {
            let st = st.lock().unwrap();
            let mut out: Vec<ChatSummary> = st.chats.values().map(|c| st.summary(c)).collect();
            out.sort_by_key(|c| (std::cmp::Reverse(c.pinned), std::cmp::Reverse(c.last_time)));
            let _ = tx.send(Ok(out));
        }
        Command::GetHistory {
            chat_id,
            before_id,
            respond,
        } => {
            let st = st.lock().unwrap();
            let msgs = st.history.get(&chat_id).cloned().unwrap_or_default();
            let mut result = match before_id {
                None => msgs,
                Some(before) => {
                    // Fabricate one older page so pagination can be exercised, then stop.
                    if chat_id == 1 && msgs.first().is_some_and(|m| m.id == before) {
                        older_marta_messages()
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
        Command::GetMessages {
            chat_id,
            ids,
            respond,
        } => {
            let st = st.lock().unwrap();
            let out: Vec<Msg> = ids.iter().filter_map(|&id| st.find(chat_id, id)).collect();
            let _ = respond.send(Ok(out));
        }
        Command::DownloadMedia {
            chat_id,
            msg_id,
            respond,
        } => {
            // Spawned: downloads must never block other commands.
            let (media, doc_name, sent_path) = {
                let st = st.lock().unwrap();
                let found = st.find(chat_id, msg_id);
                (
                    found.as_ref().and_then(|m| m.media),
                    found.as_ref().and_then(|m| m.doc_name.clone()),
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
                        Some(MediaKind::Photo | MediaKind::Sticker) => {
                            sample_image_n(msg_id as usize)
                        }
                        Some(MediaKind::Document | MediaKind::Audio | MediaKind::Video) => {
                            sample_document(doc_name.as_deref().unwrap_or("file.txt"))
                        }
                        // Voice/gif playback files are not mocked; UI shows "unavailable".
                        _ => None,
                    }
                };
                let _ = respond.send(Ok(path));
            });
        }
        Command::DownloadAvatar { chat_id, respond } => {
            let has = st
                .lock()
                .unwrap()
                .chats
                .get(&chat_id)
                .is_some_and(|c| c.has_photo)
                || st
                    .lock()
                    .unwrap()
                    .contacts
                    .iter()
                    .any(|c| c.user_id == chat_id && c.has_photo);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let _ = respond.send(Ok(if has {
                    sample_image_n(chat_id as usize)
                } else {
                    None
                }));
            });
        }
        Command::SendText {
            chat_id,
            text,
            reply_to,
            respond,
        } => {
            let sent = {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let title = st.chat_title(chat_id);
                let sent = Msg {
                    reply_to,
                    ..fmt_msg(st.next_id, chat_id, &title, "You", &text, t(0), true)
                };
                st.push(chat_id, sent.clone());
                st.next_id += 1; // reserve the reply's id
                if let Some(c) = st.chats.get_mut(&chat_id) {
                    c.draft.clear();
                }
                sent
            };
            // Whole-word triggers only ("editor"/"undeleted" don't count).
            let words: Vec<String> = text
                .split_whitespace()
                .map(|w| {
                    w.trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase()
                })
                .collect();
            let demo = if words.iter().any(|w| w == "delete") {
                Demo::DeleteReply
            } else if words.iter().any(|w| w == "edit") {
                Demo::EditReply
            } else {
                Demo::None
            };
            schedule_read(st.clone(), &events, chat_id, sent.id);
            if chat_id != ME_ID {
                schedule_reply(st.clone(), &events, chat_id, sent.id + 1, demo);
            }
            let _ = respond.send(Ok(sent));
        }
        Command::SendFile {
            chat_id,
            path,
            caption,
            respond,
        } => {
            let sent = {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let is_image = ["png", "jpg", "jpeg", "webp"].iter().any(|ext| {
                    path.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
                });
                let size = std::fs::metadata(&path).map(|m| m.len()).ok();
                let title = st.chat_title(chat_id);
                let sent = Msg {
                    media: Some(if is_image {
                        MediaKind::Photo
                    } else {
                        MediaKind::Document
                    }),
                    doc_name: if is_image { None } else { Some(name) },
                    doc_size: size,
                    ..fmt_msg(st.next_id, chat_id, &title, "You", &caption, t(0), true)
                };
                st.sent_files.insert(sent.id, path);
                st.push(chat_id, sent.clone());
                sent
            };
            schedule_read(st.clone(), &events, chat_id, sent.id);
            let _ = respond.send(Ok(sent));
        }
        Command::SendVoice {
            chat_id,
            path,
            duration,
            respond,
        } => {
            let sent = {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let title = st.chat_title(chat_id);
                let sent = Msg {
                    media: Some(MediaKind::Voice),
                    duration: Some(duration),
                    ..msg(st.next_id, chat_id, &title, "You", "", t(0), true)
                };
                st.sent_files.insert(sent.id, path);
                st.push(chat_id, sent.clone());
                sent
            };
            schedule_read(st.clone(), &events, chat_id, sent.id);
            let _ = respond.send(Ok(sent));
        }
        Command::SendSticker {
            chat_id,
            sticker_id,
            respond,
        } => {
            let r = if sticker_id == 9104 {
                Err("animated stickers cannot be sent from Omarchygram yet".to_string())
            } else {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let title = st.chat_title(chat_id);
                let sent = Msg {
                    media: Some(MediaKind::Sticker),
                    sticker_emoji: Some(sticker_emoji(sticker_id).into()),
                    ..msg(st.next_id, chat_id, &title, "You", "", t(0), true)
                };
                st.push(chat_id, sent.clone());
                Ok(sent)
            };
            let _ = respond.send(r);
        }
        Command::SendGif {
            chat_id,
            gif_id,
            respond,
        } => {
            let mut st = st.lock().unwrap();
            st.next_id += 1;
            let title = st.chat_title(chat_id);
            let sent = Msg {
                media: Some(MediaKind::Gif),
                doc_name: Some(format!("gif-{gif_id}.mp4")),
                ..msg(st.next_id, chat_id, &title, "You", "", t(0), true)
            };
            st.push(chat_id, sent.clone());
            let _ = respond.send(Ok(sent));
        }
        Command::EditText {
            chat_id,
            msg_id,
            text,
            respond,
        } => {
            let result = {
                let mut st = st.lock().unwrap();
                let (plain, spans) = parse_markdown(&text);
                let edited = st
                    .history
                    .get_mut(&chat_id)
                    .and_then(|msgs| msgs.iter_mut().find(|m| m.id == msg_id))
                    .map(|m| {
                        let old = std::mem::replace(&mut m.text, plain);
                        m.spans = spans;
                        m.markdown = text.clone();
                        m.edited = true;
                        (m.clone(), old)
                    });
                match edited {
                    Some((m, old)) => {
                        st.versions
                            .entry((chat_id, msg_id))
                            .or_default()
                            .push(MsgVersion {
                                text: old,
                                replaced_at: t(0),
                            });
                        Ok(m)
                    }
                    None => Err(format!("no message {msg_id} in chat {chat_id}")),
                }
            };
            let _ = respond.send(result);
        }
        Command::DeleteMessages {
            chat_id,
            ids,
            respond,
        } => {
            if let Some(msgs) = st.lock().unwrap().history.get_mut(&chat_id) {
                msgs.retain(|m| !ids.contains(&m.id));
            }
            let _ = respond.send(Ok(()));
        }
        Command::ForwardMessages {
            from_chat,
            ids,
            to_chat,
            respond,
        } => {
            let out = {
                let mut st = st.lock().unwrap();
                let title = st.chat_title(to_chat);
                let from_kind = st.chats.get(&from_chat).map(|c| c.kind).unwrap_or_default();
                let from_title = st.chat_title(from_chat);
                let originals: Vec<Msg> = ids
                    .iter()
                    .filter_map(|&id| st.find(from_chat, id))
                    .collect();
                let mut out = Vec::new();
                for o in originals {
                    st.next_id += 1;
                    let origin = if from_kind == ChatKind::Channel {
                        from_title.clone()
                    } else if o.outgoing {
                        "Leo Test".to_string()
                    } else {
                        o.sender.clone()
                    };
                    let copy = Msg {
                        id: st.next_id,
                        chat_id: to_chat,
                        chat_title: title.clone(),
                        sender: "You".into(),
                        sender_id: Some(ME_ID),
                        ts: t(0),
                        outgoing: true,
                        forwarded_from: Some(origin),
                        reply_to: None,
                        reactions: vec![],
                        edited: false,
                        deleted: false,
                        pinned: false,
                        views: None,
                        ..o.clone()
                    };
                    if let Some(p) = st.sent_files.get(&o.id).cloned() {
                        st.sent_files.insert(copy.id, p);
                    }
                    st.push(to_chat, copy.clone());
                    out.push(copy);
                }
                out
            };
            let _ = events.send(Event::DialogsChanged).await;
            let _ = respond.send(Ok(out));
        }
        Command::MarkRead {
            chat_id,
            up_to,
            respond,
        } => {
            let mut st = st.lock().unwrap();
            if !st.flags.ghost_mode {
                st.unread.insert(chat_id, 0);
                st.read_inbox.insert(chat_id, up_to);
                if let Some(c) = st.chats.get_mut(&chat_id) {
                    c.unread_mark = false;
                    c.mentions = 0;
                }
            }
            let _ = respond.send(Ok(()));
        }
        Command::SetFlags(flags, tx) => {
            st.lock().unwrap().flags = flags;
            let _ = tx.send(Ok(()));
        }
        Command::GetHistoryAtDate {
            chat_id,
            date,
            respond,
        } => {
            let st = st.lock().unwrap();
            let mut msgs: Vec<Msg> = st
                .history
                .get(&chat_id)
                .map(|v| v.iter().filter(|m| m.ts <= date).cloned().collect())
                .unwrap_or_default();
            if chat_id == 1 {
                msgs.extend(
                    older_marta_messages()
                        .into_iter()
                        .filter(|message| message.ts <= date),
                );
                msgs.sort_by_key(|message| message.id);
            }
            let keep = msgs.len().saturating_sub(50);
            msgs.drain(..keep);
            let _ = respond.send(Ok(msgs));
        }
        Command::GetEditHistory {
            chat_id,
            msg_id,
            respond,
        } => {
            let v = st
                .lock()
                .unwrap()
                .versions
                .get(&(chat_id, msg_id))
                .cloned()
                .unwrap_or_default();
            let _ = respond.send(Ok(v));
        }
        Command::SearchMessages {
            chat_id,
            query,
            before_id,
            respond,
        } => {
            let st = st.lock().unwrap();
            let mut hits: Vec<Msg> = st
                .history
                .get(&chat_id)
                .map(|v| {
                    v.iter()
                        .filter(|m| before_id.is_none_or(|b| m.id < b))
                        .filter(|m| !query.trim().is_empty() && contains_ci(&m.text, query.trim()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if chat_id == 1 {
                hits.extend(older_marta_messages().into_iter().filter(|message| {
                    before_id.is_none_or(|before| message.id < before)
                        && !query.trim().is_empty()
                        && contains_ci(&message.text, query.trim())
                }));
            }
            hits.sort_by_key(|m| std::cmp::Reverse(m.id));
            hits.truncate(50);
            let _ = respond.send(Ok(hits));
        }
        Command::SearchGlobal { query, respond } => {
            let st = st.lock().unwrap();
            let q = query.trim();
            let mut hits: Vec<Msg> = if q.is_empty() {
                vec![]
            } else {
                st.all_messages_newest_first()
                    .into_iter()
                    .filter(|m| contains_ci(&m.text, q))
                    .collect()
            };
            hits.truncate(50);
            let _ = respond.send(Ok(hits));
        }
        Command::SearchChats { query, respond } => {
            let st = st.lock().unwrap();
            let q = query.trim().trim_start_matches('@');
            let mut out = Vec::new();
            if !q.is_empty() {
                for c in st
                    .contacts
                    .iter()
                    .filter(|c| !st.chats.contains_key(&c.user_id))
                {
                    if contains_ci(&c.name, q) || contains_ci(&c.username, q) {
                        out.push(ChatSummary {
                            id: c.user_id,
                            title: c.name.clone(),
                            kind: ChatKind::User,
                            username: c.username.clone(),
                            presence: c.presence,
                            has_photo: c.has_photo,
                            ..ChatSummary::default()
                        });
                    }
                }
                if contains_ci("omarchy_bot", q) || contains_ci("Omarchy Bot", q) {
                    out.push(ChatSummary {
                        id: 7007,
                        title: "Omarchy Bot".into(),
                        kind: ChatKind::Bot,
                        username: "omarchy_bot".into(),
                        ..ChatSummary::default()
                    });
                }
            }
            let _ = respond.send(Ok(out));
        }
        Command::GetPinnedMessage { chat_id, respond } => {
            let st = st.lock().unwrap();
            let m = st
                .pinned_msg
                .get(&chat_id)
                .and_then(|&id| st.find(chat_id, id));
            let _ = respond.send(Ok(m));
        }
        Command::PinMessage {
            chat_id,
            msg_id,
            pinned,
            respond,
        } => {
            {
                let mut st = st.lock().unwrap();
                if let Some(msgs) = st.history.get_mut(&chat_id) {
                    for m in msgs.iter_mut() {
                        if m.id == msg_id {
                            m.pinned = pinned;
                        } else if pinned {
                            m.pinned = false;
                        }
                    }
                }
                if pinned {
                    st.pinned_msg.insert(chat_id, msg_id);
                } else if st.pinned_msg.get(&chat_id) == Some(&msg_id) {
                    st.pinned_msg.remove(&chat_id);
                }
            }
            let _ = events.send(Event::PinnedChanged { chat_id }).await;
            let _ = respond.send(Ok(()));
        }
        Command::SendReaction {
            chat_id,
            msg_id,
            emoji,
            respond,
        } => {
            let changed = {
                let mut st = st.lock().unwrap();
                st.history
                    .get_mut(&chat_id)
                    .and_then(|msgs| msgs.iter_mut().find(|m| m.id == msg_id))
                    .map(|m| {
                        // Drop my previous reaction, then add the new one.
                        for r in m.reactions.iter_mut().filter(|r| r.chosen) {
                            r.chosen = false;
                            r.count -= 1;
                        }
                        m.reactions.retain(|r| r.count > 0);
                        if let Some(e) = emoji {
                            match m.reactions.iter_mut().find(|r| r.emoji == e) {
                                Some(r) => {
                                    r.count += 1;
                                    r.chosen = true;
                                }
                                None => m.reactions.push(Reaction {
                                    emoji: e,
                                    count: 1,
                                    chosen: true,
                                }),
                            }
                        }
                        m.clone()
                    })
            };
            match changed {
                Some(m) => {
                    let _ = events.send(Event::MessageChanged(m)).await;
                    let _ = respond.send(Ok(()));
                }
                None => drop(respond.send(Err(format!("no message {msg_id} in chat {chat_id}")))),
            }
        }
        Command::GetAvailableReactions(tx) => {
            let _ = tx.send(Ok([
                "👍", "❤️", "🔥", "😂", "😮", "😢", "👏", "🎉", "🙏", "🤔",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()));
        }
        Command::SetPinned {
            chat_id,
            pinned,
            respond,
        } => {
            let ok = st
                .lock()
                .unwrap()
                .chats
                .get_mut(&chat_id)
                .map(|c| c.pinned = pinned)
                .is_some();
            let _ = events.send(Event::DialogsChanged).await;
            let _ = respond.send(if ok {
                Ok(())
            } else {
                Err(format!("unknown chat {chat_id}"))
            });
        }
        Command::SetMuted {
            chat_id,
            mode,
            respond,
        } => {
            let ok = st
                .lock()
                .unwrap()
                .chats
                .get_mut(&chat_id)
                .map(|c| c.muted = mode != MuteMode::Unmute)
                .is_some();
            let _ = events.send(Event::DialogsChanged).await;
            let _ = respond.send(if ok {
                Ok(())
            } else {
                Err(format!("unknown chat {chat_id}"))
            });
        }
        Command::SetArchived {
            chat_id,
            archived,
            respond,
        } => {
            let ok = st
                .lock()
                .unwrap()
                .chats
                .get_mut(&chat_id)
                .map(|c| c.archived = archived)
                .is_some();
            let _ = events.send(Event::DialogsChanged).await;
            let _ = respond.send(if ok {
                Ok(())
            } else {
                Err(format!("unknown chat {chat_id}"))
            });
        }
        Command::MarkUnread {
            chat_id,
            unread,
            respond,
        } => {
            let ok = st
                .lock()
                .unwrap()
                .chats
                .get_mut(&chat_id)
                .map(|c| c.unread_mark = unread)
                .is_some();
            let _ = events.send(Event::DialogsChanged).await;
            let _ = respond.send(if ok {
                Ok(())
            } else {
                Err(format!("unknown chat {chat_id}"))
            });
        }
        Command::DeleteChat { chat_id, respond } => {
            {
                let mut st = st.lock().unwrap();
                st.chats.remove(&chat_id);
                st.history.remove(&chat_id);
                st.unread.remove(&chat_id);
            }
            let _ = events.send(Event::DialogsChanged).await;
            let _ = respond.send(Ok(()));
        }
        Command::ClearHistory { chat_id, respond } => {
            {
                let mut st = st.lock().unwrap();
                st.history.insert(chat_id, vec![]);
                st.unread.insert(chat_id, 0);
                st.pinned_msg.remove(&chat_id);
            }
            let _ = events.send(Event::DialogsChanged).await;
            let _ = respond.send(Ok(()));
        }
        Command::SaveDraft {
            chat_id,
            text,
            reply_to,
            respond,
        } => {
            // The reply target is part of the real draft; the mock keeps only the text.
            let _keep = reply_to;
            let ok = st
                .lock()
                .unwrap()
                .chats
                .get_mut(&chat_id)
                .map(|c| c.draft = text)
                .is_some();
            let _ = respond.send(if ok {
                Ok(())
            } else {
                Err(format!("unknown chat {chat_id}"))
            });
        }
        Command::GetChatInfo { chat_id, respond } => {
            let st = st.lock().unwrap();
            let r = match st.chats.get(&chat_id) {
                Some(c) => Ok(ChatInfo {
                    id: c.id,
                    title: c.title.clone(),
                    kind: c.kind,
                    username: c.username.clone(),
                    phone: c.phone.clone(),
                    about: c.about.clone(),
                    members: c.members,
                    presence: c.presence,
                    has_photo: c.has_photo,
                    muted: c.muted,
                    is_contact: c.is_contact,
                }),
                None => match st.contacts.iter().find(|c| c.user_id == chat_id) {
                    Some(c) => Ok(ChatInfo {
                        id: c.user_id,
                        title: c.name.clone(),
                        kind: ChatKind::User,
                        username: c.username.clone(),
                        phone: c.phone.clone(),
                        presence: c.presence,
                        has_photo: c.has_photo,
                        is_contact: true,
                        ..ChatInfo::default()
                    }),
                    None => Err(format!("unknown chat {chat_id}")),
                },
            };
            let _ = respond.send(r);
        }
        Command::GetMembers {
            chat_id,
            offset,
            limit,
            respond,
        } => {
            let st = st.lock().unwrap();
            let total = st.chats.get(&chat_id).and_then(|c| c.members).unwrap_or(0);
            let named = [
                ("Robin", 4001, MemberRole::Creator, Presence::Online),
                ("Ada", 4002, MemberRole::Admin, Presence::Recently),
                ("Kai", 4003, MemberRole::Member, Presence::LastWeek),
            ];
            let mut all: Vec<Member> = named
                .iter()
                .map(|(n, id, role, p)| Member {
                    user_id: *id,
                    name: n.to_string(),
                    username: n.to_lowercase(),
                    presence: *p,
                    role: *role,
                })
                .collect();
            for i in all.len() as i32..total {
                all.push(Member {
                    user_id: 5000 + i as i64,
                    name: format!("Member {}", i + 1),
                    presence: Presence::LongAgo,
                    ..Member::default()
                });
            }
            let out: Vec<Member> = all
                .into_iter()
                .skip(offset.max(0) as usize)
                .take(limit.max(0) as usize)
                .collect();
            let _ = respond.send(Ok(out));
        }
        Command::GetSharedMedia {
            chat_id,
            kind,
            before_id,
            respond,
        } => {
            let st = st.lock().unwrap();
            let mut hits: Vec<Msg> = st
                .history
                .get(&chat_id)
                .map(|v| {
                    v.iter()
                        .filter(|m| before_id.is_none_or(|b| m.id < b))
                        .filter(|m| match kind {
                            SharedKind::Photos => m.media == Some(MediaKind::Photo),
                            SharedKind::Files => {
                                matches!(m.media, Some(MediaKind::Document | MediaKind::Video))
                            }
                            SharedKind::Links => {
                                m.webpage.is_some()
                                    || m.spans.iter().any(|s| matches!(s.kind, SpanKind::Link(_)))
                            }
                            SharedKind::Voice => m.media == Some(MediaKind::Voice),
                            SharedKind::Music => m.media == Some(MediaKind::Audio),
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            hits.sort_by_key(|m| std::cmp::Reverse(m.id));
            hits.truncate(50);
            let _ = respond.send(Ok(hits));
        }
        Command::GetContacts(tx) => {
            let _ = tx.send(Ok(st.lock().unwrap().contacts.clone()));
        }
        Command::OpenUser { user_id, respond } => {
            let r = {
                let mut st = st.lock().unwrap();
                if let Some(c) = st.chats.get(&user_id) {
                    Ok(st.summary(c))
                } else if let Some(c) = st.contacts.iter().find(|c| c.user_id == user_id).cloned() {
                    let mut nc = chat(user_id, &c.name, ChatKind::User);
                    nc.username = c.username.clone();
                    nc.phone = c.phone.clone();
                    nc.presence = c.presence;
                    nc.has_photo = c.has_photo;
                    nc.is_contact = true;
                    st.chats.insert(user_id, nc);
                    st.history.entry(user_id).or_default();
                    Ok(st.summary(&st.chats[&user_id]))
                } else if user_id == 7007 {
                    st.chats.insert(
                        7007,
                        MockChat {
                            username: "omarchy_bot".into(),
                            ..chat(7007, "Omarchy Bot", ChatKind::Bot)
                        },
                    );
                    st.history.entry(7007).or_default();
                    Ok(st.summary(&st.chats[&7007]))
                } else {
                    Err(format!("unknown user {user_id}"))
                }
            };
            if r.is_ok() {
                let _ = events.send(Event::DialogsChanged).await;
            }
            let _ = respond.send(r);
        }
        Command::CreateGroup {
            title,
            user_ids,
            respond,
        } => {
            let r = {
                let mut st = st.lock().unwrap();
                if title.is_empty() {
                    Err("the group needs a title".to_string())
                } else if user_ids.is_empty() {
                    Err("pick at least one member".to_string())
                } else {
                    st.next_chat_id += 1;
                    let id = st.next_chat_id;
                    let mut c = chat(id, &title, ChatKind::Group);
                    c.members = Some(user_ids.len() as i32 + 1);
                    st.chats.insert(id, c);
                    st.next_id += 1;
                    let first = msg(
                        st.next_id,
                        id,
                        &title,
                        "You",
                        "You created the group",
                        t(0),
                        true,
                    );
                    st.history.insert(id, vec![first]);
                    st.unread.insert(id, 0);
                    Ok(st.summary(&st.chats[&id]))
                }
            };
            if r.is_ok() {
                let _ = events.send(Event::DialogsChanged).await;
            }
            let _ = respond.send(r);
        }
        Command::GetFolders(tx) => {
            let _ = tx.send(Ok(st.lock().unwrap().folders.clone()));
        }
        Command::GetStickerPacks(tx) => {
            let _ = tx.send(Ok(vec![
                StickerPack {
                    id: "recent".into(),
                    title: "Recent".into(),
                    count: 3,
                },
                StickerPack {
                    id: "favorites".into(),
                    title: "Favorites".into(),
                    count: 1,
                },
                StickerPack {
                    id: "9100".into(),
                    title: "Omarchy".into(),
                    count: 4,
                },
            ]));
        }
        Command::GetStickers { pack_id, respond } => {
            let list = match pack_id.as_str() {
                "recent" => vec![9001, 9002, 9003],
                "favorites" => vec![9002],
                "9100" => vec![9101, 9102, 9103, 9104],
                _ => vec![],
            };
            let out: Vec<Sticker> = list
                .into_iter()
                .map(|id| Sticker {
                    id,
                    emoji: sticker_emoji(id).into(),
                    animated: id == 9104,
                })
                .collect();
            let _ = respond.send(Ok(out));
        }
        Command::DownloadSticker {
            sticker_id,
            respond,
        } => {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                let _ = respond.send(Ok(if sticker_id == 9104 {
                    None
                } else {
                    sample_image_n(sticker_id as usize)
                }));
            });
        }
        Command::GetSavedGifs(tx) => {
            let _ = tx.send(Ok(vec![
                Gif {
                    id: 9201,
                    width: 320,
                    height: 240,
                },
                Gif {
                    id: 9202,
                    width: 480,
                    height: 270,
                },
            ]));
        }
        Command::DownloadGif { gif_id, respond } => {
            // No mp4 fixture for any id; the UI shows the card fallback.
            let _wanted = gif_id;
            let _ = respond.send(Ok(None));
        }
    }
}

fn sticker_emoji(id: i64) -> &'static str {
    match id {
        9001 | 9101 => "😀",
        9002 | 9102 => "👍",
        9003 | 9103 => "🔥",
        9104 => "🎉",
        _ => "🙂",
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
        let (title, kind) = {
            let st = st.lock().unwrap();
            (
                st.chat_title(chat_id),
                st.chats.get(&chat_id).map(|c| c.kind).unwrap_or_default(),
            )
        };
        let (name, sender_id) = if kind == ChatKind::Group {
            ("Robin".to_string(), 4001)
        } else {
            (title.clone(), chat_id)
        };
        let _ = events
            .send(Event::Typing {
                chat_id,
                name: name.clone(),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let reply = Msg {
            sender_id: Some(sender_id),
            ..msg(
                reply_id,
                chat_id,
                &title,
                &name,
                "(mock reply) got it",
                t(0),
                false,
            )
        };
        {
            let mut st = st.lock().unwrap();
            st.push(chat_id, reply.clone());
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
                    .send(Event::MessageDeleted {
                        chat_id,
                        msg_ids: vec![reply_id],
                    })
                    .await;
            }
            Demo::EditReply => {
                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                let edited = {
                    let mut st = st.lock().unwrap();
                    st.versions
                        .entry((chat_id, reply_id))
                        .or_default()
                        .push(MsgVersion {
                            text: reply.text.clone(),
                            replaced_at: t(0),
                        });
                    let mut e = reply.clone();
                    e.text = "(mock reply) got it — actually, make that thursday".into();
                    e.markdown = to_markdown(&e.text, &e.spans);
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
