//! Frame and main-loop latency with a large animated sidebar. Run via bin/headless.
//! Modes: none, badge, overlays, all. An optional second argument supplies an
//! animation-only settings fixture; otherwise use the bundled reproduction.
use gtk4::{self as gtk, glib, prelude::*};
#[allow(dead_code)]
mod perf_support;
use omarchygram::{
    settings::{Settings, SettingsStore},
    tg::{ChatInfo, ChatKind, ChatSummary, MediaKind, Member, Msg, Reaction, Tg},
    ui::{
        anim::Effects,
        chatlist::ChatList,
        info_panel::{InfoLayout, InfoPanel},
        messages::MessagesView,
    },
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

fn cpu_seconds() -> f64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Only reads this process's CPU clock.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) };
    time.tv_sec as f64 + time.tv_nsec as f64 / 1e9
}

fn scroller(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    if let Some(scroll) = widget.downcast_ref::<gtk::ScrolledWindow>()
        && scroll.is_mapped()
        && scroll.vadjustment().upper() > scroll.vadjustment().page_size()
    {
        return Some(scroll.clone());
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(scroll) = scroller(&widget) {
            return Some(scroll);
        }
        child = widget.next_sibling();
    }
    None
}

fn percentile(values: &mut [f64], percent: usize) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    values
        .get(values.len().saturating_sub(1) * percent / 100)
        .copied()
}

fn compare_pixels(
    renderer: &gtk::gsk::Renderer,
    full: gtk::gsk::RenderNode,
    culled: gtk::gsk::RenderNode,
    bounds: gtk::graphene::Rect,
    scale: f32,
    label: &str,
) {
    // A real framebuffer has integer pixel bounds. A fractional render_texture
    // viewport would add an extra resize and no longer test the requested scale.
    let left = (bounds.x() * scale).floor();
    let top = (bounds.y() * scale).floor();
    let bounds = gtk::graphene::Rect::new(
        left,
        top,
        ((bounds.x() + bounds.width()) * scale).ceil() - left,
        ((bounds.y() + bounds.height()) * scale).ceil() - top,
    );
    let transform = gtk::gsk::Transform::new().scale(scale, scale);
    let full = gtk::gsk::TransformNode::new(&full, Some(&transform));
    let culled = gtk::gsk::TransformNode::new(&culled, Some(&transform));
    let full = renderer.render_texture(&full, Some(&bounds));
    let culled = renderer.render_texture(&culled, Some(&bounds));
    assert_eq!(
        (full.width(), full.height()),
        (culled.width(), culled.height())
    );
    let stride = full.width() as usize * 4;
    let mut reference = vec![0; stride * full.height() as usize];
    let mut actual = reference.clone();
    full.download(&mut reference, stride);
    culled.download(&mut actual, stride);
    let differences = reference
        .iter()
        .zip(&actual)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(differences, 0, "{label}: visible pixels changed");
}

async fn verify_viewport_pixels(window: &gtk::Window, messages: &MessagesView) {
    let scroll = scroller(messages.widget.upcast_ref()).expect("message scroller");
    let renderer = window.renderer().unwrap();
    let scale = window.surface().unwrap().scale() as f32;
    for fraction in [0.0, 0.17, 0.55, 0.98, 1.0] {
        let adjustment = scroll.vadjustment();
        adjustment.set_value((adjustment.upper() - adjustment.page_size()) * fraction);
        glib::timeout_future(Duration::from_millis(120)).await;
        messages.probe_request_viewport_snapshot();
        let mut pair = None;
        for _ in 0..50 {
            glib::timeout_future(Duration::from_millis(20)).await;
            pair = messages.probe_take_viewport_snapshot();
            if pair.is_some() {
                break;
            }
        }
        let (full, culled, bounds) = pair.expect("same-frame viewport snapshots");
        assert!(messages.probe_visible_content_ready(),
            "viewport fraction={fraction}: every visible slot must contain actual message content");
        compare_pixels(
            &renderer,
            full,
            culled,
            bounds,
            scale,
            &format!("viewport fraction={fraction}"),
        );
        println!("viewport pixels identical at fraction={fraction} scale={scale}");
    }
}

async fn verify_continuous_content(window: &gtk::Window, messages: &MessagesView, chats: &Rc<ChatList>) {
    // Separate from timed phases: inspecting every visible row adds work.
    for sidebar in [false, true] {
        let scroll = scroller(if sidebar { chats.widget.upcast_ref() }
            else { messages.widget.upcast_ref() }).unwrap();
        let adjustment = scroll.vadjustment();
        adjustment.set_value((adjustment.upper() - adjustment.page_size()) * 0.31);
        glib::timeout_future(Duration::from_millis(300)).await;
        let frames = Rc::new(Cell::new(0));
        let blanks = Rc::new(Cell::new(0));
        let (checked, failed, view, chats) = (frames.clone(), blanks.clone(), messages.clone(), chats.clone());
        let clock = window.frame_clock().unwrap();
        let signal = clock.connect_after_paint(move |_| {
            checked.set(checked.get() + 1);
            if !(if sidebar { chats.probe_visible_content_ready() }
                else { view.probe_visible_content_ready() }) { failed.set(failed.get() + 1); }
        });
        let last = Cell::new(None);
        let tick = window.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let elapsed = last.replace(Some(now)).map_or(0.0, |old| (now - old) as f64 / 1e6);
            adjustment.set_value(adjustment.value() + elapsed * 2000.0);
            glib::ControlFlow::Continue
        });
        glib::timeout_future(Duration::from_secs(3)).await;
        tick.remove();
        clock.disconnect(signal);
        assert!(frames.get() > 50, "continuous content check must traverse frames");
        assert_eq!(blanks.get(), 0, "sidebar={sidebar}: visible content missing in a painted frame");
        println!("continuous content sidebar={sidebar}: PASS ({} frames)", frames.get());
    }
}

fn profiler_command(path: &std::ffi::OsStr, command: &[u8]) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .expect("perf control FIFO")
        .write_all(command)
        .expect("perf control command");
}

async fn phase(
    name: &str,
    window: &gtk::Window,
    scroll: Option<gtk::ScrolledWindow>,
    effects: Rc<Effects>,
) {
    let timings = Rc::new(RefCell::new(Vec::new()));
    let work = Rc::new(RefCell::new(Vec::new()));
    let frame_start = Rc::new(Cell::new(Instant::now()));
    let clock = window.frame_clock().unwrap();
    let start = frame_start.clone();
    let before = clock.connect_before_paint(move |_| start.set(Instant::now()));
    let samples = timings.clone();
    let durations = work.clone();
    let after = clock.connect_after_paint(move |clock| {
        durations
            .borrow_mut()
            .push(frame_start.get().elapsed().as_secs_f64() * 1000.0);
        if let Some(timing) = clock.current_timings() {
            samples.borrow_mut().push(timing);
        }
    });
    // Scroll at a fixed speed on each frame, not a 16 ms timer that caps the
    // workload at 62.5 updates/s. This is a private mock widget, never desktop input.
    let scroll_tick = scroll.map(|scroll| {
        let previous = Cell::new(None);
        window.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let elapsed = previous
                .replace(Some(now))
                .map_or(0.0, |old| (now - old) as f64 / 1e6);
            effects.note_input();
            let adjustment = scroll.vadjustment();
            let end = (adjustment.upper() - adjustment.page_size()).max(0.0);
            let next = adjustment.value() + 2000.0 * elapsed;
            adjustment.set_value(if next >= end { 0.0 } else { next });
            glib::ControlFlow::Continue
        })
    });
    let delays = Rc::new(RefCell::new(Vec::<f64>::new()));
    let last = Cell::new(Instant::now());
    let samples = delays.clone();
    let timer = glib::timeout_add_local(Duration::from_millis(4), move || {
        let now = Instant::now();
        samples
            .borrow_mut()
            .push(now.duration_since(last.replace(now)).as_secs_f64() * 1000.0);
        glib::ControlFlow::Continue
    });
    let seconds = std::env::var("OMG_PERF_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(4);
    let control = std::env::var_os("OMG_PERF_CONTROL")
        .filter(|_| std::env::var("OMG_PERF_PROFILE_PHASE").ok().as_deref() == Some(name));
    if let Some(path) = &control {
        profiler_command(path, b"enable\n");
    }
    let cpu = cpu_seconds();
    let resources_start = perf_support::resources();
    let started = Instant::now();
    glib::timeout_future(Duration::from_secs(seconds)).await;
    let elapsed = started.elapsed().as_secs_f64();
    let cpu_percent = (cpu_seconds() - cpu) / elapsed * 100.0;
    if let Some(path) = &control {
        profiler_command(path, b"disable\n");
    }
    timer.remove();
    if let Some(tick) = scroll_tick {
        tick.remove();
    }
    clock.disconnect(before);
    clock.disconnect(after);
    // Presentation feedback arrives after after-paint. Retain timings until
    // that feedback can arrive, instead of equating callbacks with display.
    glib::timeout_future(Duration::from_millis(150)).await;
    let timings = timings.borrow();
    let mut presented: Vec<_> = timings
        .iter()
        .filter(|t| t.is_complete())
        .map(|t| t.presentation_time())
        .filter(|t| *t > 0)
        .collect();
    presented.sort_unstable();
    presented.dedup();
    let mut intervals: Vec<_> = presented
        .windows(2)
        .map(|t| (t[1] - t[0]) as f64 / 1000.0)
        .collect();
    let intervals_p95 = percentile(&mut intervals, 95);
    let intervals_p99 = percentile(&mut intervals, 99);
    println!(
        "{}",
        serde_json::json!({"phase": name, "seconds": elapsed, "cpu_percent": cpu_percent,
        "frame_callbacks_fps": timings.len() as f64/elapsed,
        "presented_fps": presented.len() as f64/elapsed,
        "presented_frames": presented.len(), "frames_complete": timings.iter().filter(|t| t.is_complete()).count(),
        "refresh_interval_us": timings.iter().map(|t| t.refresh_interval()).find(|t| *t > 0),
        "present_interval_p95_ms": intervals_p95, "present_interval_p99_ms": intervals_p99,
        "frame_work_p95_ms": percentile(&mut work.borrow_mut(), 95),
        "frame_work_p99_ms": percentile(&mut work.borrow_mut(), 99),
        "loop_p95_ms": percentile(&mut delays.borrow_mut(), 95),
        "loop_p99_ms": percentile(&mut delays.borrow_mut(), 99),
        "frames_over_16_7ms": intervals.iter().filter(|v| **v > 16.8).count(),
        "frames_over_33_3ms": intervals.iter().filter(|v| **v > 33.4).count(),
        "resources_start":resources_start,"resources_end":perf_support::resources()})
    );
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "none".into());
    assert!(
        ["none", "badge", "overlays", "all"].contains(&mode.as_str()),
        "expected none, badge, overlays, or all"
    );
    let mut prefs = Settings::default();
    if mode != "none" {
        let fixture = std::env::args()
            .nth(2)
            .map(|path| std::fs::read_to_string(path).unwrap())
            .unwrap_or_else(|| include_str!("fixtures/frame-effects.toml").into());
        prefs = toml::from_str(&fixture).unwrap();
        if mode == "badge" {
            prefs.animations.retain(|key, _| key == "badgepulse");
        }
        if mode == "overlays" {
            prefs
                .animations
                .retain(|key, _| ["vignette", "scanlines", "flicker"].contains(&key.as_str()));
        }
    }
    let path = std::env::temp_dir().join(format!("omg-frame-probe-{}.toml", std::process::id()));
    std::fs::write(&path, toml::to_string(&prefs).unwrap()).unwrap();
    // Before GTK/backend threads exist; never use the user's state files.
    unsafe {
        std::env::set_var("OMG_SETTINGS_PATH", &path);
        std::env::set_var("OMG_STATUS_PATH", path.with_extension("json"));
    }
    gtk::init().unwrap();
    let _theme = omarchygram::theme::ThemeManager::attach(&gtk::gdk::Display::default().unwrap());
    let settings = SettingsStore::new();
    let effects = Effects::new(settings.clone());
    let tg = Tg::spawn_mock();
    let chats = Rc::new(ChatList::new(effects.clone(), tg.clone()));
    let messages = MessagesView::new(effects.clone());
    messages.set_player_settings(settings);
    messages.set_probe(true);
    let message_count = std::env::var("OMG_PERF_MESSAGES")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);
    let media = std::env::var_os("OMG_PERF_MEDIA").is_some();
    let mut image_paths = Vec::new();
    println!("{}", serde_json::json!({"checkpoint": "empty_views", "resources": perf_support::resources()}));
    if message_count > 0 {
        messages.reset_chat(1, "Group conversation 1", 1);
        messages.set_chat_summary(
            &ChatSummary {
                id: 1,
                title: "Group conversation 1".into(),
                kind: ChatKind::Group,
                ..Default::default()
            },
            &tg,
        );
        messages.finish_initial(
            (1..=message_count)
                .map(|id| Msg {
                    id,
                    chat_id: 1,
                    sender: format!("Group participant {}", id % 12),
                    sender_id: Some(4000 + i64::from(id % 12)),
                    text: format!(
                        "Message {id}: A realistic conversation with varied wrapping. {}",
                        "Longer messages must stay readable and scroll smoothly. "
                            .repeat((id % 5) as usize)
                    ),
                    outgoing: id % 3 == 0,
                    reply_to: (id > 1 && id % 7 == 0).then_some(id - 1),
                    media: if media {
                        match id % 10 {
                            0 => Some(MediaKind::Photo),
                            4 => Some(MediaKind::Voice),
                            8 => Some(MediaKind::Document),
                            _ => None,
                        }
                    } else {
                        None
                    },
                    photo_size: (media && id % 10 == 0).then_some((640, 480)),
                    duration: (media && id % 10 == 4).then_some(25),
                    doc_name: (media && id % 10 == 8).then(|| "Conversation notes.pdf".into()),
                    doc_size: (media && id % 10 == 8).then_some(128_000),
                    reactions: if id % 4 == 0 {
                        vec![Reaction {
                            emoji: "👍".into(),
                            count: id % 9 + 1,
                            chosen: id % 8 == 0,
                        }]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                })
                .collect(),
        );
        println!("{}", serde_json::json!({"checkpoint": "history_prepared", "resources": perf_support::resources()}));
        if media {
            for id in (10..=message_count).step_by(10) {
                let pixels: Vec<u8> = (0..640 * 480)
                    .flat_map(|pixel| {
                        let x = pixel % 640 + id;
                        let y = pixel / 640 + id * 3;
                        [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]
                    })
                    .collect();
                let texture = gtk::gdk::MemoryTexture::new(
                    640,
                    480,
                    gtk::gdk::MemoryFormat::R8g8b8a8,
                    &glib::Bytes::from_owned(pixels),
                    640 * 4,
                );
                let image_path = path.with_extension(format!("{id}.png"));
                texture.save_to_png(&image_path).unwrap();
                image_paths.push(image_path.clone());
                let (_, generation) = messages.begin_media(id).unwrap();
                assert!(messages.finish_image(
                    id,
                    generation,
                    image_path.clone(),
                    texture.upcast_ref()
                ));
            }
        }
    }
    println!("{}", serde_json::json!({"checkpoint": "photo_completions", "resources": perf_support::resources()}));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&chats.widget);
    row.append(&messages.widget);
    let info = InfoPanel::new(tg.clone());
    row.append(&info.widget);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&row));
    let window = gtk::Window::builder()
        .default_width(
            std::env::var("OMG_PERF_WIDTH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1100),
        )
        .default_height(
            std::env::var("OMG_PERF_HEIGHT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(720),
        )
        .child(&overlay)
        .build();
    window.add_css_class("omg-window");
    effects.bind(&window, &overlay);
    chats.set_chats((1..=1000).map(|id| ChatSummary { id, title: format!("Group conversation {id}"), unread: (id % 9 + 1) as i32,
        last_message: "A realistic preview with enough text to fill the row and be ellipsized at the edge of the sidebar.".into(), ..Default::default() }).collect());
    println!("{}", serde_json::json!({"checkpoint": "before_present", "resources": perf_support::resources()}));
    window.present();
    println!("{}", serde_json::json!({"checkpoint": "after_present", "resources": perf_support::resources()}));
    let first_paint = Cell::new(true);
    let first_view = messages.clone();
    let initial_frame = window.frame_clock().unwrap().connect_after_paint(move |_| {
        if first_paint.replace(false) {
            println!("{}", serde_json::json!({"checkpoint": "first_paint",
                "visible_content_ready": first_view.probe_visible_content_ready(),
                "resources": perf_support::resources()}));
        }
    });
    effects.launched(&overlay);
    glib::MainContext::default().block_on(async {
        glib::timeout_future(Duration::from_secs(2)).await;
        window.frame_clock().unwrap().disconnect(initial_frame);
        println!(
            "mode={mode} scale={} renderer={} refresh_interval_us={}",
            window.scale_factor(),
            window.renderer().unwrap().type_().name(),
            window
                .frame_clock()
                .unwrap()
                .refresh_info(glib::monotonic_time())
                .0
        );
        println!(
            "width={} height={} messages={message_count} media={media}",
            window.width(),
            window.height()
        );
        let surface_scale = window.surface().unwrap().scale();
        println!("surface_scale={surface_scale}");
        if let Ok(expected) = std::env::var("OMG_PERF_EXPECT_SCALE") {
            let expected: f64 = expected.parse().unwrap();
            assert!(
                (surface_scale - expected).abs() < 0.001,
                "unexpected native surface scale"
            );
        }
        if let Some(surface) = window.surface()
            && let Some(monitor) = WidgetExt::display(&window).monitor_at_surface(&surface)
        {
            println!("output_refresh_mhz={}", monitor.refresh_rate());
        }
        let visible = chats.probe_motion_visible_rows();
        assert!(
            !visible.is_empty() && visible.len() < 40,
            "offscreen chats must not animate"
        );
        let rasters = effects.probe_vignette_rasterizations();
        phase("idle", &window, None, effects.clone()).await;
        assert_eq!(
            effects.probe_vignette_rasterizations(),
            rasters,
            "breathing must reuse the same gradient pixels"
        );
        phase(
            "sidebar_scroll",
            &window,
            Some(scroller(chats.widget.upcast_ref()).expect("sidebar scroller")),
            effects.clone(),
        )
        .await;
        assert!(
            !effects.ambient_active(),
            "active scrolling takes priority over ambient animation"
        );
        glib::timeout_future(Duration::from_millis(450)).await;
        assert!(
            effects.ambient_active(),
            "ambient animation resumes after interaction"
        );
        let after = chats.probe_motion_visible_rows();
        assert!(
            !after.is_empty() && after.len() < 40 && after != visible,
            "animated rows follow the viewport"
        );
        if message_count > 0 {
            phase(
                "message_scroll",
                &window,
                Some(scroller(messages.widget.upcast_ref()).expect("message scroller")),
                effects.clone(),
            )
            .await;
            assert_eq!(
                messages.messages().len(),
                message_count as usize,
                "scrolling preserves all messages"
            );
            if std::env::var_os("OMG_PERF_INFO").is_some() {
                let generation = info.bind(&ChatSummary {
                    id: 1,
                    title: "Group conversation 1".into(),
                    kind: ChatKind::Group,
                    ..Default::default()
                });
                assert!(
                    info.finish_info(
                        1,
                        generation,
                        &ChatInfo {
                            id: 1,
                            title: "Group conversation 1".into(),
                            kind: ChatKind::Group,
                            about:
                                "A shared space for project discussion, photos and voice messages."
                                    .into(),
                            members: Some(12),
                            ..Default::default()
                        }
                    )
                );
                assert!(
                    info.finish_members(
                        1,
                        generation,
                        0,
                        (0..12)
                            .map(|id| Member {
                                user_id: 4000 + id,
                                name: format!("Group participant {id}"),
                                ..Default::default()
                            })
                            .collect()
                    )
                );
                info.set_layout(InfoLayout::Column);
                glib::timeout_future(Duration::from_millis(500)).await;
                phase(
                    "message_scroll_with_info",
                    &window,
                    Some(scroller(messages.widget.upcast_ref()).expect("message scroller")),
                    effects.clone(),
                )
                .await;
            }
            if std::env::var_os("OMG_PERF_VERIFY").is_some() {
                verify_viewport_pixels(&window, &messages).await;
                verify_continuous_content(&window, &messages, &chats).await;
                for scale in [1.0, 1.25, 1.6, 2.0] {
                    if let Some((full, optimized, bounds)) =
                        effects.probe_scanline_comparison(scale)
                    {
                        compare_pixels(
                            &window.renderer().unwrap(),
                            full,
                            optimized,
                            bounds,
                            scale as f32,
                            &format!("scanlines scale={scale}"),
                        );
                        println!("scanline pixels identical at scale={scale}");
                    }
                }
            }
        }
        window.close();
        tg.shutdown().await;
    });
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("json"));
    for path in image_paths {
        let _ = std::fs::remove_file(path);
    }
}
