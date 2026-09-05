//! Headless read-only probe of the REAL backend against the logged-in account
//! (no GTK). Prints counts, never message bodies. Orchestrator diagnostic:
//!
//!     bin/headless cargo run --example backend_probe -- --dialogs-only
//!
//! Requires an existing authorized session. Never run concurrently with the app
//! or another real-session probe.

use omarchygram::tg::{AuthState, SharedKind, Tg};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let tg = Tg::spawn_real();
    let success = probe(&tg).await;
    tg.shutdown().await;
    if success { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
}

async fn probe(tg: &Tg) -> bool {
    // Narrow startup diagnostic: no history, media, contacts or outgoing actions.
    if std::env::args().any(|arg| arg == "--dialogs-only") {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            if tg.start().await? != AuthState::Ready {
                return Err("existing session is not authorized".to_string());
            }
            println!("ok   authorized ({:.2}s)", started.elapsed().as_secs_f64());
            let (dialogs, folders) = tokio::join!(tg.get_dialogs(), tg.get_folders());
            let dialogs = dialogs?;
            let folders = folders?;
            let unique = dialogs.iter().map(|chat| chat.id).collect::<std::collections::HashSet<_>>().len();
            let archived = dialogs.iter().filter(|chat| chat.archived).count();
            if unique != dialogs.len() { return Err("duplicate dialogs in the startup snapshot".into()); }
            println!("ok   startup: {} unique dialogs ({} normal, {archived} archived), {} folders ({:.2}s)", dialogs.len(), dialogs.len() - archived, folders.len(), started.elapsed().as_secs_f64());
            Ok::<_, String>(())
        }).await;
        match result {
            Ok(Ok(())) => return true,
            Ok(Err(error)) => eprintln!("FAIL startup: {error}"),
            Err(_) => eprintln!("FAIL startup: timed out after 60 seconds"),
        }
        return false;
    }
    match tg.start().await {
        Ok(AuthState::Ready) => {}
        Ok(other) => {
            eprintln!("not logged in: {other:?}");
            return false;
        }
        Err(e) => {
            eprintln!("start failed: {e}");
            return false;
        }
    }
    let step = |name: &str, r: Result<String, String>| match r {
        Ok(v) => println!("ok   {name}: {v}"),
        Err(e) => println!("FAIL {name}: {e}"),
    };

    let me = tg.get_me().await;
    step("get_me", me.as_ref().map(|m| format!("id {} username set: {}", m.id, !m.username.is_empty())).map_err(|e| e.clone()));

    let dialogs = tg.get_dialogs().await;
    step(
        "get_dialogs",
        dialogs.as_ref().map(|d| {
            let pinned = d.iter().filter(|c| c.pinned).count();
            let muted = d.iter().filter(|c| c.muted).count();
            let photos = d.iter().filter(|c| c.has_photo).count();
            let groups = d.iter().filter(|c| matches!(c.kind, omarchygram::tg::ChatKind::Group)).count();
            let channels = d.iter().filter(|c| matches!(c.kind, omarchygram::tg::ChatKind::Channel)).count();
            let drafts = d.iter().filter(|c| !c.draft.is_empty()).count();
            let archived = d.iter().filter(|c| c.archived).count();
            format!("{} dialogs, {pinned} pinned, {muted} muted, {photos} with photo, {groups} groups, {channels} channels, {drafts} drafts, {archived} archived", d.len())
        }).map_err(|e| e.clone()),
    );
    let Ok(dialogs) = dialogs else { return false };
    let first = dialogs.iter().find(|c| c.has_photo).map(|c| c.id);

    if let Some(id) = first {
        step("download_avatar", tg.download_avatar(id).await.map(|p| format!("{:?}", p.map(|p| p.exists()))));
    }
    step("get_folders", tg.get_folders().await.map(|f| format!("{} folders: {:?}", f.len(), f.iter().map(|x| (x.title.len(), x.chats.len())).collect::<Vec<_>>())));
    step("get_contacts", tg.get_contacts().await.map(|c| format!("{} contacts", c.len())));
    step("get_available_reactions", tg.get_available_reactions().await.map(|r| format!("{} reactions", r.len())));
    step("get_sticker_packs", tg.get_sticker_packs().await.map(|p| format!("{} packs", p.len())));
    if let Ok(packs) = tg.get_sticker_packs().await
        && let Some(p) = packs.iter().find(|p| p.id != "recent" && p.id != "favorites") {
            let stickers = tg.get_stickers(&p.id).await;
            step("get_stickers", stickers.as_ref().map(|s| format!("{} stickers, {} animated", s.len(), s.iter().filter(|x| x.animated).count())).map_err(|e| e.clone()));
            if let Ok(s) = stickers
                && let Some(st) = s.iter().find(|x| !x.animated) {
                    step("download_sticker", tg.download_sticker(st.id).await.map(|p| format!("{:?}", p.map(|p| p.exists()))));
                }
        }
    step("get_saved_gifs", tg.get_saved_gifs().await.map(|g| format!("{} gifs", g.len())));
    step("search_global", tg.search_global("the").await.map(|m| format!("{} hits", m.len())));
    if let Some(c) = dialogs.iter().find(|c| matches!(c.kind, omarchygram::tg::ChatKind::Group | omarchygram::tg::ChatKind::Channel)) {
        step("get_chat_info", tg.get_chat_info(c.id).await.map(|i| format!("kind {:?} members {:?} about {} chars", i.kind, i.members, i.about.len())));
        step("get_members", tg.get_members(c.id, 0, 10).await.map(|m| format!("{} members", m.len())));
        step("get_shared_media(photos)", tg.get_shared_media(c.id, SharedKind::Photos, None).await.map(|m| format!("{} photos", m.len())));
        step("get_pinned_message", tg.get_pinned_message(c.id).await.map(|m| format!("{}", m.is_some())));
        step("search_messages", tg.search_messages(c.id, "a", None).await.map(|m| format!("{} hits", m.len())));
    }
    if let Some(c) = dialogs.iter().find(|c| matches!(c.kind, omarchygram::tg::ChatKind::User)) {
        step("get_chat_info(user)", tg.get_chat_info(c.id).await.map(|i| format!("presence {:?} contact {} photo {}", i.presence, i.is_contact, i.has_photo)));
        let hist = tg.get_history(c.id, None).await;
        step(
            "get_history",
            hist.as_ref().map(|h| {
                let spans = h.iter().filter(|m| !m.spans.is_empty()).count();
                let media = h.iter().filter(|m| m.media.is_some()).count();
                let fwd = h.iter().filter(|m| m.forwarded_from.is_some()).count();
                format!("{} msgs, {spans} formatted, {media} media, {fwd} forwarded", h.len())
            }).map_err(|e| e.clone()),
        );
    }
    if let Some(c) = dialogs.iter().find(|c| c.archived) {
        step("get_history(archived)", tg.get_history(c.id, None).await.map(|h| format!("{} msgs in an archived chat", h.len())));
    }
    step("search_chats", tg.search_chats("telegram").await.map(|c| format!("{} results", c.len())));

    // ----- wave 6 (read-only) -----
    step("get_story_peers", tg.get_story_peers().await.map(|p| format!("{} peers with stories, {} unread", p.len(), p.iter().filter(|x| x.unread).count())));
    if let Ok(peers) = tg.get_story_peers().await
        && let Some(p) = peers.first() {
            let stories = tg.get_stories(p.chat_id).await;
            step("get_stories", stories.as_ref().map(|s| format!("{} stories, {} video", s.len(), s.iter().filter(|x| x.video).count())).map_err(|e| e.clone()));
            if let Ok(s) = stories
                && let Some(st) = s.first() {
                    step("download_story", tg.download_story(p.chat_id, st.id).await.map(|p| format!("{:?}", p.map(|p| p.exists()))));
                }
        }
    let forums = dialogs.iter().filter(|c| c.forum).count();
    step("forums", Ok(format!("{forums} forum dialogs")));
    if let Some(f) = dialogs.iter().find(|c| c.forum) {
        let topics = tg.get_topics(f.id).await;
        step("get_topics", topics.as_ref().map(|t| format!("{} topics, {} pinned, {} closed, {} with unread", t.len(), t.iter().filter(|x| x.pinned).count(), t.iter().filter(|x| x.closed).count(), t.iter().filter(|x| x.unread > 0).count())).map_err(|e| e.clone()));
        if let Ok(t) = topics
            && let Some(topic) = t.iter().find(|x| !x.last_message.is_empty()).or(t.first()) {
                let hist = tg.get_history(topic.chat_id, None).await;
                step(
                    "get_history(topic)",
                    hist.as_ref().map(|h| format!("{} msgs, {} carry topic_id {}", h.len(), h.iter().filter(|m| m.topic_id == Some(topic.id)).count(), topic.id)).map_err(|e| e.clone()),
                );
                if let Ok(h) = hist
                    && let Some(m) = h.first() {
                        step("get_messages(topic)", tg.get_messages(topic.chat_id, vec![m.id]).await.map(|v| format!("{} fetched", v.len())));
                    }
            }
    }
    if let Some(c) = dialogs.iter().find(|c| matches!(c.kind, omarchygram::tg::ChatKind::User)) {
        step("get_scheduled", tg.get_scheduled(c.id).await.map(|s| format!("{} scheduled", s.len())));
    }
    if let Some(b) = dialogs.iter().find(|c| matches!(c.kind, omarchygram::tg::ChatKind::Bot)) {
        step("get_chat_info(bot)", tg.get_chat_info(b.id).await.map(|i| format!("{} commands", i.bot_commands.len())));
        step("get_history(bot)", tg.get_history(b.id, None).await.map(|h| format!("{} msgs, {} with keyboards", h.len(), h.iter().filter(|m| m.keyboard.is_some()).count())));
    }
    let media_kinds: std::collections::BTreeMap<String, usize> = {
        let mut counts = std::collections::BTreeMap::new();
        for c in dialogs.iter().take(12) {
            if let Ok(h) = tg.get_history(c.id, None).await {
                for m in h {
                    if let Some(k) = m.media {
                        *counts.entry(format!("{k:?}")).or_insert(0) += 1;
                    }
                    if m.poll.is_some() {
                        *counts.entry("poll payload".into()).or_insert(0) += 1;
                    }
                    if m.location.is_some() {
                        *counts.entry("location payload".into()).or_insert(0) += 1;
                    }
                }
            }
        }
        counts
    };
    step("media kinds in the 12 newest chats", Ok(format!("{media_kinds:?}")));
    step("download_map", tg.download_map(omarchygram::tg::GeoPoint { lat: 52.52, lon: 13.405 }, 15, 320, 180).await.map(|p| format!("{:?}", p.map(|p| p.exists()))));
    println!("done");
    true
}
