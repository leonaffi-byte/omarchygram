# Omarchygram

A Telegram client for Omarchy that takes its colors from the active Omarchy theme.

## Requirements

Arch Linux with:

```
sudo pacman -S --needed gtk4 rust
```

## Build and run

```
cargo build            # build
cargo run              # run against real Telegram (needs credentials, see below)
cargo run -- --smoke   # run offline against mock data, no login needed
cargo test             # run the tests
```

Use `--smoke` to look at the app without setting anything up.

## Telegram API credentials (one-time)

Real Telegram access needs your own API credentials. They are personal; never
commit them.

1. Log in at <https://my.telegram.org/apps> with your Telegram account.
2. Create an application (any name, platform "Desktop").
3. Save the credentials it gives you:

   ```
   mkdir -p ~/.config/omarchygram
   (umask 077; cat > ~/.config/omarchygram/config.toml <<EOF
   api_id = <your api_id>
   api_hash = "<your api_hash>"
   EOF
   )
   ```

   (The `umask` makes the file private from the moment it is created.)

On the first `cargo run` the app asks for your phone number and the login code
Telegram sends you (and your password if you use two-step verification). The
session is stored at `~/.local/share/omarchygram/omarchygram.session`, so this
happens only once.

## Keyboard

| Key           | Action                          |
| ------------- | ------------------------------- |
| `Ctrl+K`      | Open the chat switcher          |
| `Alt+Up`      | Previous chat                   |
| `Alt+Down`    | Next chat                       |
| `Enter`       | Send the message                |
| `Shift+Enter` | New line in the message         |
| `Esc`         | Cancel, or focus the composer   |
| `Ctrl+,`      | Open settings                   |

## Settings

`Ctrl+,` opens the settings panel. Every option writes straight to
`~/.config/omarchygram/settings.toml` (also hot-reloaded if you edit the file
by hand). Everything below is off until you turn it on.

- **Timestamps** — seconds in message times, a live clock in the chat header,
  a custom time format.
- **Privacy** — *Ghost mode* (no read receipts, you stay "offline" while
  reading), *Keep deleted messages* (messages others delete stay, struck
  through), *Keep edit history* (right-click an edited message → Edit history).
  Messages are recorded in a local archive at
  `~/.local/share/omarchygram/archive.sqlite` (private to your user).
- **AI** — enable the Assistant chat and the AI actions; auto-transcribe voice
  messages; pin a provider/model if you don't want auto-detection.
- **Omarchy actions** — enable the Omarchy chat; allow shell commands (each one
  is confirmed in a dialog before it runs).

Right-click any message for Copy, Reply, Edit, Delete, Copy message id / user
id, and — with AI on — Draft reply, Translate, Summarize. Voice messages get a
*transcribe* button. "Jump…" in the header jumps to a date; ▼ returns to the
latest messages.

## Assistant and Omarchy chats

With AI or Omarchy actions enabled, two extra chats appear at the top of the
list. They are **local**: nothing you type there goes to Telegram.

**Assistant** — talk to a language model about your own conversations.
`/help` lists the commands: `/status` (which providers are available),
`/catchup [chat]` (what did I miss), `/translate <lang> <text>`,
`/summarize <text>`, `/search <question>`, or just type.

**Omarchy** — run desktop actions by name: `help`, `list`, `screenshot`,
`lock`, `notify <text>`, `volume raise`, `theme <name>`, `themes`,
`terminal`, `status`, plus every Omarchy tool installed on the machine
(`list omarchy-`). Add your own under `[os.actions]` in `settings.toml`
(`name = "shell command"`). `run <command>` runs arbitrary shell — only if
"Allow shell commands" is on, and only after you confirm the exact command in a
dialog. Everything that runs is logged to
`~/.local/state/omarchygram/os-audit.log`.

## AI providers

Providers are auto-detected; the first available one is used unless you pin
one in settings.

- **Local:** `ollama` (if it is running on `127.0.0.1:11434`) for chat;
  `whisper.cpp` (`whisper-cli` on PATH + a `ggml-*.bin` model in
  `~/.local/share/whisper.cpp/`, plus `ffmpeg`) for transcription.
- **API keys:** add a table to `~/.config/omarchygram/config.toml`
  (already private) — any of these:

  ```
  [ai]
  anthropic_api_key = "..."
  openai_api_key = "..."
  groq_api_key = "..."
  gemini_api_key = "..."
  whisper_model = "~/.local/share/whisper.cpp/ggml-base.bin"   # optional override
  ```

  Environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`,
  `GEMINI_API_KEY`) work as a fallback. Groq and OpenAI also provide
  transcription. Keys never appear in logs or error messages.

## Theming

Colors come from `~/.local/state/omarchy/current/theme/colors.toml`, the file
that holds the currently active Omarchy theme. The app watches
`~/.local/state/omarchy/current/theme.name` — the file Omarchy rewrites when
you switch theme — and re-reads `colors.toml` whenever it changes, so switching
your Omarchy theme retints the running window, no restart needed. If Omarchy is
not installed, a neutral dark palette is used instead.

## Desktop launcher

To get an application-menu entry for this checkout:

```
bin/install-desktop
```

It builds the release binary if needed and writes
`~/.local/share/applications/omarchygram.desktop` pointing at it. Run it again
after moving the checkout.
