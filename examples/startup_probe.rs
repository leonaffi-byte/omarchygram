//! Read-only native startup diagnostic. Always run through bin/headless.
//! Uses the existing session; reports widget counts, never chat names or text.
//! Set OMG_UISTATE_PATH to a temporary copy and OMG_STATUS_PATH to a temporary
//! file. Never run concurrently with the app or another real-session probe.
use std::{cell::Cell, rc::Rc, time::Duration};
use gtk4 as gtk;
use gtk::{glib, prelude::*};
use omarchygram::{theme, tg, ui, uistate};

fn inspect(widget: &gtk::Widget, rows: &mut usize, visible: &mut usize) {
    if widget.has_css_class("omg-chat-row") && !widget.has_css_class("omg-virtual") {
        *rows += 1;
        if widget.is_mapped() && widget.width() > 0 && widget.height() > 0 {
            *visible += 1;
        }
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        inspect(&widget, rows, visible);
        child = widget.next_sibling();
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("dev.leoom.Omarchygram.StartupProbe")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let success = Rc::new(Cell::new(false));
    let accepted = success.clone();
    let backend = tg::Tg::spawn_real();
    let active_backend = backend.clone();
    app.connect_activate(move |app| {
        let state = uistate::UiState::load();
        let window = gtk::ApplicationWindow::builder().application(app)
            .default_width(state.window_w).default_height(state.window_h).build();
        window.add_css_class("omg-window");
        let manager = theme::ThemeManager::attach(&WidgetExt::display(&window));
        let shell = Rc::new(ui::shell::Shell::new(active_backend.clone(), false));
        window.set_child(Some(&shell.widget));
        window.present();
        let app = app.clone();
        let accepted = accepted.clone();
        glib::MainContext::default().spawn_local(async move {
            shell.start().await;
            for _ in 0..30 {
                glib::timeout_future(Duration::from_secs(1)).await;
                let (mut rows, mut visible) = (0, 0);
                inspect(window.upcast_ref(), &mut rows, &mut visible);
                println!("startup: {rows} chat rows, {visible} allocated and mapped");
                if visible > 0 {
                    accepted.set(true);
                    glib::timeout_future(Duration::from_millis(500)).await;
                    if let Some(path) = std::env::var_os("OMG_STARTUP_SHOT")
                        && let Some(widget) = window.child()
                        && let Some(renderer) = window.renderer()
                    {
                        let snapshot = gtk::Snapshot::new();
                        gtk::WidgetPaintable::new(Some(&widget)).snapshot(&snapshot, widget.width() as f64, widget.height() as f64);
                        if let Some(node) = snapshot.to_node() {
                            let texture = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32)));
                            println!("snapshot saved: {}", texture.save_to_png(path).is_ok());
                        }
                    }
                    break;
                }
            }
            drop(manager);
            app.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
    glib::MainContext::default().block_on(backend.shutdown());
    if !success.get() { std::process::exit(1); }
}
