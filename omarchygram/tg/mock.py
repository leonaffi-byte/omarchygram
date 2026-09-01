"""Offline stand-in for TgClient (--smoke mode). Orchestrator-owned.

Same surface as TgClient so the UI cannot tell them apart. Lets the app run,
be screenshotted, and be reviewed without Telegram credentials.
"""

import asyncio
import datetime
import os
from collections.abc import Callable

from omarchygram.tg.client import AuthState, ChatSummary, Message


def _t(minutes_ago: int) -> datetime.datetime:
    return datetime.datetime.now().astimezone() - datetime.timedelta(minutes=minutes_ago)


class MockClient:
    def __init__(self) -> None:
        self.state = AuthState.NEED_CREDENTIALS
        self._on_new_message: Callable[[Message], None] | None = None
        self._next_id = 1000
        self._history: dict[int, list[Message]] = {
            1: [
                Message(101, 1, "Marta", "did you see the fog this morning", _t(95), False),
                Message(102, 1, "You", "yeah, rode through it on the way to work", _t(93), True),
                Message(103, 1, "Marta", "send pics next time", _t(90), False),
                Message(104, 1, "Marta", "also are we still on for thursday?", _t(12), False),
            ],
            2: [
                Message(201, 2, "Deni", "the build is green again", _t(340), False),
                Message(202, 2, "You", "what was it in the end?", _t(338), True),
                Message(203, 2, "Deni", "stale lockfile. always the lockfile", _t(335), False),
            ],
            3: [
                Message(301, 3, "Mom", "call me when you're free", _t(1500), False),
                Message(302, 3, "You", "will do, after dinner", _t(1440), True),
            ],
            4: [
                Message(401, 4, "Arch Linux ARM", "linux 7.1.9-arch1-2 has landed in core", _t(2100), False),
            ],
        }

    @property
    def is_mock(self) -> bool:
        return True

    async def start(self) -> AuthState:
        # OMG_MOCK_AUTH=1 lets the auth screens be walked offline.
        if os.environ.get("OMG_MOCK_AUTH"):
            self.state = AuthState.NEED_PHONE
        else:
            self.state = AuthState.READY
        return self.state

    async def submit_phone(self, phone: str) -> AuthState:
        self.state = AuthState.NEED_CODE
        return self.state

    async def submit_code(self, code: str) -> AuthState:
        # "2fa" exercises the password screen; anything else signs straight in.
        self.state = AuthState.NEED_PASSWORD if code.strip() == "2fa" else AuthState.READY
        return self.state

    async def submit_password(self, password: str) -> AuthState:
        self.state = AuthState.READY
        return self.state

    async def get_dialogs(self, limit: int = 50) -> list[ChatSummary]:
        out = []
        unread = {1: 1, 2: 0, 3: 0, 4: 3}
        for chat_id, msgs in self._history.items():
            last = msgs[-1]
            title = {1: "Marta", 2: "Deni", 3: "Mom", 4: "Arch Linux ARM"}[chat_id]
            out.append(ChatSummary(chat_id, title, last.text, last.timestamp, unread[chat_id]))
        out.sort(key=lambda c: c.last_time or _t(10**6), reverse=True)
        return out

    async def get_history(self, chat_id: int, limit: int = 50) -> list[Message]:
        return list(self._history.get(chat_id, []))[-limit:]

    async def send_text(self, chat_id: int, text: str) -> Message:
        self._next_id += 1
        msg = Message(self._next_id, chat_id, "You", text, _t(0), True)
        self._history.setdefault(chat_id, []).append(msg)
        asyncio.get_event_loop().call_later(1.5, self._echo, chat_id)
        return msg

    def on_new_message(self, callback: Callable[[Message], None]) -> None:
        self._on_new_message = callback

    async def disconnect(self) -> None:
        pass

    def _echo(self, chat_id: int) -> None:
        """Simulate an incoming reply so live-update paths can be exercised offline."""
        if self._on_new_message is None:
            return
        self._next_id += 1
        title = {1: "Marta", 2: "Deni", 3: "Mom", 4: "Arch Linux ARM"}.get(chat_id, "Someone")
        msg = Message(self._next_id, chat_id, title, "(mock reply) got it", _t(0), False)
        self._history.setdefault(chat_id, []).append(msg)
        self._on_new_message(msg)
