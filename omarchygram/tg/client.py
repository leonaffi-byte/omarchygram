"""Telethon wrapper. Orchestrator-owned: do not modify in delegations.

All coroutines must run on the GLib asyncio loop (gi.events); Telethon event
callbacks therefore already arrive on the GTK main thread — UI code may touch
widgets directly from any registered callback.

The UI talks only in the dataclasses below, never in Telethon types.
"""

import datetime
from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum, auto
from pathlib import Path

from telethon import TelegramClient, events, utils
from telethon.errors import (
    PhoneCodeInvalidError,
    PasswordHashInvalidError,
    SessionPasswordNeededError,
)
from telethon.tl import types

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
class Reaction:
    emoji: str
    count: int


@dataclass
class Message:
    id: int
    chat_id: int
    sender_name: str
    text: str
    timestamp: datetime.datetime
    outgoing: bool
    media: str | None = None  # "photo" | "sticker" | "voice" | "document" | None
    doc_name: str | None = None  # filename for documents
    reply_to: int | None = None  # id of the replied-to message in the same chat
    reactions: list[Reaction] = field(default_factory=list)
    edited: bool = False


class AuthError(Exception):
    """Wrong code/password; user-facing message in str(e). Retry is safe."""


class TgClient:
    """Auth state machine + the Telegram calls the UI needs."""

    def __init__(self) -> None:
        self._client: TelegramClient | None = None
        self._phone: str | None = None
        self._on_new_message: Callable[[Message], None] | None = None
        self._on_message_changed: Callable[[Message], None] | None = None
        self._on_typing: Callable[[int, str], None] | None = None
        # (chat_id, msg_id) -> raw Telethon message, for download/edit/delete.
        self._raw: dict[tuple[int, int], object] = {}
        self.state = AuthState.NEED_CREDENTIALS

    @property
    def is_mock(self) -> bool:
        return False

    # ---- auth ----

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

    # ---- reading ----

    async def get_dialogs(self, limit: int = 50) -> list[ChatSummary]:
        assert self._client is not None
        out: list[ChatSummary] = []
        async for d in self._client.iter_dialogs(limit=limit):
            last = d.message
            preview = (last.message or "") if last else ""
            if last and not preview:
                preview = self._media_placeholder(last)
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

    async def get_history(
        self, chat_id: int, limit: int = 50, before_id: int | None = None
    ) -> list[Message]:
        """Newest last (display order). before_id pages older messages."""
        assert self._client is not None
        kwargs = {"limit": limit}
        if before_id is not None:
            kwargs["max_id"] = before_id
        msgs = await self._client.get_messages(chat_id, **kwargs)
        out = [self._convert(m, chat_id) for m in reversed(msgs)]
        return [m for m in out if m is not None]

    async def download_media(self, chat_id: int, msg_id: int) -> Path | None:
        """Download a message's media to the cache; returns a GTK-renderable path.

        Stickers (webp) are converted to png. Returns the cached file on repeat
        calls without re-downloading. None if the message has no media.
        """
        assert self._client is not None
        raw = self._raw.get((chat_id, msg_id))
        if raw is None:
            raw = await self._client.get_messages(chat_id, ids=msg_id)
            if raw is None:
                return None
            self._raw[(chat_id, msg_id)] = raw
        if getattr(raw, "media", None) is None:
            return None
        config.MEDIA_DIR.mkdir(parents=True, exist_ok=True)
        prefix = config.MEDIA_DIR / f"{chat_id}_{msg_id}"
        existing = list(config.MEDIA_DIR.glob(f"{chat_id}_{msg_id}.*"))
        cached = [p for p in existing if p.suffix != ".webp"]
        if cached:
            return cached[0]
        path = await self._client.download_media(raw, file=str(prefix))
        if path is None:
            return None
        path = Path(path)
        if path.suffix == ".webp":
            from PIL import Image

            png = path.with_suffix(".png")
            Image.open(path).save(png)
            path.unlink()
            return png
        return path

    # ---- writing ----

    async def send_text(self, chat_id: int, text: str, reply_to: int | None = None) -> Message:
        assert self._client is not None
        sent = await self._client.send_message(chat_id, text, reply_to=reply_to)
        converted = self._convert(sent, chat_id)
        assert converted is not None
        return converted

    async def send_file(self, chat_id: int, path: str, caption: str = "") -> Message:
        assert self._client is not None
        sent = await self._client.send_file(chat_id, path, caption=caption or None)
        converted = self._convert(sent, chat_id)
        assert converted is not None
        return converted

    async def edit_text(self, chat_id: int, msg_id: int, text: str) -> Message:
        assert self._client is not None
        edited = await self._client.edit_message(chat_id, msg_id, text)
        converted = self._convert(edited, chat_id)
        assert converted is not None
        return converted

    async def delete_message(self, chat_id: int, msg_id: int) -> None:
        assert self._client is not None
        await self._client.delete_messages(chat_id, [msg_id])

    async def mark_read(self, chat_id: int) -> None:
        assert self._client is not None
        await self._client.send_read_acknowledge(chat_id)

    # ---- events (callbacks fire on the GTK main thread) ----

    def on_new_message(self, callback: Callable[[Message], None]) -> None:
        self._on_new_message = callback

    def on_message_changed(self, callback: Callable[[Message], None]) -> None:
        """Edits and reaction updates to already-displayed messages."""
        self._on_message_changed = callback

    def on_typing(self, callback: Callable[[int, str], None]) -> None:
        """callback(chat_id, display_name) — name may be empty. Fires per signal;
        the UI owns the 'X is typing' timeout (suggest 5s)."""
        self._on_typing = callback

    async def disconnect(self) -> None:
        if self._client is not None:
            await self._client.disconnect()

    # ---- internals ----

    def _install_handlers(self) -> None:
        assert self._client is not None

        @self._client.on(events.NewMessage(incoming=True))
        async def _new(event: events.NewMessage.Event) -> None:
            if self._on_new_message is None:
                return
            chat_id = utils.get_peer_id(event.message.peer_id)
            converted = self._convert(event.message, chat_id)
            if converted is not None:
                self._on_new_message(converted)

        @self._client.on(events.MessageEdited())
        async def _edited(event: events.MessageEdited.Event) -> None:
            if self._on_message_changed is None:
                return
            chat_id = utils.get_peer_id(event.message.peer_id)
            converted = self._convert(event.message, chat_id)
            if converted is not None:
                self._on_message_changed(converted)

        self._client.add_event_handler(
            self._typing_raw,
            events.Raw(
                types=[
                    types.UpdateUserTyping,
                    types.UpdateChatUserTyping,
                    types.UpdateChannelUserTyping,
                ]
            ),
        )

    async def _typing_raw(self, update) -> None:
        if self._on_typing is None:
            return
        if not isinstance(getattr(update, "action", None), types.SendMessageTypingAction):
            return
        try:
            if isinstance(update, types.UpdateUserTyping):
                chat_id = update.user_id
                user_id = update.user_id
            elif isinstance(update, types.UpdateChatUserTyping):
                chat_id = utils.get_peer_id(types.PeerChat(update.chat_id))
                user_id = utils.get_peer_id(update.from_id)
            else:  # UpdateChannelUserTyping
                chat_id = utils.get_peer_id(types.PeerChannel(update.channel_id))
                user_id = utils.get_peer_id(update.from_id)
            name = ""
            try:
                entity = await self._client.get_entity(user_id)
                name = utils.get_display_name(entity)
            except (ValueError, TypeError):
                pass
            self._on_typing(chat_id, name)
        except Exception:
            # Typing hints are cosmetic; never let a malformed update propagate.
            pass

    @staticmethod
    def _media_placeholder(m) -> str:
        if getattr(m, "photo", None) is not None:
            return "[photo]"
        if getattr(m, "sticker", None) is not None:
            return "[sticker]"
        if getattr(m, "voice", None) is not None:
            return "[voice message]"
        if getattr(m, "media", None) is not None:
            return "[file]"
        return ""

    def _convert(self, m, chat_id: int) -> Message | None:
        if m is None or not isinstance(m, types.Message):
            return None
        self._raw[(chat_id, m.id)] = m
        sender = getattr(m, "sender", None)
        name = utils.get_display_name(sender) if sender else ""

        media: str | None = None
        doc_name: str | None = None
        if m.photo is not None:
            media = "photo"
        elif m.sticker is not None:
            media = "sticker"
        elif m.voice is not None:
            media = "voice"
        elif m.document is not None:
            media = "document"
            doc_name = getattr(m.file, "name", None) or "file"

        reactions: list[Reaction] = []
        if m.reactions is not None:
            for r in m.reactions.results or []:
                emoji = getattr(r.reaction, "emoticon", None) or "★"
                reactions.append(Reaction(emoji=emoji, count=r.count))

        reply_to = None
        if m.reply_to is not None:
            reply_to = getattr(m.reply_to, "reply_to_msg_id", None)

        text = m.message or ""
        if not text and media is not None:
            text = ""  # UI renders the media widget; no placeholder text needed

        return Message(
            id=m.id,
            chat_id=chat_id,
            sender_name=name or ("You" if m.out else ""),
            text=text,
            timestamp=m.date.astimezone(),
            outgoing=bool(m.out),
            media=media,
            doc_name=doc_name,
            reply_to=reply_to,
            reactions=reactions,
            edited=m.edit_date is not None,
        )
