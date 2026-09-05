use std::rc::Rc;

use omarchygram::{config, settings, status, theme, tg, ui, uistate};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

const APP_ID: &str = "dev.leoom.Omarchygram";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let smoke = args.iter().any(|a| a == "--smoke");
    let probe = args.iter().any(|a| a == "--probe");
    let background = args.iter().any(|a| a == "--background");
    let quit = args.iter().any(|a| a == "--quit");

    if smoke {
        // Never let offline demo/probe runs touch the user's real settings.
        // $XDG_RUNTIME_DIR is per-user 0700; /tmp only as a last resort.
        let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
        let tmp = base.join(format!("omarchygram-smoke-{}.toml", std::process::id()));
        // SAFETY: called before any thread is spawned (GTK/backends start below).
        unsafe { std::env::set_var("OMG_SETTINGS_PATH", &tmp) };
        // OMG_SMOKE_SETTINGS_SEED=<toml>: start the throwaway settings from a
        // preset (demo recordings turn animations on this way).
        if let Some(seed) = std::env::var_os("OMG_SMOKE_SETTINGS_SEED")
            && let Err(error) = std::fs::copy(&seed, &tmp)
        {
            eprintln!("omarchygram: cannot seed smoke settings: {error}");
        }
        // Offline AI stand-ins so probes can traverse the Assistant paths.
        unsafe { std::env::set_var("OMG_MOCK_AI", "1") };
        let ui_tmp = base.join(format!("omarchygram-smoke-{}-ui.toml", std::process::id()));
        // Copy explicit gate seeds into isolation before replacing the path.
        if let Some(seed) = std::env::var_os("OMG_UISTATE_PATH")
            && let Err(error) = std::fs::copy(seed, &ui_tmp)
        {
            eprintln!("omarchygram: cannot seed smoke window state: {error}");
        }
        unsafe { std::env::set_var("OMG_UISTATE_PATH", &ui_tmp) };
        // The bar-plugin status file too: probes must never touch the real one.
        let status_tmp = base.join(format!("omarchygram-smoke-{}-status.json", std::process::id()));
        unsafe { std::env::set_var("OMG_STATUS_PATH", &status_tmp) };
    }
    if !smoke { config::enforce_permissions(); }

    // Smoke/probe runs must never collapse into an already-running instance
    // (GTK would forward "activate" over D-Bus and exit 0 without running
    // anything), so they register as non-unique.
    let flags = if smoke && std::env::var_os("OMG_SMOKE_SINGLE_INSTANCE").is_none() { gtk::gio::ApplicationFlags::NON_UNIQUE } else { gtk::gio::ApplicationFlags::default() };
    let app = gtk::Application::builder().application_id(if smoke { "dev.leoom.Omarchygram.Smoke" } else { APP_ID }).flags(flags).build();
    let quit_action = gtk::gio::SimpleAction::new("quit", None);
    let weak_app = app.downgrade();
    quit_action.connect_activate(move |_, _| {
        if let Some(app) = weak_app.upgrade() { app.quit(); }
    });
    app.add_action(&quit_action);
    app.set_accels_for_action("app.quit", &["<Primary>q"]);
    if quit {
        if let Err(error) = app.register(gtk::gio::Cancellable::NONE) {
            eprintln!("omarchygram: could not contact the running app: {error}");
            return glib::ExitCode::FAILURE;
        }
        if app.is_remote() { app.activate_action("quit", None); }
        return glib::ExitCode::SUCCESS;
    }
    let backend = Rc::new(std::cell::RefCell::new(None));
    let active_backend = backend.clone();
    app.connect_activate(move |app| {
        // The Omarchy bar plugin's status file (specs/spec-bar-plugin.md §1):
        // "up" once GTK owns the main loop, "gone" on every normal exit path.
        status::set_running(true);
        if let Some(tg) = build(app, smoke, probe, background) {
            *active_backend.borrow_mut() = Some(tg);
        }
    });
    app.connect_shutdown(move |app| {
        if !smoke && let Some(window) = app.windows().into_iter().find(|w| w.widget_name() == "omarchygram-main") {
            save_geometry(&window);
        }
        status::shutdown();
    });
    // GTK must not see our flags.
    let code = app.run_with_args::<&str>(&[]);
    if let Some(tg) = backend.borrow_mut().take() {
        glib::MainContext::default().block_on(tg.shutdown());
    }
    code
}

fn save_geometry(window: &gtk::Window) {
    let mut state = uistate::UiState::load();
    state.maximized = window.is_maximized();
    if !state.maximized && window.width() > 0 && window.height() > 0 {
        state.window_w = window.width();
        state.window_h = window.height();
    }
    if let Err(error) = state.save() { eprintln!("omarchygram: could not save window state: {error}"); }
}

fn build(app: &gtk::Application, smoke: bool, probe: bool, background: bool) -> Option<tg::Tg> {
    // Hidden windows have no active-window entry. Reuse the same shell and
    // session when the normal launcher activates the background application.
    if let Some(window) = app.windows().into_iter().find(|w| w.widget_name() == "omarchygram-main") {
        window.present();
        return None;
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
    window.set_widget_name("omarchygram-main");
    {
        // Remember the window geometry; pane widths are saved by the shell.
        window.connect_close_request(move |w| {
            if !smoke { save_geometry(w.upcast_ref()); }
            if settings::load().ui.keep_running {
                w.set_visible(false);
                return glib::Propagation::Stop;
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
    let shell = Rc::new(ui::shell::Shell::new(tg.clone(), probe_traversal));
    window.set_child(Some(&shell.widget));
    if !background { window.present(); }

    let shell_for_start = shell.clone();
    let weak_window = window.downgrade();
    glib::MainContext::default().spawn_local(async move {
        shell_for_start.start().await;
        // A background launch still needs a visible login form if the saved
        // session is missing or cannot be opened.
        if background && !shell_for_start.is_ready()
            && let Some(window) = weak_window.upgrade() { window.present(); }
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
            // A snapshot taken while a relayout is pending (a ticking label,
            // a live preview) yields an empty node: retry on later main-loop
            // iterations (real frames in between) before giving up.
            fn attempt(window: gtk::ApplicationWindow, app: gtk::Application, path: std::ffi::OsString, left: u32) {
                let ok = snapshot_window(&window, std::path::Path::new(&path));
                if !ok && left > 0 {
                    glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                        attempt(window, app, path, left - 1);
                    });
                    return;
                }
                finish(&app, ok);
            }
            glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
                attempt(window, app, path, 30);
            });
        }
        // OMG_SMOKE_RECORD_DIR=<dir>: after the same delay, write frame-NNNNN.png
        // at OMG_SMOKE_RECORD_FPS (default 20) for OMG_SMOKE_RECORD_MS (default
        // 10000), then quit. Demo videos are assembled from the frames with
        // ffmpeg; same offscreen render path as the single shot.
        if let Some(dir) = std::env::var_os("OMG_SMOKE_RECORD_DIR") {
            let delay = std::env::var("OMG_SMOKE_SHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000u64);
            let fps: u64 = std::env::var("OMG_SMOKE_RECORD_FPS").ok().and_then(|v| v.parse().ok()).unwrap_or(20).clamp(1, 60);
            let total_ms: u64 = std::env::var("OMG_SMOKE_RECORD_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
            let dir = std::path::PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&dir);
            let window = window.clone();
            let app = app.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
                let started = std::time::Instant::now();
                let frame = std::rc::Rc::new(std::cell::Cell::new(0u32));
                glib::timeout_add_local(std::time::Duration::from_millis(1000 / fps), move || {
                    if started.elapsed().as_millis() as u64 >= total_ms {
                        eprintln!("omarchygram: recorded {} frames", frame.get());
                        app.quit();
                        return glib::ControlFlow::Break;
                    }
                    let path = dir.join(format!("frame-{:05}.png", frame.get()));
                    // An empty render node (relayout pending) just skips a tick.
                    if snapshot_window(&window, &path) {
                        frame.set(frame.get() + 1);
                    }
                    glib::ControlFlow::Continue
                });
            });
        }
    }
    fn finish(app: &gtk::Application, ok: bool) {
        if !ok {
            eprintln!("omarchygram: OMG_SMOKE_SHOT failed");
        }
        app.quit();
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
    Some(tg)
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
