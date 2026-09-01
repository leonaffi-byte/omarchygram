//! Omarchy theme bridge: colors.toml -> GTK CSS, with live re-theme on switch.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

const STYLE_TEMPLATE: &str = include_str!("style.css");

/// Used when Omarchy is absent or a theme lacks a key (neutral dark palette).
const DEFAULTS: &[(&str, &str)] = &[
    ("mode", "dark"),
    ("background", "#1a1a1a"),
    ("dark_background", "#131313"),
    ("darker_background", "#0d0d0d"),
    ("lighter_background", "#2a2a2a"),
    ("foreground", "#c8c8c8"),
    ("light_foreground", "#8a8a8a"),
    ("muted", "#666666"),
    ("accent", "#7a9464"),
    ("selection", "#383838"),
    ("red", "#a05442"),
];

fn omarchy_state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("omarchy/current")
}

pub fn load_colors() -> BTreeMap<String, String> {
    let mut colors: BTreeMap<String, String> = DEFAULTS
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let path = omarchy_state_dir().join("theme/colors.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return colors;
    };
    let Ok(parsed) = text.parse::<toml::Table>() else {
        return colors;
    };
    for (key, value) in colors.iter_mut() {
        if let Some(toml::Value::String(s)) = parsed.get(key.as_str()) {
            if !s.is_empty() {
                *value = s.clone();
            }
        }
    }
    colors
}

pub fn build_css(colors: &BTreeMap<String, String>) -> String {
    let mut css = STYLE_TEMPLATE.to_string();
    for (key, value) in colors {
        css = css.replace(&format!("${key}"), value);
    }
    css
}

/// Applies the theme CSS app-wide and re-applies when Omarchy switches themes.
pub struct ThemeManager {
    provider: gtk::CssProvider,
    // Held so the monitor isn't dropped (dropping cancels it).
    _monitor: Option<gio::FileMonitor>,
}

impl ThemeManager {
    pub fn attach(display: &gtk::gdk::Display) -> Rc<Self> {
        let provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let monitor = {
            let name_file = omarchy_state_dir().join("theme.name");
            if name_file.parent().is_some_and(|p| p.exists()) {
                gio::File::for_path(&name_file)
                    .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
                    .ok()
            } else {
                None // not an Omarchy machine; static defaults are fine
            }
        };

        let manager = Rc::new(ThemeManager {
            provider,
            _monitor: monitor,
        });
        manager.reload();

        if let Some(monitor) = &manager._monitor {
            // A theme switch rewrites several files; coalesce the burst into one reload.
            let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
            let weak = Rc::downgrade(&manager);
            monitor.connect_changed(move |_, _, _, _| {
                if let Some(id) = debounce.borrow_mut().take() {
                    id.remove();
                }
                let weak = weak.clone();
                let debounce_inner = debounce.clone();
                let id = glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                    debounce_inner.borrow_mut().take();
                    if let Some(manager) = weak.upgrade() {
                        manager.reload();
                    }
                });
                *debounce.borrow_mut() = Some(id);
            });
        }
        manager
    }

    pub fn reload(&self) {
        let colors = load_colors();
        self.provider.load_from_string(&build_css(&colors));
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(
                colors.get("mode").map(String::as_str) != Some("light"),
            );
        }
    }
}
