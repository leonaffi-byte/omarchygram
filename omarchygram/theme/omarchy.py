"""Omarchy theme bridge: colors.toml -> GTK CSS, with live re-theme on switch."""

import string
import tomllib
from importlib import resources
from pathlib import Path

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gio, GLib, Gtk  # noqa: E402

OMARCHY_STATE = Path("~/.local/state/omarchy/current").expanduser()
COLORS_FILE = OMARCHY_STATE / "theme" / "colors.toml"
THEME_NAME_FILE = OMARCHY_STATE / "theme.name"

# Used when Omarchy is absent or a theme lacks a key (neutral dark palette).
DEFAULTS = {
    "mode": "dark",
    "background": "#1a1a1a",
    "dark_background": "#131313",
    "darker_background": "#0d0d0d",
    "lighter_background": "#2a2a2a",
    "foreground": "#c8c8c8",
    "light_foreground": "#8a8a8a",
    "muted": "#666666",
    "accent": "#7a9464",
    "selection": "#383838",
    "red": "#a05442",
}


def load_colors() -> dict[str, str]:
    colors = dict(DEFAULTS)
    try:
        with open(COLORS_FILE, "rb") as f:
            data = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return colors
    for key in colors:
        value = data.get(key)
        if isinstance(value, str) and value:
            colors[key] = value
    return colors


def build_css(colors: dict[str, str]) -> str:
    template = resources.files("omarchygram.theme").joinpath("style.css").read_text()
    return string.Template(template).substitute(colors)


class ThemeManager:
    """Applies the theme CSS app-wide and re-applies when Omarchy switches themes."""

    def __init__(self, display) -> None:
        self._provider = Gtk.CssProvider()
        Gtk.StyleContext.add_provider_for_display(
            display, self._provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )
        self._debounce_id = 0
        self._monitor = None
        self.reload()
        self._watch()

    def reload(self) -> None:
        colors = load_colors()
        self._provider.load_from_string(build_css(colors))
        settings = Gtk.Settings.get_default()
        if settings is not None:
            settings.set_property(
                "gtk-application-prefer-dark-theme", colors.get("mode") != "light"
            )

    def _watch(self) -> None:
        if not THEME_NAME_FILE.parent.exists():
            return  # not an Omarchy machine; static defaults are fine
        gfile = Gio.File.new_for_path(str(THEME_NAME_FILE))
        self._monitor = gfile.monitor_file(Gio.FileMonitorFlags.NONE, None)
        self._monitor.connect("changed", self._on_changed)

    def _on_changed(self, *_args) -> None:
        # A theme switch rewrites several files; coalesce the burst into one reload.
        if self._debounce_id:
            GLib.source_remove(self._debounce_id)
        self._debounce_id = GLib.timeout_add(150, self._do_reload)

    def _do_reload(self) -> bool:
        self._debounce_id = 0
        self.reload()
        return GLib.SOURCE_REMOVE
