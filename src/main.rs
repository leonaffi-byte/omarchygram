use std::rc::Rc;

use omarchygram::{theme, tg, ui};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

const APP_ID: &str = "dev.leoom.Omarchygram";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let smoke = args.iter().any(|a| a == "--smoke");
    let probe = args.iter().any(|a| a == "--probe");

    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| build(app, smoke, probe));
    // GTK must not see our flags.
    app.run_with_args::<&str>(&[])
}

fn build(app: &gtk::Application, smoke: bool, probe: bool) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Omarchygram")
        .default_width(960)
        .default_height(640)
        .build();
    window.add_css_class("omg-window");

    let display = gtk::prelude::WidgetExt::display(&window);
    // Leak-free: the manager lives as long as the app; keep it alive via the window.
    let manager = theme::ThemeManager::attach(&display);
    unsafe { window.set_data("theme-manager", manager) };

    let tg = if smoke { tg::Tg::spawn_mock() } else { tg::Tg::spawn_real() };
    let shell = Rc::new(ui::shell::Shell::new(tg));
    window.set_child(Some(&shell.widget));
    window.present();

    let shell_for_start = shell.clone();
    glib::MainContext::default().spawn_local(async move {
        shell_for_start.start().await;
    });
    // Keep the shell alive with the window.
    unsafe { window.set_data("shell", shell) };

    if probe {
        let app = app.clone();
        glib::timeout_add_seconds_local_once(2, move || app.quit());
    }
}
