"""Credential and path handling. Orchestrator-owned: do not modify in delegations."""

import os
import tomllib
from dataclasses import dataclass
from pathlib import Path

CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", "~/.config")).expanduser() / "omarchygram"
DATA_DIR = Path(os.environ.get("XDG_DATA_HOME", "~/.local/share")).expanduser() / "omarchygram"
CONFIG_FILE = CONFIG_DIR / "config.toml"
SESSION_FILE = DATA_DIR / "omarchygram.session"

SETUP_HELP = f"""\
Omarchygram needs Telegram API credentials (one-time setup):

  1. Log in at https://my.telegram.org/apps with your Telegram account
  2. Create an application (any name, platform "Desktop")
  3. Save the credentials:

     mkdir -p {CONFIG_DIR}
     cat > {CONFIG_FILE} <<EOF
     api_id = <your api_id>
     api_hash = "<your api_hash>"
     EOF
     chmod 600 {CONFIG_FILE}
"""


@dataclass(frozen=True)
class Credentials:
    api_id: int
    api_hash: str


def load_credentials() -> Credentials | None:
    """Read api_id/api_hash from config.toml, or None if not configured yet."""
    try:
        with open(CONFIG_FILE, "rb") as f:
            data = tomllib.load(f)
        return Credentials(api_id=int(data["api_id"]), api_hash=str(data["api_hash"]))
    except (OSError, KeyError, ValueError, tomllib.TOMLDecodeError):
        return None


def ensure_data_dir() -> Path:
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    DATA_DIR.chmod(0o700)
    return DATA_DIR
