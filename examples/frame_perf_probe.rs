//! Frame and main-loop latency with a large animated sidebar. Run via bin/headless.
//! Modes: none, badge, overlays, all. An optional second argument supplies an
//! animation-only settings fixture; otherwise use the bundled reproduction.
use gtk4::{self as gtk, glib, prelude::*};
use omarchygram::{
    settings::{Settings, SettingsStore},
    tg::{ChatSummary, Tg},
    ui::{anim::Effects, chatlist::ChatList, messages::MessagesView},
};
use std::{
    cell::RefCell,
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

async fn phase(
    name: &str,
    window: &gtk::Window,
    scroll: Option<gtk::ScrolledWindow>,
    effects: Rc<Effects>,
) {
    let frames = Rc::new(RefCell::new(Vec::<f64>::new()));
    let last_frame = Rc::new(RefCell::new(Instant::now()));
    let clock = window.frame_clock().unwrap();
    let samples = frames.clone();
    let handler = clock.connect_after_paint(move |_| {
        let now = Instant::now();
        samples
            .borrow_mut()
            .push(now.duration_since(*last_frame.borrow()).as_secs_f64() * 1000.0);
        *last_frame.borrow_mut() = now;
    });
    let delays = Rc::new(RefCell::new(Vec::<f64>::new()));
    let last = Rc::new(RefCell::new(Instant::now()));
    let samples = delays.clone();
    let timer = glib::timeout_add_local(Duration::from_millis(16), move || {
        let now = Instant::now();
        samples
            .borrow_mut()
            .push(now.duration_since(*last.borrow()).as_secs_f64() * 1000.0);
        *last.borrow_mut() = now;
        if let Some(scroll) = &scroll {
            effects.note_input();
            let adjustment = scroll.vadjustment();
            let end = (adjustment.upper() - adjustment.page_size()).max(0.0);
            let next = adjustment.value() + 32.0;
            adjustment.set_value(if next >= end { 0.0 } else { next });
        }
        glib::ControlFlow::Continue
    });
    let cpu = cpu_seconds();
    let started = Instant::now();
    glib::timeout_future(Duration::from_secs(4)).await;
    let elapsed = started.elapsed().as_secs_f64();
    timer.remove();
    clock.disconnect(handler);
    let mut delays = delays.borrow().clone();
    delays.sort_by(f64::total_cmp);
    let frame_count = frames.borrow().len();
    println!(
        "{}",
        serde_json::json!({"phase": name, "seconds": elapsed, "cpu_percent": (cpu_seconds()-cpu)/elapsed*100.0,
        "paint_fps": frame_count as f64/elapsed, "loop_p95_ms": delays[delays.len()*95/100], "loop_max_ms": delays.last()})
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
    let chats = ChatList::new(effects.clone(), tg.clone());
    let messages = MessagesView::new(effects.clone());
    messages.set_player_settings(settings);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&chats.widget);
    row.append(&messages.widget);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&row));
    let window = gtk::Window::builder()
        .default_width(1100)
        .default_height(720)
        .child(&overlay)
        .build();
    window.add_css_class("omg-window");
    effects.bind(&window, &overlay);
    chats.set_chats((1..=1000).map(|id| ChatSummary { id, title: format!("Group conversation {id}"), unread: (id % 9 + 1) as i32,
        last_message: "A realistic preview with enough text to fill the row and be ellipsized at the edge of the sidebar.".into(), ..Default::default() }).collect());
    window.present();
    effects.launched(&overlay);
    glib::MainContext::default().block_on(async {
        glib::timeout_future(Duration::from_secs(2)).await;
        println!(
            "mode={mode} scale={} renderer={}",
            window.scale_factor(),
            window.renderer().unwrap().type_().name()
        );
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
        window.close();
        tg.shutdown().await;
    });
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("json"));
}
