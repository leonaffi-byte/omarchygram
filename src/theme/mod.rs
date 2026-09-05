//! Omarchy theme bridge: colors.toml -> GTK CSS, with live re-theme on switch.

use std::cell::{Cell, RefCell};
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
    ("green", "#7a9464"),
    ("cyan", "#6a9a9a"),
    ("blue", "#6a86a8"),
    ("magenta", "#9a6a9a"),
    ("yellow", "#b8a05a"),
    ("orange", "#b87a4a"),
];

fn omarchy_state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("omarchy/current")
}

fn raw_colors() -> BTreeMap<String, String> {
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
            // Values are substituted into CSS verbatim; only accept what a
            // color can look like so a theme file cannot inject rules.
            let valid = if key == "mode" {
                s == "dark" || s == "light"
            } else {
                matches!(s.len(), 4 | 5 | 7 | 9) // #rgb #rgba #rrggbb #rrggbbaa
                    && s.starts_with('#')
                    && s[1..].chars().all(|c| c.is_ascii_hexdigit())
            };
            if valid {
                *value = s.clone();
            }
        }
    }
    colors
}

pub fn load_colors() -> BTreeMap<String, String> {
    readable_colors(raw_colors())
}

// Keep the theme's hue while correcting text contrast. UI text, including
// secondary labels, must remain readable on every conversation surface.
#[derive(Clone, Copy)]
struct Rgb([f64; 3]);
impl Rgb {
    fn parse(value: &str) -> Option<Self> {
        let hex = value.strip_prefix('#')?;
        let full = match hex.len() {
            3 | 4 => hex[..3].chars().flat_map(|c| [c, c]).collect::<String>(),
            6 | 8 => hex[..6].to_string(),
            _ => return None,
        };
        let n = u32::from_str_radix(&full, 16).ok()?;
        Some(Self([((n >> 16) & 255) as f64, ((n >> 8) & 255) as f64, (n & 255) as f64]))
    }
    fn hex(self) -> String { format!("#{:02x}{:02x}{:02x}", self.0[0].round() as u8, self.0[1].round() as u8, self.0[2].round() as u8) }
    fn mix(self, other: Self, amount: f64) -> Self { Self(std::array::from_fn(|i| self.0[i] * (1.0 - amount) + other.0[i] * amount)) }
    fn luminance(self) -> f64 {
        let linear = self.0.map(|v| { let s = v / 255.0; if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) } });
        linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
    }
    fn contrast(self, other: Self) -> f64 {
        let (a,b) = (self.luminance(),other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    fn readable(self, surfaces: &[Self], minimum: f64) -> Self {
        let score = |color: Self| surfaces.iter().map(|bg| color.contrast(*bg)).fold(f64::INFINITY, f64::min);
        let black = Self([0.0; 3]); let white = Self([255.0; 3]);
        let end = if score(black) > score(white) { black } else { white };
        for step in 0..=255 {
            let candidate = self.mix(end, f64::from(step) / 255.0);
            // Check the actual rounded CSS value, not just its float precursor.
            let candidate = Self(candidate.0.map(f64::round));
            if score(candidate) >= minimum { return candidate; }
        }
        end
    }
}

fn readable_colors(mut colors: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let rgb = |key: &str| colors.get(key).and_then(|v| Rgb::parse(v)).unwrap_or(Rgb([26.0; 3]));
    let bg = rgb("background");
    let fg = rgb("foreground").readable(&[bg], 7.0);
    let accent = rgb("accent");
    let light = bg.contrast(Rgb([0.0; 3])) > bg.contrast(Rgb([255.0; 3]));
    let text_end = if light { Rgb([0.0; 3]) } else { Rgb([255.0; 3]) };
    // Partial Omarchy palettes often omit surface tokens. Derive coherent
    // surfaces for light themes instead of inheriting the dark fallback set.
    let dark = if light { bg.mix(fg, 0.03) } else { bg.mix(Rgb([0.0; 3]), 0.22) };
    let darker = if light { bg.mix(fg, 0.06) } else { bg.mix(Rgb([0.0; 3]), 0.42) };
    let lighter = bg.mix(fg, 0.065);
    let selection = bg.mix(accent, 0.16);
    let surface = |color: Rgb| {
        let rounded = Rgb(color.0.map(f64::round));
        if rounded.contrast(text_end) >= 4.5 { rounded } else { rounded.readable(&[text_end], 4.5) }
    };
    let (dark, darker, lighter, selection) = (surface(dark), surface(darker), surface(lighter), surface(selection));
    let surfaces = [bg, dark, darker, lighter, selection];
    let changes = [
        ("dark_background",dark), ("darker_background",darker), ("lighter_background",lighter),
        ("selection",selection), ("foreground",fg.readable(&surfaces, 7.0)),
        ("muted",rgb("muted").readable(&surfaces, 4.5)),
        ("light_foreground",rgb("light_foreground").readable(&surfaces, 4.5)),
        ("accent",accent.readable(&surfaces, 4.5)),
        ("red",rgb("red").readable(&surfaces, 4.5)),
    ];
    for (key,value) in changes { colors.insert(key.into(), value.hex()); }
    for key in ["green", "cyan", "blue", "magenta", "yellow", "orange"] {
        if let Some(value) = colors.get(key).and_then(|v| Rgb::parse(v)) {
            colors.insert(key.into(), value.readable(&surfaces, 4.5).hex());
        }
    }
    let accent = Rgb::parse(&colors["accent"]).unwrap();
    let on_accent = fg.readable(&[accent], 4.5);
    colors.insert("on_accent".into(), on_accent.hex());
    colors
}

pub fn build_css(colors: &BTreeMap<String, String>) -> String {
    let mut css = STYLE_TEMPLATE.to_string();
    for (key, value) in colors {
        css = css.replace(&format!("${key}"), value);
    }
    css
}

thread_local! {
    static MANAGERS: RefCell<Vec<std::rc::Weak<ThemeManager>>> = const { RefCell::new(Vec::new()) };
    static TEXT_SCALE: Cell<u32> = const { Cell::new(100) };
}

pub fn set_text_scale(percent: u32) {
    let percent = percent.clamp(85, 150);
    if !TEXT_SCALE.with(|scale| scale.replace(percent) != percent) { return; }
    MANAGERS.with(|managers| {
        managers.borrow_mut().retain(|weak| {
            if let Some(manager) = weak.upgrade() { manager.reload(); true } else { false }
        });
    });
}

fn scaled_css(css: &str, percent: u32) -> String {
    css.lines().map(|line| {
        let Some((prefix, rest)) = line.split_once("font-size:") else { return line.to_string() };
        let Some((size, suffix, unit)) = ["px", "pt"].into_iter().find_map(|unit| {
            let (size, suffix) = rest.trim_start().split_once(unit)?;
            Some((size.parse::<f64>().ok()?, suffix, unit))
        }) else { return line.to_string() };
        format!("{prefix}font-size: {:.2}{unit}{suffix}", size * f64::from(percent) / 100.0)
    }).collect::<Vec<_>>().join("\n")
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
        MANAGERS.with(|managers| managers.borrow_mut().push(Rc::downgrade(&manager)));
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
        self.provider.load_from_string(&TEXT_SCALE.with(|scale| scaled_css(&build_css(&colors), scale.get())));
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(
                colors.get("mode").map(String::as_str) != Some("light"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_scale_handles_points_and_pixels_without_scaling_spacing() {
        assert_eq!(scaled_css("label { font-size: 10.5pt; padding: 8px; }\nbutton { font-size: 16px; }", 150),
            "label { font-size: 15.75pt; padding: 8px; }\nbutton { font-size: 24.00px; }");
    }
    #[test]
    fn secondary_text_remains_readable_in_dark_and_light_palettes() {
        for (background, foreground, muted) in [("#1a1a1a", "#c8c8c8", "#333"), ("#fff", "#222", "#ccc"), ("#777", "#999", "#888"), ("#888", "#333", "#555")] {
            let mut palette: BTreeMap<_,_> = DEFAULTS.iter().map(|(k,v)| (k.to_string(),v.to_string())).collect();
            for (key,value) in [("background",background),("foreground",foreground),("muted",muted)] { palette.insert(key.into(),value.into()); }
            let palette = readable_colors(palette);
            for text in ["foreground","muted","light_foreground","accent","red","green","blue","cyan","orange","yellow","magenta"] {
                for surface in ["background","dark_background","darker_background","lighter_background","selection"] {
                    assert!(Rgb::parse(&palette[text]).unwrap().contrast(Rgb::parse(&palette[surface]).unwrap()) >= 4.5, "{text} on {surface}");
                }
            }
            assert!(Rgb::parse(&palette["on_accent"]).unwrap().contrast(Rgb::parse(&palette["accent"]).unwrap()) >= 4.5);
        }
    }
}
