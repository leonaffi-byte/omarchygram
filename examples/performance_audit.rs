//! Local/offline timing audit. Always run through bin/headless; JSON output.
//! Private implementation modules are compiled from their original source,
//! so cache/decoder measurements exercise shipped code without new public APIs.
use gtk4::{self as gtk, glib, prelude::*};
use omarchygram::{ai, settings::SettingsStore, tg, ui};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use tg::*;
#[allow(dead_code)]
#[path = "../src/tg/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../src/tg/history_cache.rs"]
mod history_cache;
#[path = "../src/ui/media_image.rs"]
mod media_image;
#[allow(dead_code)]
mod perf_support;
#[allow(dead_code)]
#[path = "../src/storage.rs"]
mod storage;
use perf_support::{elapsed, sample};

fn decode_media(path: &Path, video: bool) -> (f64, f64) {
    use gstreamer::{self as gst, prelude::*};
    use std::sync::{Arc, Mutex};
    gst::init().unwrap();
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("signal-handoffs", true)
        .build()
        .unwrap();
    let first = Arc::new(Mutex::new(None));
    let received = first.clone();
    let start = Instant::now();
    sink.connect("handoff", false, move |_| {
        received
            .lock()
            .unwrap()
            .get_or_insert_with(|| elapsed(start));
        None
    });
    let player = gst::ElementFactory::make("playbin3")
        .property("uri", gtk::gio::File::for_path(path).uri().to_string())
        .property(if video { "video-sink" } else { "audio-sink" }, &sink)
        .build()
        .unwrap();
    let discard = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .unwrap();
    player.set_property(if video { "audio-sink" } else { "video-sink" }, &discard);
    player.set_state(gst::State::Playing).unwrap();
    let bus = player.bus().unwrap();
    let end = bus
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(20),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("decode timeout");
    let total = elapsed(start);
    player.set_state(gst::State::Null).unwrap();
    if let gst::MessageView::Error(error) = end.view() {
        panic!("decode failed: {}", error.error());
    }
    let first = first.lock().unwrap().expect("decoded buffer");
    (first, total)
}

fn history(count: i32) -> Vec<Msg> {
    (1..=count)
        .map(|id| Msg {
            id,
            chat_id: 1,
            sender_id: Some(4001 + i64::from(id % 12)),
            sender: format!("Participant {}", id % 12),
            text: format!(
                "Message {id}: Review the proposal and confirm the next steps. {}",
                "Additional detail for realistic line wrapping. ".repeat((id % 4) as usize)
            ),
            ts: chrono::Local::now(),
            ..Default::default()
        })
        .collect()
}

async fn until(name: &str, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "timed out: {name}"
        );
        glib::timeout_future(Duration::from_millis(1)).await;
    }
}

async fn painted(window: &gtk::Window) {
    let clock = window.frame_clock().unwrap();
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    let painted = done.clone();
    let handler = clock.connect_after_paint(move |_| painted.set(true));
    window.queue_draw();
    until("next GTK after-paint", || done.get()).await;
    clock.disconnect(handler);
}

fn descendants(widget: &gtk::Widget, class: &str) -> usize {
    let mut count =
        usize::from(widget.has_css_class(class) && widget.is_mapped() && widget.width() > 0);
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        count += descendants(&current, class);
    }
    count
}

fn mapped_text(widget: &gtk::Widget, expected: &str) -> bool {
    if widget.is_mapped() && widget.width() > 0 && widget.height() > 0
        && widget.downcast_ref::<gtk::Label>().is_some_and(|label| label.text() == expected) {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        if mapped_text(&current, expected) { return true; }
    }
    false
}

fn named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        if let Some(found) = named(&current, name) {
            return Some(found);
        }
    }
    None
}

async fn startup(start: Instant) {
    let application = gtk::Application::builder()
        .application_id("dev.leoom.Omarchygram.PerformanceAudit")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.register(gtk::gio::Cancellable::NONE).unwrap();
    let backend = Tg::spawn_mock();
    let window = gtk::ApplicationWindow::builder()
        .application(&application)
        .default_width(1100)
        .default_height(720)
        .build();
    window.add_css_class("omg-window");
    let shell = ui::shell::Shell::new(backend.clone(), false);
    sample("startup_shell_construct_from_main", &[elapsed(start)]);
    window.set_child(Some(&shell.widget));
    window.present();
    shell.start().await;
    until("chat rows mapped", || {
        shell.is_ready() && descendants(shell.widget.upcast_ref(), "omg-chat-row") > 0
    })
    .await;
    painted(window.upcast_ref()).await;
    sample(
        "startup_mock_chat_list_painted_from_main",
        &[elapsed(start)],
    );
    println!(
        "{}",
        serde_json::json!({"metric":"startup_resources","resources":perf_support::resources()})
    );
    let mut first = Vec::new();
    let mut reopened = Vec::new();
    for i in 0..8 {
        let id = if i % 2 == 0 { 1 } else { 3 };
        let row = named(shell.widget.upcast_ref(), &format!("chat-{id}"))
            .unwrap()
            .downcast::<gtk::ListBoxRow>()
            .unwrap();
        let list = row.parent().unwrap().downcast::<gtk::ListBox>().unwrap();
        let started = Instant::now();
        list.emit_by_name::<()>("row-activated", &[&row]);
        until("opened chat messages mapped", || {
            descendants(shell.widget.upcast_ref(), "omg-msg-row") > 0
        })
        .await;
        painted(window.upcast_ref()).await;
        if i < 2 {
            first.push(elapsed(started));
        } else {
            reopened.push(elapsed(started));
        }
    }
    sample("shell_mock_first_chat_open_to_messages_painted", &first);
    sample("shell_mock_reopen_chat_to_messages_painted", &reopened);
    let started = Instant::now();
    shell.open_contacts();
    until("contacts dialog", || {
        descendants(shell.widget.upcast_ref(), "omg-contacts-dialog") > 0
    })
    .await;
    painted(window.upcast_ref()).await;
    sample("contacts_dialog_open_to_frame", &[elapsed(started)]);
    window.close();
    backend.shutdown().await;
}

async fn native_ui() {
    let settings = SettingsStore::new();
    let effects = ui::anim::Effects::new(settings.clone());
    let backend = Tg::spawn_mock();
    let chats = std::rc::Rc::new(ui::chatlist::ChatList::new(effects.clone(), backend.clone()));
    let weak = std::rc::Rc::downgrade(&chats);
    chats.set_on_search(std::rc::Rc::new(move |_, _| {
        // Match Shell's immediate local-result rendering. The standalone
        // list otherwise updates its query/counts without building results.
        if let Some(chats) = weak.upgrade() { chats.refresh_search(); }
    }));
    let messages = ui::messages::MessagesView::new(effects);
    messages.set_player_settings(settings.clone());
    messages.set_probe(true);
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.append(&chats.widget);
    root.append(&messages.widget);
    let window = gtk::Window::builder()
        .default_width(1100)
        .default_height(720)
        .child(&root)
        .build();
    window.add_css_class("omg-window");
    window.present();
    painted(&window).await;
    let mut build = Vec::new();
    println!("{}", serde_json::json!({"metric":"sidebar_empty_resources", "resources":perf_support::resources()}));
    for _ in 0..5 {
        let dialogs = (1..=1000)
            .map(|id| ChatSummary {
                id,
                title: format!("Benchmark chat {id}"),
                last_message: "A preview of a realistic conversation".into(),
                ..Default::default()
            })
            .collect();
        let start = Instant::now();
        chats.set_chats(dialogs);
        painted(&window).await;
        assert!(chats.probe_visible_content_ready(), "first sidebar paint contains chat titles");
        build.push(elapsed(start));
    }
    sample("chat_list_1000_first_build_to_frame", &build[..1]);
    sample("chat_list_1000_unchanged_snapshot_to_frame", &build[1..]);
    println!("{}", serde_json::json!({"metric":"sidebar_1000_resources", "resources":perf_support::resources(),
        "materialized_rows":chats.probe_materialized_row_count()}));
    for count in [50, 500] {
        let data = history(count);
        let mut initial = Vec::new();
        let mut cached = Vec::new();
        let mut refresh = Vec::new();
        let mut cpu_build = Vec::new();
        for generation in 1..=7 {
            messages.reset_chat(1, "Benchmark group", generation);
            messages.set_chat_summary(
                &ChatSummary {
                    id: 1,
                    title: "Benchmark group".into(),
                    kind: ChatKind::Group,
                    ..Default::default()
                },
                &backend,
            );
            let start = Instant::now();
            messages.finish_initial(data.clone());
            cpu_build.push(elapsed(start));
            painted(&window).await;
            until("initial history ready", || !messages.is_loading()).await;
            assert!(messages.probe_visible_content_ready(), "first paint contains every visible message body");
            assert!(messages.row_visible(count), "initial history paints the latest message");
            initial.push(elapsed(start));
            messages.reset_history(1, generation + 100);
            let start = Instant::now();
            messages.finish_cached(data.clone());
            painted(&window).await;
            assert!(messages.probe_visible_content_ready(), "cached paint contains every visible message body");
            assert!(messages.row_visible(count), "cached history paints the latest message");
            cached.push(elapsed(start));
            let start = Instant::now();
            messages.finish_refreshed(data.clone(), &data);
            painted(&window).await;
            refresh.push(elapsed(start));
        }
        sample(&format!("history_{count}_widget_build_sync"), &cpu_build);
        sample(&format!("history_{count}_initial_to_frame"), &initial);
        sample(&format!("history_{count}_cached_to_frame"), &cached);
        sample(
            &format!("history_{count}_unchanged_refresh_to_frame"),
            &refresh,
        );
        println!(
            "{}",
            serde_json::json!({"metric":format!("history_{count}_resources"),"resources":perf_support::resources()})
        );
        println!("{}", serde_json::json!({"history_count": count,
            "materialized_rows": messages.probe_materialized_row_count()}));
    }
    let mut updates = Vec::new();
    let mut incoming = Vec::new();
    let mut search = Vec::new();
    let mut composer = Vec::new();
    let mut jump = Vec::new();
    let jump_history = history(500);
    for i in 0..30 {
        let start = Instant::now();
        chats.set_summary(chats.summary(500).unwrap());
        updates.push(elapsed(start));
        let start = Instant::now();
        chats.upsert(
            500,
            "Benchmark chat 500",
            &format!("Incoming update {i}"),
            None,
            ui::chatlist::UnreadUpdate::Delta(0),
        );
        painted(&window).await;
        incoming.push(elapsed(start));
        chats.clear_search();
        until("search reset before query", || {
            chats.search_counts().0 == 1000
        })
        .await;
        painted(&window).await;
        let start = Instant::now();
        chats.set_search_text(&format!("Benchmark chat {}", 970 + i));
        until("local search matches query", || {
            chats.search_counts().0 == 1
        })
        .await;
        painted(&window).await;
        assert_eq!(descendants(chats.widget.upcast_ref(), "omg-search-row"), 1, "one local result must be rendered");
        assert!(named(chats.widget.upcast_ref(), &format!("search-chat-{}", 970 + i))
            .is_some_and(|row| row.is_mapped() && row.width() > 0), "the matching chat must be painted");
        search.push(elapsed(start));
        chats.clear_search();
        // Keep search teardown out of the independent typing measurement.
        until("search reset before typing", || {
            chats.search_counts().0 == 1000
        })
        .await;
        painted(&window).await;
        let start = Instant::now();
        messages.set_composer_text(&format!("Typing benchmark {i}"));
        painted(&window).await;
        composer.push(elapsed(start));
        let start = Instant::now();
        let target = if i % 2 == 0 { 50 } else { 450 };
        assert!(messages.scroll_to_search_result(target));
        until("jump target visible", || messages.row_visible(target)).await;
        painted(&window).await;
        assert!(messages.probe_visible_content_ready(), "jump paints all visible message bodies");
        assert!(mapped_text(messages.widget.upcast_ref(), &jump_history[target as usize - 1].text),
            "jump target contains the original message text");
        jump.push(elapsed(start));
    }
    sample("unchanged_sidebar_row_update_sync", &updates);
    sample("incoming_sidebar_update_to_frame", &incoming);
    sample("local_chat_search_1000_to_frame", &search);
    sample("composer_update_to_frame", &composer);
    sample("message_jump_to_frame", &jump);
    let mut broad_search = Vec::new();
    for _ in 0..5 {
        chats.clear_search();
        painted(&window).await;
        for query in ["B", "Be", "Benchmark", "Benchmark chat"] {
            let generation = chats.search_generation();
            let started = Instant::now();
            chats.set_search_text(query);
            until("broad query processed", || chats.search_generation() != generation
                && chats.search_counts().0 == 1000).await;
            painted(&window).await;
            broad_search.push(elapsed(started));
        }
    }
    sample("broad_chat_search_1000_matches_to_frame", &broad_search);
    let search_list = named(chats.widget.upcast_ref(), "search-results").unwrap()
        .downcast::<gtk::ListView>().unwrap();
    search_list.scroll_to(1000, gtk::ListScrollFlags::NONE, None);
    until("last of 1000 search results visible", || {
        named(chats.widget.upcast_ref(), "search-chat-1000").and_then(|row| row.compute_bounds(&search_list))
            .is_some_and(|bounds| bounds.height() > 0.0 && bounds.y() < search_list.height() as f32
                && bounds.y() + bounds.height() > 0.0)
    }).await;
    painted(&window).await;
    let opened = std::rc::Rc::new(std::cell::Cell::new(None));
    let opened_callback = opened.clone();
    chats.set_on_search_open(std::rc::Rc::new(move |id, _| opened_callback.set(Some(id))));
    named(chats.widget.upcast_ref(), "search-chat-1000").unwrap()
        .downcast::<gtk::Button>().unwrap().emit_clicked();
    assert_eq!(opened.get(), Some(1000), "recycled search result opens its own chat");
    println!("{}", serde_json::json!({"search_virtualization_navigation":"PASS",
        "model_chats": chats.search_counts().0,
        "bound_buttons": descendants(chats.widget.upcast_ref(), "omg-search-row")}));
    chats.clear_search();
    painted(&window).await;
    let last_chat = named(chats.widget.upcast_ref(), "chat-1000").unwrap()
        .downcast::<gtk::ListBoxRow>().unwrap();
    let sidebar_scroll = last_chat.ancestor(gtk::ScrolledWindow::static_type()).unwrap()
        .downcast::<gtk::ScrolledWindow>().unwrap();
    let adjustment = sidebar_scroll.vadjustment();
    let first_chat = named(chats.widget.upcast_ref(), "chat-1").unwrap();
    let first_height = first_chat.measure(gtk::Orientation::Vertical, sidebar_scroll.width()).1;
    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
    painted(&window).await;
    assert!(chats.probe_visible_content_ready(), "scrolling the sidebar paints every visible title");
    assert!(mapped_text(last_chat.upcast_ref(), "Benchmark chat 1000"), "last sidebar row has its real title");
    assert_eq!(first_chat.measure(gtk::Orientation::Vertical, sidebar_scroll.width()).1,
        first_height, "releasing offscreen controls preserves their row height");
    let opened_callback = opened.clone();
    chats.set_on_open(std::rc::Rc::new(move |id| opened_callback.set(Some(id))));
    opened.set(None);
    last_chat.parent().unwrap().downcast::<gtk::ListBox>().unwrap()
        .emit_by_name::<()>("row-activated", &[&last_chat]);
    assert_eq!(opened.get(), Some(1000), "last sidebar row opens its own chat");
    adjustment.set_value(0.0);
    painted(&window).await;
    assert!(chats.probe_visible_content_ready(), "returning to the sidebar top restores its titles");
    println!("{}", serde_json::json!({"sidebar_virtualization_navigation":"PASS",
        "materialized_rows":chats.probe_materialized_row_count()}));
    // Exercise nonuniform row geometry after the timed samples. Large text
    // and unread counters can make real rows taller than the CSS minimum.
    let font = gtk::CssProvider::new();
    font.load_from_string(".omg-sidebar label { font-size: 22px; }");
    gtk::style_context_add_provider_for_display(&gtk::prelude::WidgetExt::display(&window), &font,
        gtk::STYLE_PROVIDER_PRIORITY_USER);
    for id in (1..=1000).step_by(3) { chats.set_unread_mark(id, true); }
    for (compact, collapsed) in [(false, false), (true, false), (true, true), (false, false)] {
        chats.set_compact(compact);
        chats.set_collapsed(collapsed);
        painted(&window).await;
        for fraction in [0.0, 0.51, 1.0, 0.24, 0.0] {
            adjustment.set_value((adjustment.upper() - adjustment.page_size()) * fraction);
            painted(&window).await;
            painted(&window).await;
            assert!(chats.probe_visible_content_ready(),
                "sidebar titles/avatars ready: compact={compact} collapsed={collapsed} fraction={fraction}");
        }
    }
    gtk::style_context_remove_provider_for_display(&gtk::prelude::WidgetExt::display(&window), &font);
    painted(&window).await;
    println!("sidebar compact/collapsed/large text navigation: PASS");
    let before = perf_support::resources();
    for cycle in 0..60 {
        messages.reset_chat(1, "Repeated switch", 1000 + cycle);
        messages.finish_initial(history(50));
        painted(&window).await;
    }
    println!(
        "{}",
        serde_json::json!({"metric":"60_chat_rebuilds_resources","before":before,"after":perf_support::resources()})
    );
    window.close();
    backend.shutdown().await;
}

async fn history_cache_check() {
    let settings = SettingsStore::new();
    let effects = ui::anim::Effects::new(settings.clone());
    let backend = Tg::spawn_mock();
    let messages = ui::messages::MessagesView::new(effects);
    messages.set_player_settings(settings);
    messages.set_probe(true);
    messages.set_ai_enabled(true);
    let window = gtk::Window::builder().default_width(800).default_height(720)
        .child(&messages.widget).build();
    window.add_css_class("omg-window");
    window.present();
    painted(&window).await;
    messages.reset_chat(1, "State preservation", 1);
    messages.set_chat_summary(&ChatSummary { id: 1, title: "State preservation".into(),
        kind: ChatKind::Group, ..Default::default() }, &backend);
    let mut data = history(500);
    data[9].text = "secret and ordinary text".into();
    data[9].spans = vec![Span { start: 0, end: 6, kind: SpanKind::Spoiler }];
    for index in [19, 20, 21] {
        data[index].media = Some(MediaKind::Document);
        data[index].doc_name = Some("retained-document.pdf".into());
        data[index].doc_size = Some(128_000);
    }
    messages.finish_initial(data.clone());
    let document_path = std::env::temp_dir().join("omg-document-cache-probe.pdf");
    let (_, generation) = messages.begin_media(20).unwrap();
    assert!(messages.finish_media_path(20, generation, document_path.clone()));
    messages.begin_media(21).unwrap();
    let (_, failed_generation) = messages.begin_media(22).unwrap();
    assert!(messages.fail_media(22, failed_generation, true));
    painted(&window).await;
    assert!(messages.scroll_to_search_result(10));
    painted(&window).await;
    painted(&window).await;
    assert!(messages.reveal_spoiler(10, 0, 6));
    assert!(messages.render_aux(10, Some("retained transcript"), Some("retained translation"), None));
    messages.set_monospace(10, true);
    assert!(messages.begin_selection(10));
    let markup = messages.rendered_markup(10).unwrap();
    for _ in 0..3 {
        assert!(messages.scroll_to_search_result(250));
        painted(&window).await;
        assert!(messages.scroll_to_search_result(490));
        painted(&window).await;
        painted(&window).await;
        until("old text row evicted", || !messages.probe_row_materialized(10)).await;
        assert!(!messages.probe_row_materialized(20), "completed document controls can be reclaimed");
        assert!(messages.probe_row_materialized(21), "in-flight document controls stay intact");
        assert!(messages.probe_row_materialized(22), "document error and retry controls stay intact");
        assert!(messages.scroll_to_search_result(10));
        painted(&window).await;
        painted(&window).await;
        assert_eq!(messages.rendered_markup(10).as_deref(), Some(markup.as_str()), "revealed spoiler survives eviction");
        assert!(messages.aux_contains(10, "retained transcript"));
        assert!(messages.aux_contains(10, "retained translation"));
        assert_eq!(messages.selection_ids(), vec![10]);
        assert!(messages.is_monospace(10));
        assert!(messages.probe_visible_content_ready());
    }
    assert!(messages.scroll_to_search_result(250));
    painted(&window).await;
    assert!(messages.scroll_to_search_result(490));
    painted(&window).await;
    painted(&window).await;
    until("row evicted before background update", || !messages.probe_row_materialized(10)).await;
    let mut edited = data[9].clone();
    edited.text = "changed hidden text".into();
    edited.spans = vec![Span { start: 0, end: 7, kind: SpanKind::Spoiler }];
    edited.edited = true;
    messages.merge_event(edited.clone());
    assert!(messages.render_aux(10, None, Some("new translation"), None));
    assert!(!messages.probe_row_materialized(10), "background updates do not rebuild offscreen controls");
    assert!(messages.scroll_to_search_result(10));
    painted(&window).await;
    painted(&window).await;
    assert_eq!(messages.message(10).unwrap(), edited);
    assert!(messages.aux_contains(10, "new translation"));
    assert!(!messages.aux_contains(10, "retained transcript"));
    messages.exit_selection_mode();
    messages.focus_message_or_composer(10);
    assert!(messages.scroll_to_search_result(250));
    painted(&window).await;
    assert!(messages.scroll_to_search_result(490));
    painted(&window).await;
    painted(&window).await;
    assert!(messages.probe_row_materialized(10), "focused content remains intact offscreen");
    assert!(messages.scroll_to_search_result(20));
    painted(&window).await;
    assert!(matches!(messages.media_state(20), Some(ui::messages::MediaState::Done(path)) if path == document_path));
    assert!(messages.begin_media(20).is_none(), "a restored document does not download again");
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let action = opened.clone();
    messages.set_action(std::rc::Rc::new(move |event| {
        if matches!(event, ui::messages::MessageAction::Media(20)) { action.set(true); }
    }));
    let document = messages.card_widget(20).unwrap().downcast::<gtk::Button>().unwrap();
    assert!(document.is_sensitive());
    document.emit_clicked();
    assert!(opened.get(), "restored document opens its own cached file");
    assert_eq!(messages.messages().len(), 500, "the full history remains available");
    println!("history cache state preservation: PASS");
    window.close();
    backend.shutdown().await;
}

async fn sticker_navigation_check(directory: &Path) {
    let settings = SettingsStore::new();
    let effects = ui::anim::Effects::new(settings.clone());
    let backend = Tg::spawn_mock();
    let messages = ui::messages::MessagesView::new(effects);
    messages.set_player_settings(settings);
    messages.set_probe(true);
    ui::lottie::set_animated_stickers_enabled(true);
    let path = directory.join("navigation-sticker.tgs");
    std::fs::write(&path, include_bytes!("../src/tg/fixtures/fire.tgs")).unwrap();
    let window = gtk::Window::builder().default_width(800).default_height(720)
        .child(&messages.widget).build();
    window.add_css_class("omg-window");
    window.present();
    painted(&window).await;
    for count in [14, 500] {
        messages.reset_chat(1, "Sticker navigation", 1);
        messages.set_chat_summary(&ChatSummary { id: 1, title: "Sticker navigation".into(),
            kind: ChatKind::Group, ..Default::default() }, &backend);
        let mut data = history(count);
        let last = data.last_mut().unwrap();
        last.media = Some(MediaKind::Sticker);
        last.doc_name = Some("fire.tgs".into());
        last.text.clear();
        messages.finish_initial(data);
        let (_, generation) = messages.begin_media(count).unwrap();
        assert!(messages.finish_lottie(count, generation, path.clone()));
        until("sticker ready", || messages.lottie(count).is_some_and(|sticker| sticker.is_ready())).await;
        let sticker = messages.lottie(count).unwrap();
        for round in 0..12 {
            assert!(messages.scroll_to_message(1));
            until("sticker pauses offscreen", || !messages.row_visible(count) && !sticker.is_animating()).await;
            let paused = sticker.frames_shown();
            // Exercise changed wrapping as well as already-allocated jumps.
            if round % 3 == 0 { window.set_default_size(if round % 2 == 0 { 760 } else { 800 }, 720); }
            assert!(messages.scroll_to_message(count));
            let start = Instant::now();
            while !(messages.row_visible(count) && sticker.frames_shown() > paused) {
                assert!(start.elapsed() < Duration::from_secs(3),
                    "sticker failed to resume: count={count} round={round} visible={} animating={} frames={paused}->{} {}",
                    messages.row_visible(count), sticker.is_animating(), sticker.frames_shown(), messages.probe_scroll_state(count));
                glib::timeout_future(Duration::from_millis(5)).await;
            }
        }
        println!("sticker navigation: PASS (12 returns, {count} messages)");
    }
    window.close();
    backend.shutdown().await;
}

async fn services(directory: &Path) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let cache = history_cache::HistoryCache::new(9001);
    let page = history(50);
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    let mut misses = Vec::new();
    for i in 0..30 {
        let revision = cache.revision(1);
        let start = Instant::now();
        cache.put(1, page.clone(), revision);
        cache.flush().await;
        writes.push(elapsed(start));
        let start = Instant::now();
        assert_eq!(cache.get(1).await.len(), 50);
        reads.push(elapsed(start));
        let start = Instant::now();
        assert!(cache.get(90000 + i).await.is_empty());
        misses.push(elapsed(start));
    }
    sample("history_cache_50_write_flush", &writes);
    sample("history_cache_50_read_os_warm", &reads);
    sample("history_cache_miss", &misses);
    let start = Instant::now();
    let archive = archive::Archive::open(9001).await.unwrap();
    sample("archive_new_database_open", &[elapsed(start)]);
    let mut inserts = Vec::new();
    let mut versions = Vec::new();
    for batch in 0..10 {
        let mut rows = history(1000);
        for row in &mut rows {
            row.id += batch * 1000;
        }
        let last = rows.last().unwrap().id;
        let start = Instant::now();
        for row in rows {
            archive.record(row);
        }
        // This ordered query is a barrier behind the queued write batch.
        let _ = archive.versions(1, last).await;
        inserts.push(elapsed(start));
        let start = Instant::now();
        let _ = archive.versions(1, last).await;
        versions.push(elapsed(start));
    }
    sample("archive_1000_message_batch_and_barrier", &inserts);
    sample("archive_version_lookup_10000_rows", &versions);
    let start = Instant::now();
    assert_eq!(
        archive
            .mark_deleted(Some(1), (1..=100).collect())
            .await
            .len(),
        100
    );
    sample("archive_mark_100_deleted", &[elapsed(start)]);
    let mut deleted = Vec::new();
    for _ in 0..30 {
        let start = Instant::now();
        assert_eq!(archive.deleted_between(1, 1, 1000).await.len(), 100);
        deleted.push(elapsed(start));
    }
    sample("archive_query_100_deleted", &deleted);
    let photo = directory.join("synthetic-12mp.jpg");
    let image = image::RgbImage::from_fn(4000, 3000, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
    });
    let png = directory.join("synthetic-12mp.png");
    image.save(&png).unwrap();
    assert!(
        std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-y", "-i"])
            .arg(&png)
            .args(["-frames:v", "1", "-q:v", "3"])
            .arg(&photo)
            .status()
            .unwrap()
            .success()
    );
    let mut decode = Vec::new();
    let mut reuse = Vec::new();
    let mut full = Vec::new();
    for _ in 0..15 {
        media_image::clear_cache();
        let start = Instant::now();
        let texture = media_image::preview(photo.clone(), 720, false)
            .await
            .unwrap();
        assert_eq!((texture.width(), texture.height()), (720, 540));
        decode.push(elapsed(start));
        let start = Instant::now();
        let reused = media_image::preview(photo.clone(), 720, false)
            .await
            .unwrap();
        assert_eq!(texture, reused);
        reuse.push(elapsed(start));
        let path = photo.clone();
        let start = Instant::now();
        let full_image = gtk::gio::spawn_blocking(move || gtk::gdk::Texture::from_filename(path))
            .await
            .unwrap()
            .unwrap();
        assert_eq!((full_image.width(), full_image.height()), (4000, 3000));
        full.push(elapsed(start));
    }
    sample("photo_12mp_to_720px_uncached_decode", &decode);
    sample("photo_720px_memory_cache_hit", &reuse);
    sample("photo_12mp_full_texture_decode", &full);
    for (label, video, source, seconds, codec, extension) in [
        (
            "voice_30s_opus",
            false,
            "sine=frequency=440:sample_rate=48000",
            30,
            "libopus",
            "ogg",
        ),
        (
            "video_10s_h264_720p30",
            true,
            "testsrc2=size=1280x720:rate=30",
            10,
            "libx264",
            "mp4",
        ),
    ] {
        let path = directory.join(format!("{label}.{extension}"));
        let mut ffmpeg = std::process::Command::new("ffmpeg");
        ffmpeg.args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            source,
            "-t",
            &seconds.to_string(),
        ]);
        ffmpeg.args([if video { "-c:v" } else { "-c:a" }, codec]);
        if video {
            ffmpeg.args(["-preset", "ultrafast", "-pix_fmt", "yuv420p"]);
        }
        assert!(ffmpeg.arg(&path).status().unwrap().success());
        let mut first = Vec::new();
        let mut total = Vec::new();
        for _ in 0..10 {
            let (a, b) = decode_media(&path, video);
            first.push(a);
            total.push(b);
        }
        sample(&format!("{label}_first_decoded_buffer"), &first);
        sample(&format!("{label}_full_decode_unpaced"), &total);
        println!(
            "{}",
            serde_json::json!({"metric":format!("{label}_encoded_bytes"),"value":std::fs::metadata(path).unwrap().len()})
        );
    }
    let engine = ui::lottie_backend::Engine::new().unwrap();
    for size in [64, 192, 384] {
        let start = Instant::now();
        let mut animation = engine
            .load(include_bytes!("../src/tg/fixtures/fire.tgs"), size)
            .unwrap();
        sample(&format!("sticker_{size}px_parse"), &[elapsed(start)]);
        let mut times = Vec::new();
        for frame in 0..120 {
            let start = Instant::now();
            std::hint::black_box(animation.render(frame % 60).unwrap());
            times.push(elapsed(start));
        }
        sample(&format!("sticker_{size}px_frame_raster"), &times);
    }
    let text = "Rich message with bold, italic and code text. ".repeat(50);
    let spans = vec![
        Span {
            start: 0,
            end: 12,
            kind: SpanKind::Bold,
        },
        Span {
            start: 15,
            end: 28,
            kind: SpanKind::Italic,
        },
    ];
    let mut markup = Vec::new();
    let mut summary = Vec::new();
    for _ in 0..30 {
        let start = Instant::now();
        for _ in 0..100 {
            std::hint::black_box(ui::markup::render(&text, &spans));
        }
        markup.push(elapsed(start) / 100.0);
        let data = history(2000);
        let start = Instant::now();
        let mut range = ai::summary::Range::new(1, 1, 2000).unwrap();
        range.add(data).unwrap();
        let rows = range.finish().unwrap();
        let text = rows
            .iter()
            .map(|m| ai::summary::entry(m, None).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::hint::black_box(ai::summary::chunks(&text).unwrap());
        summary.push(elapsed(start));
    }
    sample("markup_2250_chars_two_spans_sync", &markup);
    sample("summary_2000_messages_prepare_sync", &summary);
    println!(
        "{}",
        serde_json::json!({"metric":"services_resources","resources":perf_support::resources()})
    );
    // Drop the runtime outside an async runtime context; no real account data.
    drop(_entered);
    runtime.shutdown_background();
}

fn main() {
    let start = Instant::now();
    assert!(
        std::env::var("WAYLAND_DISPLAY")
            .unwrap_or_default()
            .starts_with("wayland-omg-"),
        "bin/headless is required"
    );
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ui".into());
    let state_root = std::env::var_os("OMG_PERF_STATE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = state_root.join(format!("omg-performance-audit-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    // All state is synthetic and process-local; set before creating threads.
    unsafe {
        for (key, file) in [
            ("OMG_SETTINGS_PATH", "settings.toml"),
            ("OMG_UISTATE_PATH", "ui.toml"),
            ("OMG_STATUS_PATH", "status.json"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
        ] {
            std::env::set_var(key, directory.join(file));
        }
        std::env::set_var("OMG_MOCK_AI", "1");
    }
    gtk::init().unwrap();
    let _theme = omarchygram::theme::ThemeManager::attach(&gtk::gdk::Display::default().unwrap());
    println!(
        "{}",
        serde_json::json!({"mode":mode,"version":env!("CARGO_PKG_VERSION")})
    );
    glib::MainContext::default().block_on(async {
        match mode.as_str() {
            "startup" => startup(start).await,
            "ui" => native_ui().await,
            "history-cache-check" => history_cache_check().await,
            "sticker-navigation-check" => sticker_navigation_check(&directory).await,
            "services" => services(&directory).await,
            _ => panic!("expected startup, ui, services, history-cache-check, or sticker-navigation-check"),
        }
    });
    let _ = std::fs::remove_dir_all(directory);
}
