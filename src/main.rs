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

    // OMG_SMOKE_SHOT=<file.png>: render the window's content offscreen after
    // OMG_SMOKE_SHOT_DELAY_MS (default 3000) and quit. Works while the window
    // sits on another workspace — no compositor screenshot, no synthetic input.
    if smoke && !probe {
        if let Some(path) = std::env::var_os("OMG_SMOKE_SHOT") {
            let delay = std::env::var("OMG_SMOKE_SHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000u64);
            let window = window.clone();
            let app = app.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
                // A snapshot taken while a relayout is pending (a ticking
                // label, a live preview) yields an empty node: retry on later
                // frames before giving up.
                let mut ok = false;
                for _ in 0..20 {
                    ok = snapshot_window(&window, std::path::Path::new(&path));
                    if ok {
                        break;
                    }
                    let ctx = glib::MainContext::default();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
                    while std::time::Instant::now() < deadline {
                        ctx.iteration(false);
                    }
                }
                if !ok {
                    eprintln!("omarchygram: OMG_SMOKE_SHOT failed");
                }
                app.quit();
            });
        }
    }

    if probe_traversal {
        // The traversal grows with every wave (100+ steps, delete/edit demos
        // wait 2.5s each, latency variants add 0.4s per command); 90s is the
        // hard ceiling before a run counts as hung.
        // 150 s: the wave-6 traversal is 187 steps; under OMG_MOCK_LATENCY_MS=400
        // the wave-5 90 s budget silently ran out.
        glib::timeout_add_seconds_local_once(150, || {
            eprintln!("omarchygram: probe failsafe: traversal did not finish in 150 s");
            std::process::exit(1)
        });
    } else if probe {
        let app = app.clone();
        glib::timeout_add_seconds_local_once(2, move || app.quit());
    }
}

/// Render the toplevel's child into a PNG through GSK (no screen capture).
fn snapshot_window(window: &gtk::ApplicationWindow, path: &std::path::Path) -> bool {
    use gtk::prelude::NativeExt;
    let Some(child) = window.child() else {
        eprintln!("omarchygram: snapshot: window has no child");
        return false;
    };
    let Some(renderer) = window.renderer() else {
        eprintln!("omarchygram: snapshot: window has no renderer (not realized?)");
        return false;
    };
    let paintable = gtk::WidgetPaintable::new(Some(&child));
    let snapshot = gtk::Snapshot::new();
    let (w, h) = (child.width() as f64, child.height() as f64);
    if w < 1.0 || h < 1.0 {
        eprintln!("omarchygram: snapshot: child is {w}x{h}");
        return false;
    }
    gtk::prelude::PaintableExt::snapshot(&paintable, &snapshot, w, h);
    let Some(node) = snapshot.to_node() else {
        eprintln!("omarchygram: snapshot: empty render node");
        return false;
    };
    let texture = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32)));
    match texture.save_to_png(path) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("omarchygram: snapshot: save failed: {error}");
            false
        }
    }
}
