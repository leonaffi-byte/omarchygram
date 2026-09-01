"""Telethon wrapper. Orchestrator-owned: do not modify in delegations.

All coroutines must run on the GLib asyncio loop (gi.events); Telethon event
callbacks therefore already arrive on the GTK main thread — UI code may touch
widgets directly from the on_new_message callback.
"""

import datetime
from collections.abc import Callable
from dataclasses import dataclass
from enum import Enum, auto

from telethon import TelegramClient, events, utils
from telethon.errors import (
    PhoneCodeInvalidError,
    PasswordHashInvalidError,
    SessionPasswordNeededError,
)

from omarchygram.tg import config


class AuthState(Enum):
    NEED_CREDENTIALS = auto()  # config.toml missing/invalid
    NEED_PHONE = auto()
    NEED_CODE = auto()
    NEED_PASSWORD = auto()  # 2FA
    READY = auto()


@dataclass
class ChatSummary:
    id: int
    title: str
    last_message: str
    last_time: datetime.datetime | None
    unread_count: int


@dataclass
class Message:
    id: int
    chat_id: int
    sender_name: str
    text: str
    timestamp: datetime.datetime
    outgoing: bool


class AuthError(Exception):
    """Wrong code/password; user-facing message in str(e). Retry is safe."""


class TgClient:
    """Auth state machine + the few Telegram calls the UI needs."""

    def __init__(self) -> None:
        self._client: TelegramClient | None = None
        self._phone: str | None = None
        self._on_new_message: Callable[[Message], None] | None = None
        self.state = AuthState.NEED_CREDENTIALS

    @property
    def is_mock(self) -> bool:
        return False

    async def start(self) -> AuthState:
        creds = config.load_credentials()
        if creds is None:
            self.state = AuthState.NEED_CREDENTIALS
            return self.state
        config.ensure_data_dir()
        self._client = TelegramClient(str(config.SESSION_FILE), creds.api_id, creds.api_hash)
        await self._client.connect()
        config.SESSION_FILE.chmod(0o600)
        if await self._client.is_user_authorized():
            self._install_handlers()
            self.state = AuthState.READY
        else:
            self.state = AuthState.NEED_PHONE
        return self.state

    async def submit_phone(self, phone: str) -> AuthState:
        assert self._client is not None
        self._phone = phone.strip()
        await self._client.send_code_request(self._phone)
        self.state = AuthState.NEED_CODE
        return self.state

    async def submit_code(self, code: str) -> AuthState:
        assert self._client is not None and self._phone is not None
        try:
            await self._client.sign_in(self._phone, code.strip())
        except SessionPasswordNeededError:
            self.state = AuthState.NEED_PASSWORD
            return self.state
        except PhoneCodeInvalidError as e:
            raise AuthError("That code is not right — check Telegram and try again.") from e
        self._install_handlers()
        self.state = AuthState.READY
        return self.state

    async def submit_password(self, password: str) -> AuthState:
        assert self._client is not None
        try:
            await self._client.sign_in(password=password)
        except PasswordHashInvalidError as e:
            raise AuthError("Wrong password — try again.") from e
        self._install_handlers()
        self.state = AuthState.READY
        return self.state

    async def get_dialogs(self, limit: int = 50) -> list[ChatSummary]:
        assert self._client is not None
        out: list[ChatSummary] = []
        async for d in self._client.iter_dialogs(limit=limit):
            last = d.message
            preview = (last.message or "") if last else ""
            if last and not preview:
                preview = "[media]"
            out.append(
                ChatSummary(
                    id=d.id,
                    title=d.title or "Unknown",
                    last_message=preview,
                    last_time=last.date if last else None,
                    unread_count=d.unread_count or 0,
                )
            )
        return out

    async def get_history(self, chat_id: int, limit: int = 50) -> list[Message]:
        """Newest last (display order)."""
        assert self._client is not None
        msgs = await self._client.get_messages(chat_id, limit=limit)
        out = [self._convert(m, chat_id) for m in reversed(msgs)]
        return [m for m in out if m is not None]

    async def send_text(self, chat_id: int, text: str) -> Message:
        assert self._client is not None
        sent = await self._client.send_message(chat_id, text)
        converted = self._convert(sent, chat_id)
        assert converted is not None
        return converted

    def on_new_message(self, callback: Callable[[Message], None]) -> None:
        """Register the single UI callback for incoming messages (called on the GTK thread)."""
        self._on_new_message = callback

    async def disconnect(self) -> None:
        if self._client is not None:
            await self._client.disconnect()

    def _install_handlers(self) -> None:
        assert self._client is not None

        @self._client.on(events.NewMessage(incoming=True))
        async def _handler(event: events.NewMessage.Event) -> None:
            if self._on_new_message is None:
                return
            chat_id = utils.get_peer_id(event.message.peer_id)
            converted = self._convert(event.message, chat_id)
            if converted is not None:
                self._on_new_message(converted)

    def _convert(self, m, chat_id: int) -> Message | None:
        if m is None:
            return None
        sender = getattr(m, "sender", None)
        name = utils.get_display_name(sender) if sender else ""
        return Message(
            id=m.id,
            chat_id=chat_id,
            sender_name=name or ("You" if m.out else ""),
            text=m.message or "[media]",
            timestamp=m.date.astimezone(),
            outgoing=bool(m.out),
        )
