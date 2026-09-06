//! Synthetic native UI workload. Always run via bin/headless; no real session.
use std::time::{Duration, Instant};

use gtk4::{self as gtk, glib, prelude::*};
use omarchygram::{settings::SettingsStore, tg::{ChatKind, ChatSummary, Msg, Tg}, ui::{anim::Effects, chatlist::{ChatList, UnreadUpdate}, messages::MessagesView}};

fn main() {
    let settings_path = std::env::temp_dir().join(format!("omg-ui-perf-{}.toml", std::process::id()));
    std::fs::write(&settings_path, "[animations]\ndatefloat = true\n").unwrap();
    // Before GTK or the mock backend creates threads; isolate all settings.
    unsafe {
        std::env::set_var("OMG_SETTINGS_PATH", &settings_path);
        std::env::set_var("OMG_STATUS_PATH", settings_path.with_extension("json"));
    }
    gtk::init().unwrap();
    let theme = omarchygram::theme::ThemeManager::attach(&gtk::gdk::Display::default().unwrap());
    let settings = SettingsStore::new();
    let effects = Effects::new(settings.clone());
    let tg = Tg::spawn_mock();
    let chats = ChatList::new(effects.clone(), tg.clone());
    let messages = MessagesView::new(effects.clone());
    messages.set_player_settings(settings);
    messages.set_probe(true);
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.append(&chats.widget);
    root.append(&messages.widget);
    let window = gtk::Window::builder().default_width(1100).default_height(720).child(&root).build();
    window.present();
    glib::MainContext::default().block_on(async {
        let dialogs = (1..=1000).map(|id| ChatSummary { id, title: format!("Test chat {id}"), ..Default::default() }).collect();
        chats.set_chats(dialogs);
        messages.reset_chat(1, "Synthetic history", 1);
        messages.set_chat_summary(&ChatSummary { id: 1, title: "Synthetic history".into(), kind: ChatKind::Group, ..Default::default() }, &tg);
        messages.finish_initial((1..=500).map(|id| Msg { id, chat_id: 1, sender: "Test sender".into(), sender_id: Some(4001), text: "Synthetic text used to measure scrolling and sidebar updates.".into(), ts: chrono::Local::now(), ..Default::default() }).collect());
        glib::timeout_future(Duration::from_millis(500)).await;
        let nearby = messages.nearby_media_ids();
        assert!(!nearby.is_empty() && nearby.len() < 40, "viewport work must not include all 500 messages");
        assert!(chats.probe_update_preserves_mapped_rows(), "updating one chat must not unmap the list");
        let started = Instant::now();
        for _ in 0..100 { chats.set_summary(chats.summary(500).unwrap()); }
        println!("same_row_updates_100_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        for id in 1..=100 { chats.upsert(id, &format!("Test chat {id}"), "New message", None, UnreadUpdate::Delta(0)); }
        println!("incoming_updates_100_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        for _ in 0..10000 { std::hint::black_box(effects.on("datefloat")); }
        println!("animation_lookups_10000_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        for id in (1..=500).rev() { messages.scroll_to_search_result(id); }
        println!("scroll_callbacks_500_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        glib::timeout_future(Duration::from_millis(300)).await;
        let cached = messages.messages();
        messages.reset_history(1, 2);
        messages.finish_cached(cached.clone());
        messages.finish_refreshed(cached.clone(), &cached);
        for _ in 0..15 {
            if messages.pagination_ready() { break; }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        assert!(messages.pagination_ready(), "a refresh before the first layout must restore paging: {}", messages.probe_scroll_state(400));
        messages.finish_refreshed(cached.clone(), &cached);
        assert!(messages.scroll_to_search_result(200));
        glib::timeout_future(Duration::from_millis(500)).await;
        assert!(messages.row_visible(200), "explicit navigation must supersede a pending scroll to bottom");
        window.close();
        tg.shutdown().await;
    });
    drop(theme);
    let _ = std::fs::remove_file(settings_path);
}
