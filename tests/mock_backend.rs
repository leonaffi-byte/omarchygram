//! Contract tests for the offline backend (`Tg::spawn_mock`, --smoke mode).
//!
//! Each test spawns its own backend, so the tests share no state. Auth states
//! are deliberately not covered: the mock picks them from the `OMG_MOCK_AUTH`
//! environment variable, which is process-wide and would race between tests.

use std::time::Duration;

use omarchygram::tg::{topic_chat_id, AuthState, CallPhase, ChatKind, Event, GeoPoint, MediaKind, PollDraft, Tg};

/// Generous upper bound: the mock's scripted reply lands after ~2s.
const EVENT_WAIT: Duration = Duration::from_secs(5);

/// A mock backend that has been started.
async fn started_mock() -> Tg {
    let tg = Tg::spawn_mock();
    tg.start().await.expect("start() should succeed");
    tg
}

#[tokio::test]
async fn start_reports_ready() {
    let tg = Tg::spawn_mock();
    assert_eq!(
        tg.start().await.expect("start() should succeed"),
        AuthState::Ready,
        "the mock backend needs no login"
    );
}

#[tokio::test]
async fn shutdown_stops_all_backend_clones_and_is_repeatable() {
    let tg = started_mock().await;
    let other = tg.clone();
    tokio::time::timeout(EVENT_WAIT, async {
        tokio::join!(tg.shutdown(), other.shutdown());
    }).await.expect("all shutdown callers must observe completed teardown");
    assert!(other.get_dialogs().await.is_err(), "a stopped backend cannot accept requests");
    assert!(tg.events.is_closed(), "shutdown must finish background event producers");
    tokio::time::timeout(EVENT_WAIT, tg.shutdown()).await.expect("repeated shutdown completes");
}

#[tokio::test]
async fn dialogs_are_the_mock_chats_newest_first() {
    let tg = started_mock().await;

    let chats = tg.get_dialogs().await.expect("get_dialogs()");

    assert_eq!(chats.len(), 12, "expected the twelve mock chats: {chats:?}");
    // Pinned chats first, then newest first within each group.
    for pair in chats.windows(2) {
        assert!(
            (pair[0].pinned, pair[0].last_time) >= (pair[1].pinned, pair[1].last_time)
                || (pair[0].pinned && !pair[1].pinned),
            "chats are not sorted pinned-first then by last_time descending: {:?} before {:?}",
            pair[0],
            pair[1]
        );
    }
    assert!(chats[0].pinned, "the pinned chat (Mom) must come first: {chats:?}");
}

#[tokio::test]
async fn history_starts_at_the_newest_message() {
    let tg = started_mock().await;

    let history = tg.get_history(1, None).await.expect("get_history(1, None)");

    let last = history.last().expect("chat 1 has messages");
    assert!(
        last.text.contains("thursday"),
        "expected the newest message last, got {:?}",
        last.text
    );
}

#[tokio::test]
async fn history_pages_one_page_backwards_then_stops() {
    let tg = started_mock().await;

    let history = tg.get_history(1, None).await.expect("get_history(1, None)");
    let oldest_loaded = history.first().expect("chat 1 has messages").id;

    let older = tg
        .get_history(1, Some(oldest_loaded))
        .await
        .expect("get_history(1, Some(..))");
    assert_eq!(older.len(), 2, "expected one older page of 2: {older:?}");

    let chat2 = tg.get_history(2, None).await.expect("get_history(2, None)");
    let oldest_chat2 = chat2.first().expect("chat 2 has messages").id;
    let older_chat2 = tg
        .get_history(2, Some(oldest_chat2))
        .await
        .expect("get_history(2, Some(..))");
    assert!(
        older_chat2.is_empty(),
        "chat 2 has nothing older, got {older_chat2:?}"
    );
}

#[tokio::test]
async fn send_text_appends_to_history_and_emits_typing_then_message() {
    let tg = started_mock().await;

    let sent = tg
        .send_text(1, "hi", None)
        .await
        .expect("send_text() should succeed");
    assert!(sent.outgoing, "a sent message is outgoing");
    assert_eq!(sent.text, "hi");
    assert_eq!(sent.chat_id, 1);

    let history = tg.get_history(1, None).await.expect("get_history(1, None)");
    assert!(
        history.iter().any(|m| m.id == sent.id && m.text == "hi"),
        "the sent message is missing from the history: {history:?}"
    );

    // One deadline for the whole sequence, not one per event. Read-state,
    // presence and dialog-list events may interleave; only the typing →
    // reply order matters here.
    let is_noise = |e: &Event| {
        matches!(
            e,
            Event::ReadOutbox { .. } | Event::ReadInbox { .. } | Event::Presence { .. } | Event::DialogsChanged | Event::PinnedChanged { .. }
        )
    };
    let (typing, incoming) = tokio::time::timeout(EVENT_WAIT, async {
        let mut typing = tg.events.recv().await.expect("event channel closed");
        while is_noise(&typing) {
            typing = tg.events.recv().await.expect("event channel closed");
        }
        let mut incoming = tg.events.recv().await.expect("event channel closed");
        while is_noise(&incoming) {
            incoming = tg.events.recv().await.expect("event channel closed");
        }
        (typing, incoming)
    })
    .await
    .expect("the typing and reply events did not both arrive within 5s");

    match typing {
        Event::Typing { chat_id, .. } => assert_eq!(chat_id, 1),
        other => panic!("expected a Typing event first, got {other:?}"),
    }

    match incoming {
        Event::NewMessage(msg) => {
            assert_eq!(msg.chat_id, 1);
            assert!(!msg.outgoing, "the scripted reply is incoming");
        }
        other => panic!("expected a NewMessage event second, got {other:?}"),
    }
}

#[tokio::test]
async fn edit_text_marks_the_message_edited() {
    let tg = started_mock().await;

    let target = tg
        .get_history(1, None)
        .await
        .expect("get_history(1, None)")
        .first()
        .expect("chat 1 has messages")
        .clone();
    assert!(!target.edited, "the fixture starts unedited");

    let edited = tg
        .edit_text(1, target.id, "corrected")
        .await
        .expect("edit_text() should succeed");
    assert!(edited.edited, "edit_text should set the edited flag");
    assert_eq!(edited.text, "corrected");

    let history = tg.get_history(1, None).await.expect("get_history(1, None)");
    let stored = history
        .iter()
        .find(|m| m.id == target.id)
        .expect("the edited message is still in the history");
    assert!(stored.edited);
    assert_eq!(stored.text, "corrected");
}

#[tokio::test]
async fn delete_message_removes_it_from_history() {
    let tg = started_mock().await;

    let before = tg.get_history(1, None).await.expect("get_history(1, None)");
    let victim = before.first().expect("chat 1 has messages").id;

    tg.delete_message(1, victim)
        .await
        .expect("delete_message() should succeed");

    let after = tg.get_history(1, None).await.expect("get_history(1, None)");
    assert!(
        after.iter().all(|m| m.id != victim),
        "message {victim} survived the delete: {after:?}"
    );
    assert_eq!(after.len(), before.len() - 1);
}

#[tokio::test]
async fn download_media_returns_an_existing_file_or_nothing() {
    let tg = started_mock().await;

    // Message 103 in chat 1 is a photo; the mock serves an Omarchy theme
    // preview, which a machine without /usr/share/omarchy/themes does not have.
    let path = tg
        .download_media(1, 103)
        .await
        .expect("download_media() should succeed");

    if let Some(path) = path {
        assert!(
            path.exists(),
            "download_media returned a path that does not exist: {}",
            path.display()
        );
    }
}

// ===================== wave 6 =====================

/// Waits for the first event `pick` accepts, dropping the others.
async fn wait_event<T>(tg: &Tg, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + EVENT_WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, tg.events.recv())
            .await
            .expect("timed out waiting for an event")
            .expect("event channel closed");
        if let Some(found) = pick(&event) {
            return found;
        }
    }
}

async fn chat_titled(tg: &Tg, title: &str) -> i64 {
    tg.get_dialogs()
        .await
        .expect("get_dialogs()")
        .into_iter()
        .find(|c| c.title == title)
        .unwrap_or_else(|| panic!("no mock chat titled {title}"))
        .id
}

#[tokio::test]
async fn media_lab_has_every_wave6_kind() {
    let tg = started_mock().await;
    let lab = chat_titled(&tg, "Media Lab").await;
    let history = tg.get_history(lab, None).await.expect("history");
    for kind in [
        MediaKind::Voice,
        MediaKind::Audio,
        MediaKind::Video,
        MediaKind::VideoNote,
        MediaKind::Gif,
        MediaKind::Location,
        MediaKind::Venue,
        MediaKind::Contact,
        MediaKind::Dice,
        MediaKind::Sticker,
    ] {
        assert!(history.iter().any(|m| m.media == Some(kind)), "missing {kind:?}");
    }
    let live = history.iter().find(|m| m.location.as_ref().is_some_and(|l| l.live.is_some())).expect("a live location");
    assert!(!live.location.as_ref().unwrap().live.as_ref().unwrap().stopped);
    let venue = history.iter().find(|m| m.media == Some(MediaKind::Venue)).unwrap();
    assert_eq!(venue.location.as_ref().unwrap().title, "Café Einstein");
    assert!(history.iter().any(|m| m.contact.as_ref().is_some_and(|c| c.user_id.is_none())), "a contact without user id");
    assert!(history.iter().any(|m| m.dice.as_ref().is_some_and(|d| d.value == 0)), "a rolling dice");
    let music = history.iter().find(|m| m.media == Some(MediaKind::Audio)).unwrap();
    assert_eq!(music.audio_title.as_deref(), Some("Night Drive"));
    let sticker = history.iter().find(|m| m.media == Some(MediaKind::Sticker)).unwrap();
    let path = tg.download_media(lab, sticker.id).await.expect("download").expect("tgs path");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("tgs"));
    let map = tg.download_media(lab, venue.id).await.expect("download").expect("map path");
    assert_eq!(map.extension().and_then(|e| e.to_str()), Some("png"));
    let tile = tg.download_map(GeoPoint { lat: 1.0, lon: 2.0 }, 12, 96, 96).await.expect("map").expect("tile");
    assert!(tile.exists());
    let tgs = tg.download_sticker(9104).await.expect("download_sticker").expect("animated sticker file");
    assert_eq!(tgs.extension().and_then(|e| e.to_str()), Some("tgs"));
}

#[tokio::test]
async fn polls_vote_retract_and_quiz() {
    let tg = started_mock().await;
    let polls = chat_titled(&tg, "Polls").await;
    let history = tg.get_history(polls, None).await.expect("history");
    let open = history.iter().find(|m| m.poll.as_ref().is_some_and(|p| !p.voted && !p.closed && !p.quiz && !p.multiple_choice)).unwrap();
    let before = open.poll.clone().unwrap();
    tg.send_vote(polls, open.id, vec![1]).await.expect("vote");
    let poll = wait_event(&tg, |e| match e {
        Event::PollChanged { poll_id, poll } if *poll_id == before.id => Some(poll.clone()),
        _ => None,
    })
    .await;
    assert!(poll.voted);
    assert!(poll.options[1].chosen);
    assert_eq!(poll.options[1].voters, before.options[1].voters + 1);
    assert_eq!(poll.total_voters, before.total_voters + 1);
    assert!(tg.send_vote(polls, open.id, vec![0]).await.is_err(), "voting twice must fail");

    tg.send_vote(polls, open.id, vec![]).await.expect("retract");
    let poll = wait_event(&tg, |e| match e {
        Event::PollChanged { poll_id, poll } if *poll_id == before.id => Some(poll.clone()),
        _ => None,
    })
    .await;
    assert!(!poll.voted);
    assert_eq!(poll.total_voters, before.total_voters);

    let quiz = history.iter().find(|m| m.poll.as_ref().is_some_and(|p| p.quiz)).unwrap();
    tg.send_vote(polls, quiz.id, vec![0]).await.expect("quiz vote");
    let poll = wait_event(&tg, |e| match e {
        Event::PollChanged { poll_id, poll } if *poll_id == quiz.poll.as_ref().unwrap().id => Some(poll.clone()),
        _ => None,
    })
    .await;
    assert_eq!(poll.options[0].correct, Some(false));
    assert_eq!(poll.options[1].correct, Some(true));
    assert!(poll.solution.is_some());
    assert!(tg.send_vote(polls, quiz.id, vec![]).await.is_err(), "quiz answers cannot be retracted");

    let closed = history.iter().find(|m| m.poll.as_ref().is_some_and(|p| p.closed)).unwrap();
    assert!(tg.send_vote(polls, closed.id, vec![0]).await.is_err());

    let draft = PollDraft {
        question: "Lunch?".into(),
        options: vec!["Ramen".into(), "Pizza".into(), "".into()],
        anonymous: true,
        multiple_choice: false,
        quiz: false,
        correct_option: None,
        solution: None,
    };
    let sent = tg.send_poll(polls, draft).await.expect("send_poll");
    assert_eq!(sent.media, Some(MediaKind::Poll));
    assert_eq!(sent.poll.as_ref().unwrap().options.len(), 2, "empty options are dropped");
    assert!(tg.send_poll(polls, PollDraft { question: "x".into(), options: vec!["a".into()], ..PollDraft::default() }).await.is_err());
}

#[tokio::test]
async fn scheduled_messages_round_trip() {
    let tg = started_mock().await;
    let list = tg.get_scheduled(1).await.expect("get_scheduled");
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|m| m.scheduled));
    assert!(list[0].ts <= list[1].ts, "soonest first");
    let at = chrono::Local::now() + chrono::Duration::hours(1);
    tg.send_text_at(1, "later", None, at).await.expect("send_text_at");
    wait_event(&tg, |e| matches!(e, Event::ScheduledChanged { chat_id: 1 }).then_some(())).await;
    let list = tg.get_scheduled(1).await.unwrap();
    assert_eq!(list.len(), 3);
    assert!(tg.send_text_at(1, "past", None, chrono::Local::now() - chrono::Duration::hours(1)).await.is_err());
    let first = list[0].id;
    tg.send_scheduled_now(1, vec![first]).await.expect("send now");
    let landed = wait_event(&tg, |e| match e {
        Event::NewMessage(m) if m.chat_id == 1 && m.text == list[0].text => Some(m.clone()),
        _ => None,
    })
    .await;
    assert!(!landed.scheduled);
    assert_eq!(tg.get_scheduled(1).await.unwrap().len(), 2);
    let history = tg.get_history(1, None).await.unwrap();
    assert!(history.iter().any(|m| m.id == landed.id));
    let rest: Vec<i32> = tg.get_scheduled(1).await.unwrap().iter().map(|m| m.id).collect();
    tg.delete_scheduled(1, rest).await.expect("delete");
    assert!(tg.get_scheduled(1).await.unwrap().is_empty());
}

#[tokio::test]
async fn forum_topics_and_topic_chat_ids() {
    let tg = started_mock().await;
    let forum = tg.get_dialogs().await.unwrap().into_iter().find(|c| c.forum).expect("a forum dialog");
    assert_eq!(forum.title, "Omarchy Forum");
    assert!(forum.unread >= 3, "forum unread sums its topics: {}", forum.unread);
    let topics = tg.get_topics(forum.id).await.expect("get_topics");
    assert_eq!(topics.len(), 4);
    assert!(topics[0].pinned, "pinned topic first: {topics:?}");
    let themes = topics.iter().find(|t| t.title == "Themes").unwrap();
    assert_eq!(themes.unread, 3);
    assert_eq!(themes.chat_id, topic_chat_id(forum.id, themes.id));
    let history = tg.get_history(themes.chat_id, None).await.expect("topic history");
    assert_eq!(history.len(), 8);
    assert!(history.iter().all(|m| m.chat_id == forum.id && m.topic_id == Some(themes.id)));
    let sent = tg.send_text(themes.chat_id, "porting now", None).await.expect("send in topic");
    assert_eq!(sent.chat_id, forum.id);
    assert_eq!(sent.topic_id, Some(themes.id));
    let reply = wait_event(&tg, |e| match e {
        Event::NewMessage(m) if !m.outgoing && m.chat_id == forum.id => Some(m.clone()),
        _ => None,
    })
    .await;
    assert_eq!(reply.topic_id, Some(themes.id), "the mock reply lands in the same topic");
    tg.mark_read(themes.chat_id, reply.id).await.expect("mark_read on a topic");
    let created = tg.create_topic(forum.id, "Keyboards").await.expect("create_topic");
    wait_event(&tg, |e| matches!(e, Event::TopicsChanged { forum_id } if *forum_id == forum.id).then_some(())).await;
    assert!(tg.get_topics(forum.id).await.unwrap().iter().any(|t| t.id == created.id));
    assert!(tg.get_history(created.chat_id, None).await.unwrap().is_empty());
    assert!(tg.get_topics(1).await.is_err(), "not a forum");
}

#[tokio::test]
async fn bot_keyboard_and_commands() {
    let tg = started_mock().await;
    let bot = chat_titled(&tg, "Omarchy Bot").await;
    let info = tg.get_chat_info(bot).await.expect("chat info");
    assert_eq!(info.kind, ChatKind::Bot);
    assert!(info.bot_commands.iter().any(|c| c.command == "theme"));
    let history = tg.get_history(bot, None).await.unwrap();
    let keyed = history.iter().find(|m| m.keyboard.is_some()).expect("a keyboard message");
    let keyboard = keyed.keyboard.clone().unwrap();
    assert_eq!(keyboard.rows.len(), 3);
    let lock = match &keyboard.rows[0][1].kind {
        omarchygram::tg::ButtonKind::Callback(data) => data.clone(),
        other => panic!("expected a callback button, got {other:?}"),
    };
    assert_eq!(tg.press_button(bot, keyed.id, lock).await.unwrap(), Some("Locked (mock)".to_string()));
    let next = match &keyboard.rows[0][0].kind {
        omarchygram::tg::ButtonKind::Callback(data) => data.clone(),
        other => panic!("expected a callback button, got {other:?}"),
    };
    assert_eq!(tg.press_button(bot, keyed.id, next).await.unwrap(), None);
    let changed = wait_event(&tg, |e| match e {
        Event::MessageChanged(m) if m.id == keyed.id => Some(m.clone()),
        _ => None,
    })
    .await;
    assert_eq!(changed.keyboard.unwrap().rows[0][0].text, "Next theme ✓");
    let helper = chat_titled(&tg, "Helper Bot").await;
    assert!(tg.get_history(helper, None).await.unwrap().is_empty(), "the Start button fixture is empty");
}

#[tokio::test]
async fn stories_and_live_location() {
    let tg = started_mock().await;
    let peers = tg.get_story_peers().await.expect("story peers");
    assert_eq!(peers[0].name, "Marta");
    assert!(peers[0].unread);
    let stories = tg.get_stories(1).await.unwrap();
    assert_eq!(stories.len(), 2);
    assert!(stories[1].video);
    let photo = tg.download_story(1, stories[0].id).await.expect("download_story");
    assert!(photo.is_none_or(|p| p.exists()));
    tg.mark_stories_seen(1, 2).await.unwrap();
    wait_event(&tg, |e| matches!(e, Event::StoriesChanged).then_some(())).await;
    let peers = tg.get_story_peers().await.unwrap();
    assert!(!peers.iter().find(|p| p.chat_id == 1).unwrap().unread);
    let marta = tg.get_dialogs().await.unwrap().into_iter().find(|c| c.id == 1).unwrap();
    assert_eq!(marta.story_ring, omarchygram::tg::StoryRing::Read);

    let sent = tg.send_live_location(1, GeoPoint { lat: 52.5, lon: 13.4 }, 900).await.expect("live");
    let live = sent.location.as_ref().and_then(|l| l.live.as_ref()).expect("live payload");
    assert_eq!(live.period_secs, 900);
    tg.update_live_location(1, sent.id, GeoPoint { lat: 52.6, lon: 13.4 }).await.expect("update");
    let moved = wait_event(&tg, |e| match e {
        Event::MessageChanged(m) if m.id == sent.id => Some(m.clone()),
        _ => None,
    })
    .await;
    assert!((moved.location.as_ref().unwrap().point.lat - 52.6).abs() < 1e-9);
    tg.stop_live_location(1, sent.id).await.expect("stop");
    let stopped = wait_event(&tg, |e| match e {
        Event::MessageChanged(m) if m.id == sent.id => Some(m.clone()),
        _ => None,
    })
    .await;
    assert!(stopped.location.unwrap().live.unwrap().stopped);
    assert!(tg.update_live_location(1, sent.id, GeoPoint { lat: 1.0, lon: 1.0 }).await.is_err());
    let plain = tg.send_location(1, GeoPoint { lat: 1.0, lon: 2.0 }).await.unwrap();
    assert_eq!(plain.media, Some(MediaKind::Location));
    tg.add_contact(5007, "Alex", "Petrov", "+7 900").await.expect("add_contact");
    assert!(tg.add_contact(5008, "", "", "").await.is_err());
}

#[tokio::test]
async fn outgoing_call_rings_connects_mutes_and_hangs_up() {
    let tg = started_mock().await;
    tg.call_start(1).await.expect("call_start");
    // First event is Requesting.
    let first = wait_event(&tg, |e| match e {
        Event::CallChanged(c) => Some(c.clone()),
        _ => None,
    })
    .await;
    assert_eq!(first.phase, CallPhase::Requesting);
    assert!(first.outgoing);
    assert_eq!(first.peer_id, 1);
    // Advances to Active with the emoji fingerprint.
    let active = wait_event(&tg, |e| match e {
        Event::CallChanged(c) if c.phase == CallPhase::Active => Some(c.clone()),
        _ => None,
    })
    .await;
    assert!(!active.emojis.is_empty());
    assert!(active.connected_at.is_some());
    // Mute re-emits with muted = true.
    tg.call_set_muted(true).await.expect("mute");
    let muted = wait_event(&tg, |e| match e {
        Event::CallChanged(c) if c.muted => Some(c.clone()),
        _ => None,
    })
    .await;
    assert_eq!(muted.phase, CallPhase::Active);
    tg.call_set_devices(active.id, "mock-mic-usb".into(), "mock-out-hdmi".into()).await.expect("switch active call devices");
    assert!(tg.call_set_devices(active.id + 1, String::new(), String::new()).await.is_err(), "stale device changes must be rejected");
    // A second call while active is refused.
    assert!(tg.call_start(2).await.is_err());
    // Hang up ends the call.
    tg.call_hang_up().await.expect("hang up");
    let ended = wait_event(&tg, |e| match e {
        Event::CallChanged(c) if c.phase == CallPhase::Ended => Some(c.clone()),
        _ => None,
    })
    .await;
    assert!(ended.end_reason.is_some());
}

#[tokio::test]
async fn calling_a_bot_is_refused_and_devices_list() {
    let tg = started_mock().await;
    let bot = chat_titled(&tg, "Omarchy Bot").await;
    assert!(tg.call_start(bot).await.is_err(), "cannot call a bot");
    let devices = tg.call_devices().await.expect("devices");
    assert!(!devices.input.is_empty() && !devices.output.is_empty());
}

#[tokio::test]
async fn topic_actions_never_mutate_the_forum_or_other_topics() {
    use omarchygram::tg::MuteMode;
    let tg = started_mock().await;
    let forum = chat_titled(&tg, "Omarchy Forum").await;
    let topics = tg.get_topics(forum).await.unwrap();
    let topic = topics.iter().find(|topic| topic.id != 1 && !topic.pinned).unwrap();
    let other = topics.iter().find(|candidate| candidate.id != topic.id).unwrap();
    let parent_before = tg.get_dialogs().await.unwrap().into_iter().find(|chat| chat.id == forum).unwrap();
    let other_history = tg.get_history(other.chat_id, None).await.unwrap();
    tg.set_muted(topic.chat_id, MuteMode::Forever).await.unwrap();
    tg.set_pinned(topic.chat_id, true).await.unwrap();
    tg.save_draft(topic.chat_id, "topic draft", Some(123)).await.unwrap();
    assert!(tg.set_archived(topic.chat_id, true).await.is_err());
    assert!(tg.mark_unread(topic.chat_id, true).await.is_err());
    let changed = tg.get_topics(forum).await.unwrap().into_iter().find(|entry| entry.id == topic.id).unwrap();
    assert!(changed.muted && changed.pinned);
    assert_eq!(changed.draft, "topic draft");
    assert_eq!(changed.draft_reply_to, Some(123));
    let parent_after = tg.get_dialogs().await.unwrap().into_iter().find(|chat| chat.id == forum).unwrap();
    assert_eq!((parent_before.muted,parent_before.pinned,parent_before.draft), (parent_after.muted,parent_after.pinned,parent_after.draft));
    tg.clear_history(topic.chat_id).await.unwrap();
    assert!(tg.get_history(topic.chat_id, None).await.unwrap().is_empty());
    assert_eq!(tg.get_history(other.chat_id, None).await.unwrap(), other_history);
    assert!(tg.get_topics(forum).await.unwrap().iter().any(|entry| entry.id == topic.id));
    tg.delete_chat(topic.chat_id).await.unwrap();
    assert!(!tg.get_topics(forum).await.unwrap().iter().any(|entry| entry.id == topic.id));
    assert!(tg.get_dialogs().await.unwrap().iter().any(|chat| chat.id == forum));
    assert!(tg.delete_chat(topic_chat_id(forum, 1)).await.is_err());
}

#[tokio::test]
async fn recent_history_is_available_without_another_server_load() {
    let tg = started_mock().await;
    assert!(tg.get_cached_history(1).await.unwrap().is_empty());
    let messages = tg.get_history(1, None).await.unwrap();
    assert_eq!(tg.get_cached_history(1).await.unwrap(), messages);
    assert!(tg.get_cached_history(2).await.unwrap().is_empty());
    tg.shutdown().await;
}

#[tokio::test]
async fn group_sender_profile_does_not_create_a_dialog_until_message_action() {
    let tg = started_mock().await;
    let before = tg.get_dialogs().await.unwrap();
    let sender = tg.get_history(4, None).await.unwrap().into_iter()
        .find(|m| m.sender_id == Some(4001)).unwrap();
    let info = tg.get_user_profile(4001, Some((4, sender.id))).await.unwrap();
    assert_eq!(info.title, "Robin");
    assert!(info.has_photo);
    assert!(!info.is_contact);
    assert!(tg.download_profile_photo(info.id).await.unwrap().is_some());
    assert_eq!(tg.get_dialogs().await.unwrap().len(), before.len());
    let summary = tg.open_user(info.id).await.unwrap();
    assert_eq!(summary.id, info.id);
    assert_eq!(summary.kind, ChatKind::User);
    assert_eq!(tg.get_dialogs().await.unwrap().len(), before.len() + 1);
    tg.shutdown().await;
}
