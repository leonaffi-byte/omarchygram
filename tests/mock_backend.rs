//! Contract tests for the offline backend (`Tg::spawn_mock`, --smoke mode).
//!
//! Each test spawns its own backend, so the tests share no state. Auth states
//! are deliberately not covered: the mock picks them from the `OMG_MOCK_AUTH`
//! environment variable, which is process-wide and would race between tests.

use std::time::Duration;

use omarchygram::tg::{AuthState, Event, Tg};

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
async fn dialogs_are_the_mock_chats_newest_first() {
    let tg = started_mock().await;

    let chats = tg.get_dialogs().await.expect("get_dialogs()");

    assert_eq!(chats.len(), 7, "expected the seven mock chats: {chats:?}");
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
