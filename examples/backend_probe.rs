//! Headless read-only probe of the REAL backend against the logged-in account
//! (no GTK). Prints counts, never message bodies. Orchestrator diagnostic:
//!
//!     cargo run --example backend_probe
//!
//! Requires an existing session (`cargo run` once and log in).

use omarchygram::tg::{AuthState, SharedKind, Tg};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let tg = Tg::spawn_real();
    match tg.start().await {
        Ok(AuthState::Ready) => {}
        Ok(other) => {
            eprintln!("not logged in: {other:?}");
            return;
        }
        Err(e) => {
            eprintln!("start failed: {e}");
            return;
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
            format!("{} dialogs, {pinned} pinned, {muted} muted, {photos} with photo, {groups} groups, {channels} channels, {drafts} drafts", d.len())
        }).map_err(|e| e.clone()),
    );
    let Ok(dialogs) = dialogs else { return };
    let first = dialogs.iter().find(|c| c.has_photo).map(|c| c.id);

    if let Some(id) = first {
        step("download_avatar", tg.download_avatar(id).await.map(|p| format!("{:?}", p.map(|p| p.exists()))));
    }
    step("get_folders", tg.get_folders().await.map(|f| format!("{} folders: {:?}", f.len(), f.iter().map(|x| (x.title.len(), x.chats.len())).collect::<Vec<_>>())));
    step("get_contacts", tg.get_contacts().await.map(|c| format!("{} contacts", c.len())));
    step("get_available_reactions", tg.get_available_reactions().await.map(|r| format!("{} reactions", r.len())));
    step("get_sticker_packs", tg.get_sticker_packs().await.map(|p| format!("{} packs", p.len())));
    if let Ok(packs) = tg.get_sticker_packs().await {
        if let Some(p) = packs.iter().find(|p| p.id != "recent" && p.id != "favorites") {
            let stickers = tg.get_stickers(&p.id).await;
            step("get_stickers", stickers.as_ref().map(|s| format!("{} stickers, {} animated", s.len(), s.iter().filter(|x| x.animated).count())).map_err(|e| e.clone()));
            if let Ok(s) = stickers {
                if let Some(st) = s.iter().find(|x| !x.animated) {
                    step("download_sticker", tg.download_sticker(st.id).await.map(|p| format!("{:?}", p.map(|p| p.exists()))));
                }
            }
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
    step("search_chats", tg.search_chats("telegram").await.map(|c| format!("{} results", c.len())));
    println!("done");
}
