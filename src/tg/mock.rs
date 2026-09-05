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
    AuthState, BackendFlags, BotCommand, ButtonKind, ChatInfo, ChatKind, ChatSummary, Command, Contact,
    ContactCard, DiceInfo, Event, Folder, GeoPoint, Gif, KeyButton, Keyboard, LiveLocation, LocationInfo,
    CallDevice, CallDevices, CallEndReason, CallInfo, CallPhase, Me, MediaKind, Member, MemberRole, Msg, MsgVersion,
    MuteMode, Poll, PollOption, Presence, Reaction,
    SharedKind, Span, SpanKind, Sticker, StickerPack, Story, StoryPeer, StoryRing, Topic, WebPreview,
    parse_markdown, paths, reject, split_topic_chat_id, to_markdown, topic_chat_id,
};

pub const ME_ID: i64 = 424242;
/// Mock forum supergroup (wave 6E); a channel-style id so topic ids encode.
pub const FORUM_ID: i64 = -1_001_000_000_011;
const MEDIA_LAB: i64 = 8;
const POLLS: i64 = 9;
const BOT: i64 = 7007;
const HELPER_BOT: i64 = 7008;
const ALEX: i64 = 5007;

/// Bundled minimal Lottie sticker (a spinning flame), gzip-compressed like a real .tgs.
const FIRE_TGS: &[u8] = include_bytes!("fixtures/fire.tgs");

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty())
}

/// Playable mock media, rendered on first use with ffmpeg into the media
/// cache (`mock-voice.ogg`, `mock-music.ogg`, `mock-video.mp4`,
/// `mock-note.mp4`, `mock-gif.mp4`). None without ffmpeg (or with
/// OMG_MOCK_NO_FFMPEG=1), which the UI must show as "unavailable".
async fn mock_media(name: &str) -> Option<PathBuf> {
    if env_flag("OMG_MOCK_NO_FFMPEG") {
        return None;
    }
    let dir = paths::media_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("mock-{name}"));
    if path.exists() {
        return Some(path);
    }
    let (muxer, args): (&str, Vec<&str>) = match name {
        "voice.ogg" => ("ogg", vec!["-f", "lavfi", "-i", "sine=frequency=440:duration=3", "-c:a", "libopus", "-b:a", "32k"]),
        "music.ogg" => (
            "ogg",
            vec![
                "-f", "lavfi", "-i", "sine=frequency=330:duration=3", "-metadata", "title=Night Drive", "-metadata",
                "artist=Marta", "-c:a", "libopus", "-b:a", "48k",
            ],
        ),
        "video.mp4" => (
            "mp4",
            vec![
                "-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=15", "-f", "lavfi", "-i",
                "sine=frequency=440:duration=3", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
            ],
        ),
        "note.mp4" => (
            "mp4",
            vec![
                "-f", "lavfi", "-i", "testsrc=duration=3:size=240x240:rate=15", "-f", "lavfi", "-i",
                "sine=frequency=520:duration=3", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
            ],
        ),
        "gif.mp4" => (
            "mp4",
            vec!["-f", "lavfi", "-i", "testsrc=duration=2:size=200x150:rate=10", "-an", "-c:v", "libx264", "-pix_fmt", "yuv420p"],
        ),
        _ => return None,
    };
    let tmp = dir.join(format!("mock-{name}.{}.tmp", std::process::id()));
    let run = tokio::process::Command::new("ffmpeg")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .args(&args)
        .arg("-f")
        .arg(muxer)
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let ok = matches!(tokio::time::timeout(std::time::Duration::from_secs(30), run).await, Ok(Ok(st)) if st.success());
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return path.exists().then_some(path);
    }
    Some(path)
}

/// The bundled animated sticker, written to the media cache.
fn mock_tgs() -> Option<PathBuf> {
    let dir = paths::media_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("mock-fire.tgs");
    if !path.exists() {
        std::fs::write(&path, FIRE_TGS).ok()?;
    }
    Some(path)
}

/// A synthetic "map tile": neutral ground, a street grid offset by the
/// coordinates (so neighbouring grid cells differ), a center marker.
fn mock_map(point: GeoPoint, zoom: u8, width: u32, height: u32, marker: bool) -> Option<PathBuf> {
    let dir = paths::media_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let key = format!("mock-map-{:.4}_{:.4}_{zoom}_{width}x{height}{}.png", point.lat, point.lon, if marker { "" } else { "-tile" });
    let path = dir.join(key);
    if path.exists() {
        return Some(path);
    }
    let width = width.clamp(16, 2048);
    let height = height.clamp(16, 2048);
    let ox = ((point.lat.abs() * 1000.0) as u32) % 40;
    let oy = ((point.lon.abs() * 1000.0) as u32) % 40;
    let mut img = image::RgbaImage::from_pixel(width, height, image::Rgba([0xe6, 0xe2, 0xd8, 0xff]));
    for y in 0..height {
        for x in 0..width {
            let street = (x + ox) % 40 < 3 || (y + oy) % 40 < 3;
            let avenue = (x + ox) % 120 < 6 || (y + oy) % 120 < 6;
            if avenue {
                img.put_pixel(x, y, image::Rgba([0xf6, 0xd9, 0x8a, 0xff]));
            } else if street {
                img.put_pixel(x, y, image::Rgba([0xff, 0xff, 0xff, 0xff]));
            }
        }
    }
    let (cx, cy) = (width as i32 / 2, height as i32 / 2);
    for y in (cy - 8).max(0)..(cy + 8).min(height as i32) {
        if !marker {
            break;
        }
        for x in (cx - 8).max(0)..(cx + 8).min(width as i32) {
            let d2 = (x - cx).pow(2) + (y - cy).pow(2);
            if d2 <= 36 {
                img.put_pixel(x as u32, y as u32, image::Rgba([0xd0, 0x3a, 0x2f, 0xff]));
            } else if d2 <= 64 {
                img.put_pixel(x as u32, y as u32, image::Rgba([0xff, 0xff, 0xff, 0xff]));
            }
        }
    }
    img.save(&path).ok()?;
    Some(path)
}

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
    forum: bool,
    story_ring: StoryRing,
    bot_commands: Vec<BotCommand>,
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
        forum: false,
        story_ring: StoryRing::None,
        bot_commands: Vec::new(),
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
    cached_chats: HashSet<i64>,
    // ----- wave 6 -----
    /// Scheduled messages per chat (`Msg::scheduled`, `ts` = send time).
    scheduled: HashMap<i64, Vec<Msg>>,
    /// Forum topics per forum id (histories live under the topic's synthetic chat id).
    topics: HashMap<i64, Vec<Topic>>,
    /// Stories per peer, oldest first.
    stories: HashMap<i64, Vec<Story>>,
    /// Quiz answer keys: poll id -> correct option index.
    quiz_correct: HashMap<i64, usize>,
    // ----- wave 7 -----
    /// The one active/ringing call, if any.
    call: Option<CallInfo>,
    /// Bumped on every new call so a stale advance task can't touch a newer one.
    call_gen: u64,
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

        let mut c = chat(MEDIA_LAB, "Media Lab", ChatKind::Group);
        c.members = Some(3);
        c.about = "Every media kind Telegram knows, for the eyes.".into();
        chats.insert(MEDIA_LAB, c);
        let mut c = chat(POLLS, "Polls", ChatKind::Group);
        c.members = Some(12);
        chats.insert(POLLS, c);
        let mut c = chat(BOT, "Omarchy Bot", ChatKind::Bot);
        c.username = "omarchy_bot".into();
        c.about = "Controls the desktop (mock).".into();
        c.bot_commands = vec![
            BotCommand { command: "start".into(), description: "Start the bot".into() },
            BotCommand { command: "help".into(), description: "List what I can do".into() },
            BotCommand { command: "theme".into(), description: "Switch the Omarchy theme".into() },
            BotCommand { command: "screenshot".into(), description: "Take a screenshot".into() },
        ];
        chats.insert(BOT, c);
        let mut c = chat(HELPER_BOT, "Helper Bot", ChatKind::Bot);
        c.username = "omarchy_helper_bot".into();
        c.bot_commands = vec![
            BotCommand { command: "start".into(), description: "Start the bot".into() },
            BotCommand { command: "help".into(), description: "Show help".into() },
        ];
        chats.insert(HELPER_BOT, c);
        let mut c = chat(FORUM_ID, "Omarchy Forum", ChatKind::Group);
        c.forum = true;
        c.members = Some(240);
        c.username = "omarchy_forum".into();
        c.about = "Themes, bugs and everything else — in topics.".into();
        chats.insert(FORUM_ID, c);
        if let Some(c) = chats.get_mut(&1) {
            c.story_ring = StoryRing::Unread;
        }

        let mut history = HashMap::new();
        history.insert(MEDIA_LAB, media_lab_messages());
        history.insert(POLLS, poll_messages());
        history.insert(BOT, bot_messages());
        history.insert(HELPER_BOT, Vec::new());
        let (topics, topic_histories) = forum_fixture();
        for (chat_id, msgs) in topic_histories {
            history.insert(chat_id, msgs);
        }
        let mut topics_map = HashMap::new();
        topics_map.insert(FORUM_ID, topics);
        let mut quiz_correct = HashMap::new();
        quiz_correct.insert(9002, 1);
        let mut scheduled = HashMap::new();
        scheduled.insert(
            1,
            vec![
                Msg {
                    scheduled: true,
                    ..msg(5001, 1, "Marta", "You", "morning! don't forget the ridge photos", tomorrow_at(9, 0), true)
                },
                Msg {
                    scheduled: true,
                    ..msg(5002, 1, "Marta", "You", "see you at the gallery", Local::now() + Duration::days(3) - Duration::hours(Local::now().format("%H").to_string().parse::<i64>().unwrap_or(0)) + Duration::hours(18) + Duration::minutes(30) - Duration::minutes(Local::now().format("%M").to_string().parse::<i64>().unwrap_or(0)), true)
                },
            ],
        );
        let mut stories = HashMap::new();
        stories.insert(
            1,
            vec![
                Story { id: 1, chat_id: 1, ts: t(180), expires: Local::now() + Duration::hours(21), video: false, duration: None, caption: "fog on the ridge".into(), seen: false },
                Story { id: 2, chat_id: 1, ts: t(60), expires: Local::now() + Duration::hours(23), video: true, duration: Some(3), caption: String::new(), seen: false },
            ],
        );
        stories.insert(
            ALEX,
            vec![Story { id: 1, chat_id: ALEX, ts: t(600), expires: Local::now() + Duration::hours(14), video: false, duration: None, caption: "new build farm".into(), seen: true }],
        );
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
            story_ring: StoryRing::None,
            },
            Contact {
                user_id: 2,
                name: "Deni".into(),
                username: "deni".into(),
                phone: String::new(),
                presence: Presence::LastSeen(t(140)),
                has_photo: false,
            story_ring: StoryRing::None,
            },
            Contact {
                user_id: 3,
                name: "Mom".into(),
                username: String::new(),
                phone: "+1 555 0101".into(),
                presence: Presence::LastWeek,
                has_photo: false,
            story_ring: StoryRing::None,
            },
            Contact {
                user_id: 5005,
                name: "Sam Rivera".into(),
                username: "samr".into(),
                phone: "+1 555 0177".into(),
                presence: Presence::Recently,
                has_photo: false,
            story_ring: StoryRing::None,
            },
            Contact {
                user_id: ALEX,
                name: "Alex Petrov".into(),
                username: "alexp".into(),
                phone: "+7 900 000 0000".into(),
                presence: Presence::Recently,
                has_photo: false,
                story_ring: StoryRing::Read,
            },
            Contact {
                user_id: 5006,
                name: "Yuki Tanaka".into(),
                username: String::new(),
                phone: "+81 90 0000 0000".into(),
                presence: Presence::LastMonth,
                has_photo: false,
            story_ring: StoryRing::None,
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
            unread: HashMap::from([
                (1, 1),
                (2, 0),
                (3, 0),
                (4, 3),
                (5, 0),
                (ME_ID, 0),
                (7, 0),
                (MEDIA_LAB, 2),
                (topic_chat_id(FORUM_ID, 20), 3),
            ]),
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
            cached_chats: HashSet::new(),
            call: None,
            call_gen: 0,
            scheduled,
            topics: topics_map,
            stories,
            quiz_correct,
        }
    }

    /// Messages of a chat, or of every topic when `chat_id` is a forum.
    fn messages_of(&self, chat_id: i64) -> Vec<Msg> {
        if let Some(topics) = self.topics.get(&chat_id) {
            let mut all: Vec<Msg> = topics
                .iter()
                .filter_map(|t| self.history.get(&t.chat_id))
                .flatten()
                .cloned()
                .collect();
            all.sort_by_key(|m| (m.ts, m.id));
            return all;
        }
        self.history.get(&chat_id).cloned().unwrap_or_default()
    }

    fn find_mut(&mut self, chat_id: i64, msg_id: i32) -> Option<&mut Msg> {
        let key = if self.topics.contains_key(&chat_id) {
            self.topics[&chat_id]
                .iter()
                .map(|t| t.chat_id)
                .find(|id| self.history.get(id).is_some_and(|v| v.iter().any(|m| m.id == msg_id)))?
        } else {
            chat_id
        };
        self.history.get_mut(&key)?.iter_mut().find(|m| m.id == msg_id)
    }

    fn topics_of(&self, forum_id: i64) -> Vec<Topic> {
        let mut out: Vec<Topic> = self
            .topics
            .get(&forum_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut t| {
                let last = self.history.get(&t.chat_id).and_then(|v| v.last());
                t.last_message = last.map(MockState::preview).unwrap_or_default();
                t.last_time = last.map(|m| m.ts);
                t.unread = *self.unread.get(&t.chat_id).unwrap_or(&0);
                t
            })
            .collect();
        out.sort_by_key(|t| (std::cmp::Reverse(t.pinned), std::cmp::Reverse(t.last_time)));
        out
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
            Some(MediaKind::Location) => "[location]".into(),
            Some(MediaKind::Venue) => "[venue]".into(),
            Some(MediaKind::Contact) => "[contact]".into(),
            Some(MediaKind::Dice) => format!("{} [dice]", m.dice.as_ref().map(|d| d.emoji.as_str()).unwrap_or("")).trim().to_string(),
            Some(MediaKind::Poll) => format!("[poll] {}", m.poll.as_ref().map(|p| p.question.as_str()).unwrap_or("")).trim().to_string(),
            Some(MediaKind::Unsupported) => "[unsupported]".into(),
            None => String::new(),
        }
    }

    fn chat_title(&self, chat_id: i64) -> String {
        let chat_id = split_topic_chat_id(chat_id).map(|(f, _)| f).unwrap_or(chat_id);
        self.chats
            .get(&chat_id)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| "Someone".into())
    }

    fn summary(&self, c: &MockChat) -> ChatSummary {
        let forum_msgs = c.forum.then(|| self.messages_of(c.id));
        let msgs = if c.forum { forum_msgs.as_ref() } else { self.history.get(&c.id) };
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
            unread: if c.forum {
                self.topics.get(&c.id).map(|ts| ts.iter().map(|t| *self.unread.get(&t.chat_id).unwrap_or(&0)).sum()).unwrap_or(0)
            } else {
                *self.unread.get(&c.id).unwrap_or(&0)
            },
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
            forum: c.forum,
            story_ring: c.story_ring,
        }
    }

    fn find(&self, chat_id: i64, msg_id: i32) -> Option<Msg> {
        if self.topics.contains_key(&chat_id) {
            return self.messages_of(chat_id).into_iter().find(|m| m.id == msg_id);
        }
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

    /// Appends to a chat; messages of a topic get the forum's chat id and
    /// their `topic_id` (they arrive that way from the real backend too).
    fn push(&mut self, chat_id: i64, mut m: Msg) -> Msg {
        if let Some((forum_id, topic_id)) = split_topic_chat_id(chat_id) {
            m.chat_id = forum_id;
            m.topic_id = Some(topic_id);
            m.chat_title = self.chat_title(forum_id);
        }
        self.history.entry(chat_id).or_default().push(m.clone());
        m
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
    let mut state = MockState::new();
    if env_flag("OMG_MOCK_LONG_PIN")
        && let Some(message) = state.history.get_mut(&5).and_then(|messages| messages.iter_mut().find(|m| m.id == 501)) {
            message.text = (1..=40).map(|n| format!("Line {n}: This pinned announcement contains details that remain available when expanded.")).collect::<Vec<_>>().join("\n");
            message.markdown = message.text.clone();
    }
    let st = Arc::new(Mutex::new(state));

    // OMG_MOCK_LIVE_MS=<ms>: after a 4 s grace period, Marta types for ~1.4 s
    // and sends a short message every <ms> — demo recordings use it to show
    // the typing indicator, message-reveal and badge animations.
    if let Some(every) = std::env::var("OMG_MOCK_LIVE_MS").ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|ms| *ms >= 500) {
        let st = st.clone();
        let events = events.clone();
        tokio::spawn(async move {
            const LINES: [&str; 6] = [
                "fog lifted, the ridge is clear now",
                "sending the photos in a minute",
                "does the new theme look right on your side?",
                "ok — switching to gruvbox then",
                "see you at six",
                "bring the wide lens",
            ];
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            for (i, line) in LINES.iter().cycle().enumerate() {
                if events.send(Event::Typing { chat_id: 1, name: "Marta".to_string() }).await.is_err() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1400)).await;
                let m = {
                    let mut st = st.lock().unwrap();
                    st.next_id += 1;
                    let id = st.next_id;
                    let m = msg(id, 1, "Marta", "Marta", line, Local::now(), false);
                    st.push(1, m)
                };
                if events.send(Event::NewMessage(m)).await.is_err() {
                    return;
                }
                let _ = i;
                tokio::time::sleep(std::time::Duration::from_millis(every.saturating_sub(1400).max(200))).await;
            }
        });
    }

    // Wave 7: OMG_MOCK_INCOMING_CALL=<seconds> makes Marta call us that many
    // seconds after start (0 = immediately). Drives the incoming-call UI/probe.
    if let Some(secs) = std::env::var("OMG_MOCK_INCOMING_CALL").ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        let st = st.clone();
        let events = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            let info = {
                let mut st = st.lock().unwrap();
                if st.call.as_ref().is_some_and(|c| c.phase != CallPhase::Ended) {
                    return;
                }
                st.call_gen += 1;
                let name = st.chat_title(1);
                st.call = Some(CallInfo {
                    id: 7_100_000 + st.call_gen as i64,
                    peer_id: 1,
                    peer_name: name,
                    outgoing: false,
                    phase: CallPhase::Incoming,
                    muted: false,
                    emojis: String::new(),
                    connected_at: None,
                    end_reason: None,
                    error: None,
                });
                st.call.clone().unwrap()
            };
            let _ = events.send(Event::CallChanged(info)).await;
        });
    }

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
        if matches!(cmd, Command::Shutdown) { break; }
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
            | Command::GetCachedHistory { .. }
            | Command::SubmitCredentials { .. }
            | Command::SubmitPhone(..)
            | Command::SubmitCode(..)
            | Command::SubmitPassword(..)
    )
        && let Some(ms) = std::env::var("OMG_MOCK_LATENCY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
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
        Command::Shutdown => {},
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
        Command::SetOnline(_, respond) => { let _ = respond.send(Ok(())); }
        Command::GetCachedHistory { chat_id, respond } => {
            let state = st.lock().unwrap();
            let _ = respond.send(Ok(if state.cached_chats.contains(&chat_id) { state.messages_of(chat_id) } else { Vec::new() }));
        }
        Command::GetHistory {
            chat_id,
            before_id,
            respond,
        } => {
            let mut st = st.lock().unwrap();
            if before_id.is_none() { st.cached_chats.insert(chat_id); }
            let msgs = st.messages_of(chat_id);
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
            let (media, doc_name, sent_path, point) = {
                let st = st.lock().unwrap();
                let found = st.find(chat_id, msg_id);
                (
                    found.as_ref().and_then(|m| m.media),
                    found.as_ref().and_then(|m| m.doc_name.clone()),
                    st.sent_files.get(&msg_id).cloned(),
                    found.as_ref().and_then(|m| m.location.as_ref()).map(|l| l.point),
                )
            };
            tokio::spawn(async move {
                // Simulate network so loading placeholders are visible.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let path = if let Some(p) = sent_path {
                    Some(p)
                } else {
                    match media {
                        Some(MediaKind::Sticker) if doc_name.as_deref() == Some("fire.tgs") => mock_tgs(),
                        Some(MediaKind::Photo | MediaKind::Sticker) => sample_image_n(msg_id as usize),
                        Some(MediaKind::Document) => sample_document(doc_name.as_deref().unwrap_or("file.txt")),
                        Some(MediaKind::Voice) => mock_media("voice.ogg").await,
                        Some(MediaKind::Audio) => mock_media("music.ogg").await,
                        Some(MediaKind::Video) => mock_media("video.mp4").await,
                        Some(MediaKind::VideoNote) => mock_media("note.mp4").await,
                        Some(MediaKind::Gif) => mock_media("gif.mp4").await,
                        Some(MediaKind::Location | MediaKind::Venue) => {
                            point.and_then(|p| mock_map(p, 15, 320, 180, true))
                        }
                        // Contact, dice and polls have no file behind them.
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
                let sent = st.push(chat_id, sent);
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

                st.push(chat_id, sent)
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

                st.push(chat_id, sent)
            };
            schedule_read(st.clone(), &events, chat_id, sent.id);
            let _ = respond.send(Ok(sent));
        }
        Command::SendSticker {
            chat_id,
            sticker_id,
            respond,
        } => {
            // Animated stickers are sendable since wave 6 (the UI renders them via 6D).
            let r = {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let title = st.chat_title(chat_id);
                let sent = Msg {
                    media: Some(MediaKind::Sticker),
                    sticker_emoji: Some(sticker_emoji(sticker_id).into()),
                    ..msg(st.next_id, chat_id, &title, "You", "", t(0), true)
                };
                let sent = st.push(chat_id, sent);
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
            let sent = st.push(chat_id, sent);
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
                    let copy = st.push(to_chat, copy);
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
                if !st.chats.contains_key(&BOT) && (contains_ci("omarchy_bot", q) || contains_ci("Omarchy Bot", q)) {
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
            if let Some((forum, topic_id)) = split_topic_chat_id(chat_id) {
                let ok = st.lock().unwrap().topics.get_mut(&forum)
                    .and_then(|topics| topics.iter_mut().find(|topic| topic.id == topic_id))
                    .map(|topic| topic.pinned = pinned).is_some();
                let _ = respond.send(if ok { Ok(()) } else { Err("Topic unavailable".into()) });
                let _ = events.send(Event::TopicsChanged { forum_id: forum }).await;
                return;
            }
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
            if let Some((forum, topic_id)) = split_topic_chat_id(chat_id) {
                let ok = st.lock().unwrap().topics.get_mut(&forum)
                    .and_then(|topics| topics.iter_mut().find(|topic| topic.id == topic_id))
                    .map(|topic| topic.muted = mode != MuteMode::Unmute).is_some();
                let _ = respond.send(if ok { Ok(()) } else { Err("Topic unavailable".into()) });
                let _ = events.send(Event::TopicsChanged { forum_id: forum }).await;
                return;
            }
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
            if split_topic_chat_id(chat_id).is_some() {
                let _ = respond.send(Err("This action is available for the whole forum only".into()));
                return;
            }
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
            if split_topic_chat_id(chat_id).is_some() {
                let _ = respond.send(Err("This action is available for the whole forum only".into()));
                return;
            }
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
            if let Some((forum, topic)) = split_topic_chat_id(chat_id) {
                if topic == 1 { let _ = respond.send(Err("General cannot be deleted".into())); return; }
                if let Some(topics) = st.lock().unwrap().topics.get_mut(&forum) { topics.retain(|t| t.id != topic); }
                let _ = events.send(Event::TopicsChanged { forum_id: forum }).await;
            }
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
            if let Some((forum, topic_id)) = split_topic_chat_id(chat_id) {
                let ok = st.lock().unwrap().topics.get_mut(&forum)
                    .and_then(|topics| topics.iter_mut().find(|t| t.id == topic_id))
                    .map(|t| { t.draft = text.clone(); t.draft_reply_to = reply_to; }).is_some();
                let _ = respond.send(if ok { Ok(()) } else { Err("Topic unavailable".into()) });
                return;
            }
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
                    bot_commands: c.bot_commands.clone(),
                    forum: c.forum,
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
                    video: false,
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
                    mock_tgs()
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
            let _wanted = gif_id;
            tokio::spawn(async move {
                let _ = respond.send(Ok(mock_media("gif.mp4").await));
            });
        }
        // ===================== wave 6 =====================
        Command::DownloadMap { point, zoom, width, height, marker, respond } => {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                let _ = respond.send(Ok(mock_map(point, zoom, width, height, marker)));
            });
        }
        Command::SendVote { chat_id, msg_id, options, respond } => {
            let (r, changed) = {
                let mut st = st.lock().unwrap();
                let quiz_keys = st.quiz_correct.clone();
                match st.find_mut(chat_id, msg_id).and_then(|m| m.poll.as_mut()) {
                    None => (Err("not a poll".to_string()), None),
                    Some(poll) if poll.closed => (Err("this poll is closed".to_string()), None),
                    Some(poll) => {
                        if options.is_empty() {
                            if poll.quiz && poll.voted {
                                (Err("quiz answers can't be retracted".to_string()), None)
                            } else {
                                for o in poll.options.iter_mut() {
                                    if o.chosen {
                                        o.chosen = false;
                                        o.voters = (o.voters - 1).max(0);
                                    }
                                    o.correct = None;
                                }
                                if poll.voted {
                                    poll.total_voters = (poll.total_voters - 1).max(0);
                                }
                                poll.voted = false;
                                (Ok(()), Some(poll.clone()))
                            }
                        } else if poll.voted {
                            (Err("already voted".to_string()), None)
                        } else if options.iter().any(|&i| i >= poll.options.len()) {
                            (Err("no such option".to_string()), None)
                        } else if !poll.multiple_choice && options.len() > 1 {
                            (Err("this poll allows one answer".to_string()), None)
                        } else {
                            for &i in &options {
                                poll.options[i].chosen = true;
                                poll.options[i].voters += 1;
                            }
                            poll.total_voters += 1;
                            poll.voted = true;
                            if poll.quiz {
                                let key = quiz_keys.get(&poll.id).copied().unwrap_or(0);
                                for (i, o) in poll.options.iter_mut().enumerate() {
                                    o.correct = Some(i == key);
                                }
                            }
                            (Ok(()), Some(poll.clone()))
                        }
                    }
                }
            };
            let _ = respond.send(r);
            if let Some(poll) = changed {
                let events = events.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let _ = events.send(Event::PollChanged { poll_id: poll.id, poll }).await;
                });
            }
        }
        Command::AddContact { user_id, first_name, last_name, phone, respond } => {
            let mut st = st.lock().unwrap();
            let name = format!("{first_name} {last_name}").trim().to_string();
            if name.is_empty() {
                let _ = respond.send(Err("a name is required".to_string()));
                return;
            }
            if let Some(c) = st.contacts.iter_mut().find(|c| c.user_id == user_id) {
                c.name = name;
            } else {
                st.contacts.push(Contact {
                    user_id,
                    name,
                    username: String::new(),
                    phone,
                    presence: Presence::Unknown,
                    has_photo: false,
                    story_ring: StoryRing::None,
                });
            }
            if let Some(c) = st.chats.get_mut(&user_id) {
                c.is_contact = true;
            }
            let _ = respond.send(Ok(()));
        }
        Command::SendPoll { chat_id, draft, respond } => {
            let r = {
                let mut st = st.lock().unwrap();
                let n = draft.options.iter().filter(|o| !o.trim().is_empty()).count();
                if draft.question.trim().is_empty() {
                    Err("the poll needs a question".to_string())
                } else if !(2..=10).contains(&n) {
                    Err("a poll needs 2 to 10 options".to_string())
                } else if draft.quiz && draft.correct_option.is_none_or(|i| i >= n) {
                    Err("a quiz needs a correct answer".to_string())
                } else {
                    st.next_id += 1;
                    let id = st.next_id;
                    let poll_id = 9_000_000 + id as i64;
                    if let Some(i) = draft.correct_option.filter(|_| draft.quiz) {
                        st.quiz_correct.insert(poll_id, i);
                    }
                    let title = st.chat_title(chat_id);
                    let sent = Msg {
                        media: Some(MediaKind::Poll),
                        poll: Some(Poll {
                            id: poll_id,
                            question: draft.question.trim().to_string(),
                            options: draft
                                .options
                                .iter()
                                .filter(|o| !o.trim().is_empty())
                                .map(|o| PollOption { text: o.trim().to_string(), ..PollOption::default() })
                                .collect(),
                            total_voters: 0,
                            closed: false,
                            public_voters: !draft.anonymous,
                            multiple_choice: draft.multiple_choice && !draft.quiz,
                            quiz: draft.quiz,
                            voted: false,
                            solution: draft.solution.filter(|s| !s.trim().is_empty()),
                            close_date: None,
                        }),
                        ..msg(id, chat_id, &title, "You", "", t(0), true)
                    };
                    let sent = st.push(chat_id, sent);
                    Ok(sent)
                }
            };
            let _ = respond.send(r);
        }
        Command::SendLocation { chat_id, point, respond } => {
            let mut st = st.lock().unwrap();
            st.next_id += 1;
            let title = st.chat_title(chat_id);
            let sent = Msg {
                media: Some(MediaKind::Location),
                location: Some(LocationInfo { point, ..LocationInfo::default() }),
                ..msg(st.next_id, chat_id, &title, "You", "", t(0), true)
            };
            let sent = st.push(chat_id, sent);
            let _ = respond.send(Ok(sent));
        }
        Command::SendTextAt { chat_id, text, reply_to, at, respond } => {
            if at <= Local::now() {
                let _ = respond.send(Err("the time is in the past".to_string()));
                return;
            }
            {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let title = st.chat_title(chat_id);
                let m = Msg { reply_to, scheduled: true, ..fmt_msg(st.next_id, chat_id, &title, "You", &text, at, true) };
                st.scheduled.entry(chat_id).or_default().push(m);
            }
            let _ = respond.send(Ok(()));
            let _ = events.send(Event::ScheduledChanged { chat_id }).await;
        }
        Command::SendFileAt { chat_id, path, caption, at, respond } => {
            if at <= Local::now() {
                let _ = respond.send(Err("the time is in the past".to_string()));
                return;
            }
            {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let title = st.chat_title(chat_id);
                let m = Msg {
                    media: Some(MediaKind::Document),
                    doc_name: Some(name),
                    doc_size: std::fs::metadata(&path).map(|m| m.len()).ok(),
                    scheduled: true,
                    ..fmt_msg(st.next_id, chat_id, &title, "You", &caption, at, true)
                };
                st.sent_files.insert(m.id, path);
                st.scheduled.entry(chat_id).or_default().push(m);
            }
            let _ = respond.send(Ok(()));
            let _ = events.send(Event::ScheduledChanged { chat_id }).await;
        }
        Command::GetScheduled { chat_id, respond } => {
            let st = st.lock().unwrap();
            let mut list = st.scheduled.get(&chat_id).cloned().unwrap_or_default();
            list.sort_by_key(|m| m.ts);
            let _ = respond.send(Ok(list));
        }
        Command::SendScheduledNow { chat_id, ids, respond } => {
            let sent: Vec<Msg> = {
                let mut st = st.lock().unwrap();
                let mut out = Vec::new();
                let mut taken = Vec::new();
                if let Some(list) = st.scheduled.get_mut(&chat_id) {
                    let mut keep = Vec::new();
                    for m in list.drain(..) {
                        if ids.contains(&m.id) {
                            taken.push(m);
                        } else {
                            keep.push(m);
                        }
                    }
                    *list = keep;
                }
                for mut m in taken {
                    let old = m.id;
                    st.next_id += 1;
                    m.id = st.next_id;
                    m.scheduled = false;
                    m.ts = Local::now();
                    if let Some(p) = st.sent_files.remove(&old) {
                        st.sent_files.insert(m.id, p);
                    }
                    let m = st.push(chat_id, m);
                    out.push(st.find(chat_id, m.id).unwrap_or(m));
                }
                out
            };
            let _ = respond.send(Ok(()));
            for m in sent {
                let _ = events.send(Event::NewMessage(m)).await;
            }
            let _ = events.send(Event::ScheduledChanged { chat_id }).await;
        }
        Command::DeleteScheduled { chat_id, ids, respond } => {
            {
                let mut st = st.lock().unwrap();
                if let Some(list) = st.scheduled.get_mut(&chat_id) {
                    list.retain(|m| !ids.contains(&m.id));
                }
            }
            let _ = respond.send(Ok(()));
            let _ = events.send(Event::ScheduledChanged { chat_id }).await;
        }
        Command::PressButton { chat_id, msg_id, data, respond } => {
            let (r, changed) = {
                let mut st = st.lock().unwrap();
                match st.find_mut(chat_id, msg_id) {
                    None => (Err("message not found".to_string()), None),
                    Some(m) => match data.as_slice() {
                        b"lock" => (Ok(Some("Locked (mock)".to_string())), None),
                        b"next_theme" => {
                            if let Some(first) = m.keyboard.as_mut().and_then(|k| k.rows.first_mut()).and_then(|r| r.first_mut()) {
                                first.text = "Next theme ✓".to_string();
                            }
                            (Ok(None), Some(m.clone()))
                        }
                        _ => (Ok(None), None),
                    },
                }
            };
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let _ = respond.send(r);
            if let Some(m) = changed {
                let _ = events.send(Event::MessageChanged(m)).await;
            }
        }
        Command::GetTopics { forum_id, respond } => {
            let st = st.lock().unwrap();
            let r = if st.topics.contains_key(&forum_id) {
                Ok(st.topics_of(forum_id))
            } else {
                Err("not a forum".to_string())
            };
            let _ = respond.send(r);
        }
        Command::CreateTopic { forum_id, title, respond } => {
            let r = {
                let mut st = st.lock().unwrap();
                if title.trim().is_empty() {
                    Err("the topic needs a title".to_string())
                } else if !st.topics.contains_key(&forum_id) {
                    Err("not a forum".to_string())
                } else {
                    st.next_id += 1;
                    let id = st.next_id;
                    let topic = Topic {
                        id,
                        chat_id: topic_chat_id(forum_id, id),
                        forum_id,
                        title: title.trim().to_string(),
                        icon_emoji: String::new(),
                        unread: 0,
                        last_message: String::new(),
                        last_time: Some(Local::now()),
                        pinned: false,
                        closed: false,
                        ..Topic::default()
                    };
                    st.history.entry(topic.chat_id).or_default();
                    st.topics.entry(forum_id).or_default().push(topic.clone());
                    Ok(topic)
                }
            };
            let ok = r.is_ok();
            let _ = respond.send(r);
            if ok {
                let _ = events.send(Event::TopicsChanged { forum_id }).await;
            }
        }
        Command::SendVideoNote { chat_id, path, duration, size, respond } => {
            let sent = {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let title = st.chat_title(chat_id);
                let sent = Msg {
                    media: Some(MediaKind::VideoNote),
                    duration: Some(duration),
                    photo_size: Some((size as i32, size as i32)),
                    round: true,
                    doc_size: std::fs::metadata(&path).map(|m| m.len()).ok(),
                    ..msg(st.next_id, chat_id, &title, "You", "", t(0), true)
                };
                st.sent_files.insert(sent.id, path);

                st.push(chat_id, sent)
            };
            schedule_read(st.clone(), &events, chat_id, sent.id);
            let _ = respond.send(Ok(sent));
        }
        Command::SendLiveLocation { chat_id, point, period_secs, respond } => {
            let mut st = st.lock().unwrap();
            st.next_id += 1;
            let title = st.chat_title(chat_id);
            let now = Local::now();
            let sent = Msg {
                media: Some(MediaKind::Location),
                location: Some(LocationInfo {
                    point,
                    live: Some(LiveLocation {
                        period_secs,
                        expires: now + Duration::seconds(period_secs as i64),
                        last_update: now,
                        heading: None,
                        stopped: false,
                    }),
                    ..LocationInfo::default()
                }),
                ..msg(st.next_id, chat_id, &title, "You", "", t(0), true)
            };
            let sent = st.push(chat_id, sent);
            let _ = respond.send(Ok(sent));
        }
        Command::UpdateLiveLocation { chat_id, msg_id, point, respond } => {
            let changed = {
                let mut st = st.lock().unwrap();
                match st.find_mut(chat_id, msg_id).and_then(|m| m.location.as_mut().map(|l| (l, m.id))) {
                    Some((l, _)) if l.live.as_ref().is_some_and(|live| !live.stopped) => {
                        l.point = point;
                        if let Some(live) = l.live.as_mut() {
                            live.last_update = Local::now();
                        }
                        Ok(())
                    }
                    Some(_) => Err("this location is not live".to_string()),
                    None => Err("message not found".to_string()),
                }
            };
            let ok = changed.is_ok();
            let _ = respond.send(changed);
            if ok {
                let m = st.lock().unwrap().find(chat_id, msg_id);
                if let Some(m) = m {
                    let _ = events.send(Event::MessageChanged(m)).await;
                }
            }
        }
        Command::StopLiveLocation { chat_id, msg_id, respond } => {
            let r = {
                let mut st = st.lock().unwrap();
                match st.find_mut(chat_id, msg_id).and_then(|m| m.location.as_mut()).and_then(|l| l.live.as_mut()) {
                    Some(live) => {
                        live.stopped = true;
                        Ok(())
                    }
                    None => Err("this location is not live".to_string()),
                }
            };
            let ok = r.is_ok();
            let _ = respond.send(r);
            if ok {
                let m = st.lock().unwrap().find(chat_id, msg_id);
                if let Some(m) = m {
                    let _ = events.send(Event::MessageChanged(m)).await;
                }
            }
        }
        Command::GetStoryPeers(tx) => {
            let st = st.lock().unwrap();
            let mut peers: Vec<StoryPeer> = st
                .stories
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(&chat_id, v)| {
                    let (name, has_photo) = st
                        .chats
                        .get(&chat_id)
                        .map(|c| (c.title.clone(), c.has_photo))
                        .or_else(|| st.contacts.iter().find(|c| c.user_id == chat_id).map(|c| (c.name.clone(), c.has_photo)))
                        .unwrap_or_else(|| ("Someone".into(), false));
                    StoryPeer { chat_id, name, unread: v.iter().any(|s| !s.seen), has_photo }
                })
                .collect();
            peers.sort_by_key(|p| (std::cmp::Reverse(p.unread), p.name.clone()));
            let _ = tx.send(Ok(peers));
        }
        Command::GetStories { chat_id, respond } => {
            let st = st.lock().unwrap();
            let _ = respond.send(Ok(st.stories.get(&chat_id).cloned().unwrap_or_default()));
        }
        Command::DownloadStory { chat_id, story_id, respond } => {
            let video = st
                .lock()
                .unwrap()
                .stories
                .get(&chat_id)
                .and_then(|v| v.iter().find(|s| s.id == story_id))
                .map(|s| s.video);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let path = match video {
                    Some(true) => mock_media("video.mp4").await,
                    Some(false) => sample_image_n((chat_id.unsigned_abs() as usize).wrapping_add(story_id as usize)),
                    None => None,
                };
                let _ = respond.send(Ok(path));
            });
        }
        Command::MarkStoriesSeen { chat_id, up_to_id, respond } => {
            let changed = {
                let mut st = st.lock().unwrap();
                let mut changed = false;
                if let Some(v) = st.stories.get_mut(&chat_id) {
                    for s in v.iter_mut().filter(|s| s.id <= up_to_id && !s.seen) {
                        s.seen = true;
                        changed = true;
                    }
                }
                let all_seen = st.stories.get(&chat_id).is_some_and(|v| v.iter().all(|s| s.seen));
                let ring = if all_seen { StoryRing::Read } else { StoryRing::Unread };
                if let Some(c) = st.chats.get_mut(&chat_id) {
                    c.story_ring = ring;
                }
                if let Some(c) = st.contacts.iter_mut().find(|c| c.user_id == chat_id) {
                    c.story_ring = ring;
                }
                changed
            };
            let _ = respond.send(Ok(()));
            if changed {
                let _ = events.send(Event::StoriesChanged).await;
                let _ = events.send(Event::DialogsChanged).await;
            }
        }
        // ===================== wave 7: voice calls =====================
        Command::CallStart { user_id, respond } => {
            let r = {
                let mut st = st.lock().unwrap();
                if st.call.as_ref().is_some_and(|c| c.phase != CallPhase::Ended) {
                    Err("a call is already in progress".to_string())
                } else {
                    let (name, kind) = st
                        .chats
                        .get(&user_id)
                        .map(|c| (c.title.clone(), c.kind))
                        .or_else(|| st.contacts.iter().find(|c| c.user_id == user_id).map(|c| (c.name.clone(), ChatKind::User)))
                        .unwrap_or(("Someone".into(), ChatKind::User));
                    if kind == ChatKind::Bot {
                        Err("you can't call a bot".to_string())
                    } else {
                        st.call_gen += 1;
                        st.call = Some(CallInfo {
                            id: 7_000_000 + st.call_gen as i64,
                            peer_id: user_id,
                            peer_name: name,
                            outgoing: true,
                            phase: CallPhase::Requesting,
                            muted: false,
                            emojis: String::new(),
                            connected_at: None,
                            end_reason: None,
                    error: None,
                        });
                        Ok(st.call.clone().unwrap())
                    }
                }
            };
            match r {
                Ok(info) => {
                    let _ = respond.send(Ok(()));
                    let generation = st.lock().unwrap().call_gen;
                    let _ = events.send(Event::CallChanged(info)).await;
                    spawn_call_advance(st.clone(), events.clone(), generation);
                }
                Err(e) => {
                    let _ = respond.send(Err(e));
                }
            }
        }
        Command::CallAccept(tx) => {
            let advanced = {
                let mut st = st.lock().unwrap();
                match st.call.as_mut() {
                    Some(c) if c.phase == CallPhase::Incoming => {
                        c.phase = CallPhase::Exchanging;
                        Some((st.call_gen, st.call.clone().unwrap()))
                    }
                    _ => None,
                }
            };
            match advanced {
                Some((generation, info)) => {
                    let _ = tx.send(Ok(()));
                    let _ = events.send(Event::CallChanged(info)).await;
                    spawn_call_advance(st.clone(), events.clone(), generation);
                }
                None => {
                    let _ = tx.send(Err("no incoming call to accept".to_string()));
                }
            }
        }
        Command::CallHangUp(tx) => {
            let ended = {
                let mut st = st.lock().unwrap();
                st.call_gen += 1; // invalidate any in-flight advance task
                match st.call.as_mut() {
                    Some(c) if c.phase != CallPhase::Ended => {
                        c.end_reason = Some(if c.phase == CallPhase::Incoming {
                            CallEndReason::Declined
                        } else {
                            CallEndReason::Hangup
                        });
                        c.phase = CallPhase::Ended;
                        Some(st.call.clone().unwrap())
                    }
                    _ => None,
                }
            };
            let _ = tx.send(Ok(()));
            if let Some(info) = ended {
                let _ = events.send(Event::CallChanged(info)).await;
                st.lock().unwrap().call = None;
            }
        }
        Command::CallSetMuted { muted, respond } => {
            let changed = {
                let mut st = st.lock().unwrap();
                match st.call.as_mut() {
                    Some(c) if c.phase != CallPhase::Ended => {
                        c.muted = muted;
                        Some(st.call.clone().unwrap())
                    }
                    _ => None,
                }
            };
            let _ = respond.send(Ok(()));
            if let Some(info) = changed {
                let _ = events.send(Event::CallChanged(info)).await;
            }
        }
        Command::ImportLegacyArchive { respond, .. } => { let _ = respond.send(Ok(12)); }
        Command::CallSetDevices { call_id, respond, .. } => {
            let active = st.lock().unwrap().call.as_ref().is_some_and(|call| call.id == call_id && call.phase == CallPhase::Active);
            let _ = respond.send(if active { Ok(()) } else { Err("The call is no longer connected".into()) });
        }
        Command::SearchPlaces { query, respond } => {
            let _ = respond.send(Ok(if query.trim().is_empty() { Vec::new() } else { vec![super::Place { name: format!("{query}, Berlin (mock)"), point: GeoPoint { lat: 52.52, lon: 13.405 } }] }));
        }
        Command::CallDevicesList(tx) => {
            let _ = tx.send(Ok(CallDevices {
                input: vec![
                    CallDevice { id: "mock-mic-default".into(), name: "System default".into() },
                    CallDevice { id: "mock-mic-usb".into(), name: "USB microphone".into() },
                ],
                output: vec![
                    CallDevice { id: "mock-out-default".into(), name: "System default".into() },
                    CallDevice { id: "mock-out-hdmi".into(), name: "HDMI output".into() },
                ],
            }));
        }
    }
}

/// Drives an outgoing/accepted call Requesting/Exchanging -> Connecting ->
/// Active, ~700ms per step, stopping if `gen` no longer matches (hung up or a
/// newer call started).
fn spawn_call_advance(st: Arc<Mutex<MockState>>, events: async_channel::Sender<Event>, generation: u64) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            let info = {
                let mut st = st.lock().unwrap();
                if st.call_gen != generation {
                    return;
                }
                let Some(c) = st.call.as_mut() else { return };
                c.phase = match c.phase {
                    CallPhase::Requesting | CallPhase::Incoming => CallPhase::Exchanging,
                    CallPhase::Exchanging => CallPhase::Connecting,
                    CallPhase::Connecting => {
                        c.connected_at = Some(Local::now());
                        c.emojis = "\u{1f434}\u{1f34e}\u{1f697}\u{1f30d}".to_string();
                        CallPhase::Active
                    }
                    CallPhase::Active | CallPhase::Ended => return,
                };
                st.call.clone().unwrap()
            };
            let done = info.phase == CallPhase::Active;
            if events.send(Event::CallChanged(info)).await.is_err() || done {
                return;
            }
        }
    });
}

/// Tomorrow at HH:MM local time.
fn tomorrow_at(hour: u32, minute: u32) -> DateTime<Local> {
    let tomorrow = Local::now().date_naive() + Duration::days(1);
    tomorrow
        .and_hms_opt(hour, minute, 0)
        .and_then(|naive| naive.and_local_timezone(Local).single())
        .unwrap_or_else(|| Local::now() + Duration::days(1))
}

fn geo(id: i32, sender: &str, minutes_ago: i64, lat: f64, lon: f64) -> Msg {
    Msg {
        media: Some(MediaKind::Location),
        location: Some(LocationInfo { point: GeoPoint { lat, lon }, ..LocationInfo::default() }),
        sender_id: Some(if sender == "Marta" { 1 } else { 4001 }),
        ..msg(id, MEDIA_LAB, "Media Lab", sender, "", t(minutes_ago), false)
    }
}

/// "Media Lab": every wave-6 media kind once (specs/spec-wave6.md §1.10).
fn media_lab_messages() -> Vec<Msg> {
    let now = Local::now();
    let mut live = geo(807, "Marta", 30, 52.5163, 13.3777);
    live.location.as_mut().unwrap().live = Some(LiveLocation {
        period_secs: 3600,
        expires: now + Duration::minutes(45),
        last_update: now - Duration::minutes(2),
        heading: Some(90),
        stopped: false,
    });
    let mut venue = geo(806, "Robin", 34, 52.5033, 13.3559);
    venue.media = Some(MediaKind::Venue);
    if let Some(l) = venue.location.as_mut() {
        l.title = "Café Einstein".into();
        l.address = "Kurfürstenstraße 58, Berlin".into();
    }
    vec![
        Msg {
            media: Some(MediaKind::Voice),
            duration: Some(3),
            doc_size: Some(14_200),
            sender_id: Some(1),
            ..msg(800, MEDIA_LAB, "Media Lab", "Marta", "", t(60), false)
        },
        Msg {
            media: Some(MediaKind::Audio),
            duration: Some(3),
            doc_name: Some("night-drive.ogg".into()),
            doc_size: Some(48_000),
            audio_title: Some("Night Drive".into()),
            audio_performer: Some("Marta".into()),
            sender_id: Some(1),
            ..msg(801, MEDIA_LAB, "Media Lab", "Marta", "", t(58), false)
        },
        Msg {
            media: Some(MediaKind::Video),
            duration: Some(3),
            doc_name: Some("ridge.mp4".into()),
            doc_size: Some(180_000),
            photo_size: Some((320, 240)),
            sender_id: Some(4001),
            ..msg(802, MEDIA_LAB, "Media Lab", "Robin", "test pattern from the ridge", t(52), false)
        },
        Msg {
            media: Some(MediaKind::VideoNote),
            duration: Some(3),
            photo_size: Some((240, 240)),
            round: true,
            doc_size: Some(120_000),
            sender_id: Some(1),
            ..msg(803, MEDIA_LAB, "Media Lab", "Marta", "", t(50), false)
        },
        Msg {
            media: Some(MediaKind::Gif),
            doc_name: Some("loop.mp4".into()),
            doc_size: Some(40_000),
            photo_size: Some((200, 150)),
            duration: Some(2),
            sender_id: Some(4001),
            ..msg(804, MEDIA_LAB, "Media Lab", "Robin", "", t(44), false)
        },
        geo(805, "Marta", 40, 52.5200, 13.4050),
        venue,
        live,
        Msg {
            media: Some(MediaKind::Contact),
            contact: Some(ContactCard {
                first_name: "Marta".into(),
                last_name: "Koenig".into(),
                phone: "+49 30 1234567".into(),
                user_id: Some(1),
            }),
            sender_id: Some(4001),
            ..msg(808, MEDIA_LAB, "Media Lab", "Robin", "", t(26), false)
        },
        Msg {
            media: Some(MediaKind::Contact),
            contact: Some(ContactCard {
                first_name: "Unknown".into(),
                last_name: "Caller".into(),
                phone: "+1 555 0199".into(),
                user_id: None,
            }),
            sender_id: Some(4001),
            ..msg(809, MEDIA_LAB, "Media Lab", "Robin", "", t(25), false)
        },
        Msg {
            media: Some(MediaKind::Dice),
            dice: Some(DiceInfo { emoji: "🎲".into(), value: 4 }),
            sender_id: Some(1),
            ..msg(810, MEDIA_LAB, "Media Lab", "Marta", "", t(20), false)
        },
        Msg {
            media: Some(MediaKind::Dice),
            dice: Some(DiceInfo { emoji: "🎯".into(), value: 6 }),
            ..msg(811, MEDIA_LAB, "Media Lab", "You", "", t(19), true)
        },
        Msg {
            media: Some(MediaKind::Dice),
            dice: Some(DiceInfo { emoji: "🎲".into(), value: 0 }),
            sender_id: Some(4001),
            ..msg(812, MEDIA_LAB, "Media Lab", "Robin", "", t(18), false)
        },
        Msg {
            media: Some(MediaKind::Sticker),
            sticker_emoji: Some("🔥".into()),
            doc_name: Some("fire.tgs".into()),
            photo_size: Some((512, 512)),
            sender_id: Some(1),
            ..msg(813, MEDIA_LAB, "Media Lab", "Marta", "", t(10), false)
        },
    ]
}

fn poll(id: i32, minutes_ago: i64, poll: Poll) -> Msg {
    Msg {
        media: Some(MediaKind::Poll),
        poll: Some(poll),
        sender_id: Some(4001),
        ..msg(id, POLLS, "Polls", "Robin", "", t(minutes_ago), false)
    }
}

fn opts(items: &[(&str, i32)]) -> Vec<PollOption> {
    items.iter().map(|(text, voters)| PollOption { text: (*text).into(), voters: *voters, chosen: false, correct: None }).collect()
}

/// "Polls": open, multiple-choice, quiz, closed and public-voted polls.
fn poll_messages() -> Vec<Msg> {
    let mut voted = opts(&[("Tokyo Night", 7), ("Catppuccin", 5), ("Gruvbox", 4)]);
    voted[1].chosen = true;
    vec![
        poll(
            900,
            90,
            Poll {
                id: 9001,
                question: "Which theme should be the default?".into(),
                options: opts(&[("Tokyo Night", 5), ("Catppuccin", 4), ("Gruvbox", 2), ("Nord", 1)]),
                total_voters: 12,
                ..Poll::default()
            },
        ),
        poll(
            901,
            80,
            Poll {
                id: 9003,
                question: "Which editors do you use? (pick all)".into(),
                options: opts(&[("Neovim", 6), ("Helix", 2), ("VS Code", 3), ("Zed", 1)]),
                total_voters: 8,
                multiple_choice: true,
                ..Poll::default()
            },
        ),
        poll(
            902,
            70,
            Poll {
                id: 9002,
                question: "What does omarchy-theme-next do?".into(),
                options: opts(&[("Installs a theme", 2), ("Switches to the next theme", 9), ("Removes the theme", 1)]),
                total_voters: 12,
                quiz: true,
                solution: Some("It rotates through the installed themes in order.".into()),
                ..Poll::default()
            },
        ),
        poll(
            903,
            60,
            Poll {
                id: 9004,
                question: "Meetup on Friday?".into(),
                options: opts(&[("Yes", 8), ("No", 3)]),
                total_voters: 11,
                closed: true,
                ..Poll::default()
            },
        ),
        poll(
            904,
            50,
            Poll {
                id: 9005,
                question: "Favorite wallpaper pack (public vote)".into(),
                options: voted,
                total_voters: 16,
                public_voters: true,
                voted: true,
                ..Poll::default()
            },
        ),
    ]
}

/// "Omarchy Bot": a welcome message and an inline keyboard.
fn bot_messages() -> Vec<Msg> {
    vec![
        Msg {
            sender_id: Some(BOT),
            ..msg(7100, BOT, "Omarchy Bot", "Omarchy Bot", "Hi! I control the desktop. Type / to see my commands.", t(30), false)
        },
        Msg {
            sender_id: Some(BOT),
            keyboard: Some(Keyboard {
                rows: vec![
                    vec![
                        KeyButton { text: "Next theme".into(), kind: ButtonKind::Callback(b"next_theme".to_vec()) },
                        KeyButton { text: "Lock".into(), kind: ButtonKind::Callback(b"lock".to_vec()) },
                    ],
                    vec![KeyButton { text: "Docs".into(), kind: ButtonKind::Url("https://omarchy.org".into()) }],
                    vec![KeyButton {
                        text: "Search".into(),
                        kind: ButtonKind::SwitchInline { query: "omarchy".into(), same_chat: true },
                    }],
                ],
            }),
            ..msg(7101, BOT, "Omarchy Bot", "Omarchy Bot", "What should I do?", t(29), false)
        },
    ]
}

/// "Omarchy Forum": four topics with their histories under synthetic chat ids.
fn forum_fixture() -> (Vec<Topic>, Vec<(i64, Vec<Msg>)>) {
    let topic = |id: i32, title: &str, icon: &str, pinned: bool, closed: bool| Topic {
        id,
        chat_id: topic_chat_id(FORUM_ID, id),
        forum_id: FORUM_ID,
        title: title.into(),
        icon_emoji: icon.into(),
        unread: 0,
        last_message: String::new(),
        last_time: None,
        pinned,
        closed,
        ..Topic::default()
    };
    let topics = vec![
        topic(1, "General", "", false, false),
        topic(20, "Themes", "🎨", false, false),
        topic(40, "Bugs", "🐛", true, false),
        topic(60, "Off-topic", "", false, true),
    ];
    let tm = |id: i32, topic_id: i32, sender: &str, text: &str, minutes_ago: i64| Msg {
        topic_id: Some(topic_id),
        sender_id: Some(if sender == "You" { ME_ID } else { 4001 + (sender.len() as i64 % 3) }),
        ..msg(id, FORUM_ID, "Omarchy Forum", sender, text, t(minutes_ago), sender == "You")
    };
    let histories = vec![
        (
            topic_chat_id(FORUM_ID, 1),
            vec![
                tm(2, 1, "Robin", "Welcome to the forum. Pick a topic on the left.", 3000),
                tm(3, 1, "Sam", "Rules: be nice, search before asking.", 2990),
                tm(4, 1, "You", "hi all", 2500),
                tm(5, 1, "Robin", "hey!", 2490),
                tm(6, 1, "Yuki", "is there a topic for keyboards?", 900),
            ],
        ),
        (
            topic_chat_id(FORUM_ID, 20),
            vec![
                tm(21, 20, "Robin", "Theme requests go here.", 2000),
                tm(22, 20, "Sam", "Rosé Pine dawn variant please", 1800),
                tm(23, 20, "Yuki", "+1 for Rosé Pine", 1790),
                tm(24, 20, "You", "I can port it this weekend", 1500),
                tm(25, 20, "Robin", "the accent should follow the terminal", 120),
                tm(26, 20, "Sam", "screenshot?", 100),
                tm(27, 20, "Yuki", "looks great in the preview", 40),
                tm(28, 20, "Robin", "merging tonight", 12),
            ],
        ),
        (
            topic_chat_id(FORUM_ID, 40),
            vec![
                tm(41, 40, "Robin", "Report bugs with omarchy-version output.", 2200),
                tm(42, 40, "Yuki", "waybar overlaps the bar after resume", 1000),
                tm(43, 40, "Sam", "reproduced on 2.1", 990),
                tm(44, 40, "You", "fix is in the next release", 800),
                tm(45, 40, "Yuki", "confirmed fixed, thanks", 60),
            ],
        ),
        (
            topic_chat_id(FORUM_ID, 60),
            vec![
                tm(61, 60, "Sam", "anyone at the meetup on friday?", 5000),
                tm(62, 60, "Robin", "yes!", 4990),
                tm(63, 60, "Yuki", "closing this one, use General", 4900),
                tm(64, 60, "Robin", "ok", 4890),
                tm(65, 60, "Sam", "👍", 4880),
            ],
        ),
    ];
    (topics, histories)
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
        let reply = {
            let mut st = st.lock().unwrap();
            let reply = st.push(chat_id, reply);
            *st.unread.entry(chat_id).or_insert(0) += 1;
            reply
        };
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
                    if let Some(msgs) = st.history.get_mut(&chat_id)
                        && let Some(m) = msgs.iter_mut().find(|m| m.id == reply_id) {
                            *m = e.clone();
                        }
                    e
                };
                let _ = events.send(Event::MessageChanged(edited)).await;
            }
        }
    });
}
