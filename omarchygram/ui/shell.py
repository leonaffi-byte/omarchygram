"""Placeholder shell — replaced by the real UI (see specs/spec-ui.md)."""

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk  # noqa: E402

from omarchygram.tg.client import AuthState  # noqa: E402
from omarchygram.tg.config import SETUP_HELP  # noqa: E402


class Shell(Gtk.Box):
    def __init__(self, client) -> None:
        super().__init__(orientation=Gtk.Orientation.VERTICAL)
        self._client = client
        self._label = Gtk.Label(label="Connecting…")
        self._label.add_css_class("omg-empty-state")
        self._label.set_vexpand(True)
        self.append(self._label)

    async def start(self) -> None:
        state = await self._client.start()
        if state is AuthState.NEED_CREDENTIALS:
            self._label.set_label(SETUP_HELP)
        else:
            self._label.set_label(f"UI under construction — auth state: {state.name}")
