"""Entrypoint: GLib/asyncio loop setup, window shell, theme attach."""

import argparse
import asyncio
import sys
import warnings

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.events import GLibEventLoopPolicy  # noqa: E402
from gi.repository import Gdk, GLib, Gtk  # noqa: E402

from omarchygram import APP_ID, __version__  # noqa: E402
from omarchygram.theme.omarchy import ThemeManager  # noqa: E402


class App(Gtk.Application):
    def __init__(self, smoke: bool, probe: bool) -> None:
        super().__init__(application_id=APP_ID)
        self._smoke = smoke
        self._probe = probe
        self._theme: ThemeManager | None = None

    def do_activate(self) -> None:
        win = self.props.active_window
        if win is None:
            win = Gtk.ApplicationWindow(application=self, title="Omarchygram")
            win.set_default_size(960, 640)
            win.add_css_class("omg-window")
            self._theme = ThemeManager(Gdk.Display.get_default())

            if self._smoke:
                from omarchygram.tg.mock import MockClient

                client = MockClient()
            else:
                from omarchygram.tg.client import TgClient

                client = TgClient()

            from omarchygram.ui.shell import Shell

            shell = Shell(client)
            win.set_child(shell)
            asyncio.get_event_loop().create_task(shell.start())

        win.present()
        if self._probe:
            GLib.timeout_add(1500, self.quit)


def run() -> None:
    parser = argparse.ArgumentParser(prog="omarchygram")
    parser.add_argument("--smoke", action="store_true", help="offline mode with mock data (no Telegram login)")
    parser.add_argument("--probe", action="store_true", help=argparse.SUPPRESS)  # open, render, auto-quit
    parser.add_argument("--version", action="version", version=__version__)
    args = parser.parse_args()

    # PyGObject's GLib loop integration still ships as a policy; silence the
    # 3.14 deprecation until upstream offers the replacement API.
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", DeprecationWarning)
        asyncio.set_event_loop_policy(GLibEventLoopPolicy())

    app = App(smoke=args.smoke, probe=args.probe)
    raise SystemExit(app.run([sys.argv[0]]))
