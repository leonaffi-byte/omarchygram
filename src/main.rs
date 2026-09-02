use std::rc::Rc;

use omarchygram::{config, theme, tg, ui, uistate};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

const APP_ID: &str = "dev.leoom.Omarchygram";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let smoke = args.iter().any(|a| a == "--smoke");
    let probe = args.iter().any(|a| a == "--probe");

    if smoke {
        // Never let offline demo/probe runs touch the user's real settings.
        // $XDG_RUNTIME_DIR is per-user 0700; /tmp only as a last resort.
        let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
        let tmp = base.join(format!("omarchygram-smoke-{}.toml", std::process::id()));
        // SAFETY: called before any thread is spawned (GTK/backends start below).
        unsafe { std::env::set_var("OMG_SETTINGS_PATH", &tmp) };
        // Offline AI stand-ins so probes can traverse the Assistant paths.
        unsafe { std::env::set_var("OMG_MOCK_AI", "1") };
        let ui_tmp = base.join(format!("omarchygram-smoke-{}-ui.toml", std::process::id()));
        unsafe { std::env::set_var("OMG_UISTATE_PATH", &ui_tmp) };
    }
    config::enforce_permissions();

    // Smoke/probe runs must never collapse into an already-running instance
    // (GTK would forward "activate" over D-Bus and exit 0 without running
    // anything), so they register as non-unique.
    let flags = if smoke { gtk::gio::ApplicationFlags::NON_UNIQUE } else { gtk::gio::ApplicationFlags::default() };
    let app = gtk::Application::builder().application_id(APP_ID).flags(flags).build();
    app.connect_activate(move |app| build(app, smoke, probe));
    // GTK must not see our flags.
    app.run_with_args::<&str>(&[])
}

fn build(app: &gtk::Application, smoke: bool, probe: bool) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    let state = uistate::UiState::load();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Omarchygram")
        .default_width(state.window_w)
        .default_height(state.window_h)
        .maximized(state.maximized)
        .build();
    window.add_css_class("omg-window");
    if !smoke {
        // Remember the window geometry; pane widths are saved by the shell.
        window.connect_close_request(|w| {
            let mut s = uistate::UiState::load();
            s.maximized = w.is_maximized();
            if !s.maximized {
                s.window_w = w.width();
                s.window_h = w.height();
            }
            if let Err(e) = s.save() {
                eprintln!("omarchygram: could not save ui-state: {e}");
            }
            glib::Propagation::Proceed
        });
    }

    let display = gtk::prelude::WidgetExt::display(&window);
    // Leak-free: the manager lives as long as the app; keep it alive via the window.
    let manager = theme::ThemeManager::attach(&display);
    unsafe { window.set_data("theme-manager", manager) };

    let tg = if smoke { tg::Tg::spawn_mock() } else { tg::Tg::spawn_real() };
    // In smoke mode, --probe runs a scripted UI traversal (see specs/spec-ui.md)
    // that quits the app on success; a failsafe below fails the run instead of
    // hanging. Without --smoke there is nothing scriptable, so just flash-run.
    let probe_traversal = probe && smoke;
    let shell = Rc::new(ui::shell::Shell::new(tg, probe_traversal));
    window.set_child(Some(&shell.widget));
    window.present();

    let shell_for_start = shell.clone();
    glib::MainContext::default().spawn_local(async move {
        shell_for_start.start().await;
    });
    // Keep the shell alive with the window.
    unsafe { window.set_data("shell", shell) };

    if probe_traversal {
        // The traversal grows with every wave (100+ steps, delete/edit demos
        // wait 2.5s each, latency variants add 0.4s per command); 90s is the
        // hard ceiling before a run counts as hung.
        glib::timeout_add_seconds_local_once(90, || std::process::exit(1));
    } else if probe {
        let app = app.clone();
        glib::timeout_add_seconds_local_once(2, move || app.quit());
    }
}
