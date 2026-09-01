"""Offline stand-in for TgClient (--smoke mode). Orchestrator-owned.

Same surface as TgClient so the UI cannot tell them apart. Lets every feature
(media, replies, edits, reactions, typing, pagination) be exercised and
screenshotted without Telegram credentials.
"""

import asyncio
import datetime
import glob
import os
from collections.abc import Callable
from pathlib import Path

from omarchygram.tg.client import AuthState, ChatSummary, Message, Reaction

_TITLES = {1: "Marta", 2: "Deni", 3: "Mom", 4: "Arch Linux ARM"}


def _t(minutes_ago: int) -> datetime.datetime:
    return datetime.datetime.now().astimezone() - datetime.timedelta(minutes=minutes_ago)


def _sample_image() -> Path | None:
    hits = sorted(glob.glob("/usr/share/omarchy/themes/*/preview.png"))
    return Path(hits[0]) if hits else None


class MockClient:
    def __init__(self) -> None:
        self.state = AuthState.NEED_CREDENTIALS
        self._on_new_message: Callable[[Message], None] | None = None
        self._on_message_changed: Callable[[Message], None] | None = None
        self._on_typing: Callable[[int, str], None] | None = None
        self._next_id = 1000
        self._unread = {1: 1, 2: 0, 3: 0, 4: 3}
        self._history: dict[int, list[Message]] = {
            1: [
                Message(101, 1, "Marta", "did you see the fog this morning", _t(95), False),
                Message(102, 1, "You", "yeah, rode through it on the way to work", _t(93), True),
                Message(103, 1, "Marta", "", _t(91), False, media="photo"),
                Message(
                    104, 1, "Marta", "send pics next time", _t(90), False,
                    reactions=[Reaction("👍", 1)],
                ),
                Message(105, 1, "You", "that one's from the pass", _t(88), True, reply_to=103),
                Message(106, 1, "Marta", "", _t(85), False, media="sticker"),
                Message(107, 1, "Marta", "also are we still on for thursday?", _t(12), False),
            ],
            2: [
                Message(201, 2, "Deni", "the build is green again", _t(340), False),
                Message(202, 2, "You", "what was it in the end?", _t(338), True),
                Message(
                    203, 2, "Deni", "stale lockfile. always the lockfile", _t(335), False,
                    edited=True,
                ),
                Message(
                    204, 2, "Deni", "", _t(330), False,
                    media="document", doc_name="ci-log.txt",
                ),
            ],
            3: [
                Message(301, 3, "Mom", "", _t(1502), False, media="voice"),
                Message(302, 3, "Mom", "call me when you're free", _t(1500), False),
                Message(303, 3, "You", "will do, after dinner", _t(1440), True),
            ],
            4: [
                Message(401, 4, "Arch Linux ARM", "linux 7.1.9-arch1-2 has landed in core", _t(2100), False),
            ],
        }

    @property
    def is_mock(self) -> bool:
        return True

    # ---- auth ----

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

    # ---- reading ----

    async def get_dialogs(self, limit: int = 50) -> list[ChatSummary]:
        out = []
        for chat_id, msgs in self._history.items():
            last = msgs[-1]
            preview = last.text or {
                "photo": "[photo]", "sticker": "[sticker]",
                "voice": "[voice message]", "document": "[file]",
            }.get(last.media or "", "")
            out.append(
                ChatSummary(chat_id, _TITLES[chat_id], preview, last.timestamp,
                            self._unread.get(chat_id, 0))
            )
        out.sort(key=lambda c: c.last_time or _t(10**6), reverse=True)
        return out

    async def get_history(
        self, chat_id: int, limit: int = 50, before_id: int | None = None
    ) -> list[Message]:
        msgs = self._history.get(chat_id, [])
        if before_id is not None:
            # Fabricate one older page so pagination can be exercised, then stop.
            if before_id == (msgs[0].id if msgs else 0) and chat_id == 1:
                return [
                    Message(90, 1, "Marta", "older message from last week", _t(10000), False),
                    Message(91, 1, "You", "yep, scroll-back works", _t(9990), True),
                ]
            return []
        return list(msgs)[-limit:]

    async def download_media(self, chat_id: int, msg_id: int) -> Path | None:
        await asyncio.sleep(0.3)  # simulate network so placeholders are visible
        for m in self._history.get(chat_id, []):
            if m.id == msg_id and m.media in ("photo", "sticker"):
                return _sample_image()
        return None

    # ---- writing ----

    async def send_text(self, chat_id: int, text: str, reply_to: int | None = None) -> Message:
        self._next_id += 1
        msg = Message(self._next_id, chat_id, "You", text, _t(0), True, reply_to=reply_to)
        self._history.setdefault(chat_id, []).append(msg)
        loop = asyncio.get_event_loop()
        loop.call_later(0.8, self._typing, chat_id)
        loop.call_later(2.0, self._echo, chat_id)
        return msg

    async def send_file(self, chat_id: int, path: str, caption: str = "") -> Message:
        self._next_id += 1
        name = Path(path).name
        media = "photo" if name.lower().endswith((".png", ".jpg", ".jpeg", ".webp")) else "document"
        msg = Message(self._next_id, chat_id, "You", caption, _t(0), True,
                      media=media, doc_name=None if media == "photo" else name)
        self._history.setdefault(chat_id, []).append(msg)
        return msg

    async def edit_text(self, chat_id: int, msg_id: int, text: str) -> Message:
        for i, m in enumerate(self._history.get(chat_id, [])):
            if m.id == msg_id:
                edited = Message(m.id, m.chat_id, m.sender_name, text, m.timestamp,
                                 m.outgoing, m.media, m.doc_name, m.reply_to,
                                 m.reactions, edited=True)
                self._history[chat_id][i] = edited
                return edited
        raise ValueError(f"no message {msg_id} in chat {chat_id}")

    async def delete_message(self, chat_id: int, msg_id: int) -> None:
        self._history[chat_id] = [m for m in self._history.get(chat_id, []) if m.id != msg_id]

    async def mark_read(self, chat_id: int) -> None:
        self._unread[chat_id] = 0

    # ---- events ----

    def on_new_message(self, callback: Callable[[Message], None]) -> None:
        self._on_new_message = callback

    def on_message_changed(self, callback: Callable[[Message], None]) -> None:
        self._on_message_changed = callback

    def on_typing(self, callback: Callable[[int, str], None]) -> None:
        self._on_typing = callback

    async def disconnect(self) -> None:
        pass

    # ---- internals ----

    def _typing(self, chat_id: int) -> None:
        if self._on_typing is not None:
            self._on_typing(chat_id, _TITLES.get(chat_id, "Someone"))

    def _echo(self, chat_id: int) -> None:
        """Simulate an incoming reply so live-update paths can be exercised offline."""
        if self._on_new_message is None:
            return
        self._next_id += 1
        title = _TITLES.get(chat_id, "Someone")
        msg = Message(self._next_id, chat_id, title, "(mock reply) got it", _t(0), False)
        self._history.setdefault(chat_id, []).append(msg)
        self._on_new_message(msg)
