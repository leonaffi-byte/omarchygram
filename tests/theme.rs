//! Tests for the pure part of `omarchygram::theme`.
//!
//! Only `load_colors()` and `build_css()` are exercised — no `ThemeManager`,
//! no GTK, so these run headless. `load_colors()` reads the live Omarchy state
//! of this machine, so nothing here asserts a specific color from it: only
//! structural properties, plus one hand-built map where the values are known.

use std::collections::BTreeMap;

use omarchygram::theme::{build_css, load_colors};

/// The keys every `load_colors()` result carries (the built-in defaults).
/// A live theme overrides values, never the key set.
const KEYS: &[&str] = &[
    "mode",
    "background",
    "dark_background",
    "darker_background",
    "lighter_background",
    "foreground",
    "light_foreground",
    "muted",
    "accent",
    "selection",
    "red",
    "green",
    "cyan",
    "blue",
    "magenta",
    "yellow",
    "orange",
];

/// Color key -> the CSS custom property its placeholder fills, per the `:root`
/// block of `src/theme/style.css`. `mode` is absent on purpose: it drives the
/// GTK dark-theme preference, not the stylesheet.
const TOKENS: &[(&str, &str)] = &[
    ("background", "--bg"),
    ("dark_background", "--bg-dark"),
    ("darker_background", "--bg-darker"),
    ("lighter_background", "--bg-lighter"),
    ("foreground", "--fg"),
    ("light_foreground", "--fg-light"),
    ("muted", "--muted"),
    ("accent", "--accent"),
    ("selection", "--selection"),
    ("red", "--red"),
    ("green", "--green"),
    ("cyan", "--cyan"),
    ("blue", "--blue"),
    ("magenta", "--magenta"),
    ("yellow", "--yellow"),
    ("orange", "--orange"),
];

#[test]
fn load_colors_returns_the_standard_keys() {
    let colors = load_colors();
    for key in KEYS {
        assert!(
            colors.contains_key(*key),
            "load_colors() is missing the key `{key}`: {colors:?}"
        );
    }
    assert_eq!(
        colors.len(),
        KEYS.len(),
        "load_colors() returned unexpected keys: {:?}",
        colors.keys().collect::<Vec<_>>()
    );
}

#[test]
fn load_colors_values_are_colors_and_a_valid_mode() {
    let colors = load_colors();
    let mode = colors.get("mode").expect("mode key");
    assert!(
        mode == "dark" || mode == "light",
        "mode should be \"dark\" or \"light\", got {mode:?}"
    );
    for (key, value) in &colors {
        if key == "mode" {
            continue;
        }
        assert!(
            value.starts_with('#'),
            "{key} should be a hex color, got {value:?}"
        );
    }
}

#[test]
fn build_css_substitutes_every_placeholder() {
    let colors = load_colors();
    let css = build_css(&colors);

    assert!(
        !css.contains('$'),
        "the generated CSS still holds an unsubstituted placeholder"
    );

    // Each value must land on its own custom property, not merely somewhere in
    // the file — two placeholders wired to each other's color would otherwise
    // pass.
    for (key, token) in TOKENS {
        let value = colors
            .get(*key)
            .unwrap_or_else(|| panic!("load_colors() is missing the key `{key}`"));
        let declaration = format!("{token}: {value};");
        assert!(
            css.contains(&declaration),
            "expected `{declaration}` in the generated CSS ({key} is not bound to {token})"
        );
    }
}

#[test]
fn build_css_uses_the_map_it_is_given() {
    // Same key set as a real `load_colors()` result, but with values we
    // control — a distinct one per key, so a value reaching the wrong custom
    // property cannot pass.
    let mut colors: BTreeMap<String, String> = load_colors()
        .keys()
        .enumerate()
        .map(|(i, key)| (key.clone(), format!("#0000{i:02x}")))
        .collect();
    colors.insert("mode".to_string(), "dark".to_string());
    colors.insert("accent".to_string(), "#123456".to_string());

    let css = build_css(&colors);

    assert!(
        css.contains("--accent: #123456;"),
        "the accent from the given map did not reach the --accent token"
    );
    for (key, token) in TOKENS {
        let value = &colors[*key];
        let declaration = format!("{token}: {value};");
        assert!(
            css.contains(&declaration),
            "expected `{declaration}` in the generated CSS ({key} is not bound to {token})"
        );
    }
    assert!(
        !css.contains('$'),
        "the generated CSS still holds an unsubstituted placeholder"
    );
}
