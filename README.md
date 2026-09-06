# Omarchygram

**Telegram at native speed. Dressed in your Omarchy theme.**

A fast, lightweight Telegram client built with **Rust + GTK 4**. Quick chat
switching, bounded caches, and a **39.95 MB release executable** with voice
calls enabled. Your Omarchy colors carry through the entire app, live.

![Omarchygram in Tokyo Night: a dummy conversation with a mountain photo, replies, a sticker, and the contact details panel](docs/showcase/hero.png)

## Fast where you feel it

Measured with the optimized **0.1.8 source build**:

| Interaction / footprint | Measured result |
| --- | ---: |
| Paint the first screen of a 500-message history | **31 ms** median |
| Reopen a chat in the offline app | **33 ms** median |
| Local search across 1,000 chats | **28 ms** median |
| Message scrolling, selected effects enabled | **118 FPS** on a 120 Hz headless output |
| Largest phase-end RAM in the loaded scrolling run | **193.9 MB**; brief peak **243.2 MB** |
| Release executable, including calls | **39.95 MB** |

The 500-message history's first screen paints about **41× faster** than the
0.1.7 baseline; the executable is **30% smaller**. In a separate paired
comparison, message scrolling used **43% less CPU**, with roughly 2% lower FPS
and longer cold frame tails. Full controls are built near the viewport,
distant decoded images are released, and recent chat history stays cached.
Original media, message history, and calling support are retained.

These are **local, offline benchmarks**, not Telegram network latency or a
promise for every machine. Measured on a Core Ultra 9 285H with Intel graphics;
scrolling used a private 120 Hz output at 1.6× scale. The first row measures
history rendering after data is available; opening an uncached chat also
needs a Telegram response. Read the [results, methods, and tradeoffs](specs/performance-audit-0.1.8.md).

### Move between conversations

![Real-time dummy-account recording switching between conversations, polls, saved messages, and a bot, then revisiting recent chats](docs/showcase/speed.gif)

Real-time playback, including first visits and returns to recent chats.
[Full-color video](docs/showcase/speed.mp4).

### Your theme, throughout

![One running Omarchygram window changing live from Tokyo Night to Gruvbox, Catppuccin, and Catppuccin Latte](docs/showcase/themes.gif)

Change the Omarchy theme and the conversation, sidebar, controls, and profile
panel follow. No restart. [Full-color video](docs/showcase/themes.mp4).

| Catppuccin | Gruvbox | Catppuccin Latte |
| :---: | :---: | :---: |
| [![Catppuccin dark theme](docs/showcase/catppuccin.png)](docs/showcase/catppuccin.png) | [![Gruvbox warm dark theme](docs/showcase/gruvbox.png)](docs/showcase/gruvbox.png) | [![Catppuccin Latte light theme](docs/showcase/light.png)](docs/showcase/light.png) |

### A little motion. Your choice.

![Optional staggered chat transitions, typing dots, and typewriter reveal as new dummy messages arrive](docs/showcase/animations.gif)

54 individually selectable effects, from subtle transitions to terminal-style
message reveals. All are off by default. This demo enables a small selection;
whole-window flicker stays off. [Full-color video](docs/showcase/animations.mp4).

All media above comes from the real app's **offline dummy account**, captured
headlessly. The profile and landscape artwork are generated; the UI is the
actual GTK rendering. GIFs play at **1× elapsed speed**; capture overhead limits
their frame rate, so use the benchmark table for application FPS. [Reproduce the captures](docs/showcase/README.md).

## A full Telegram client

Keyboard-first, with inline photos, voice messages, video, stickers and animated
stickers, polls, forum topics, stories, voice calls, and background notifications.
Group messages show sender pictures; click through to a profile, a private
conversation, or an expanded profile photo. Small previews keep scrolling light,
while zooming and saving use the original files.

An AI assistant can summarize selected message ranges, including voice
transcripts, using a local or configured cloud provider. Anti-delete and edit
history, Omarchy actions from chat, and per-chat drafts are built in.

## Install (Omarchy / Arch)

The latest prebuilt package is **0.1.0**. For the **0.1.8 performance improvements
shown above**, [build the current source](#build-and-run).

```
sudo pacman -U https://github.com/leonaffi-byte/omarchygram/releases/download/v0.1.0/omarchygram-0.1.0-1-x86_64.pkg.tar.zst
```

It pulls in GTK 4, GStreamer (with the good and libav plugin sets), ffmpeg and
the JetBrainsMono Nerd Font, and adds an "Omarchygram" entry to the app menu.

To get updates with `pacman -Syu`, enable the project's package repository
instead (once):

```
sudo tee -a /etc/pacman.conf >/dev/null <<'EOF'

[omarchygram]
SigLevel = Optional
Server = https://github.com/leonaffi-byte/omarchygram/releases/latest/download
EOF
sudo pacman -Sy omarchygram
```

(`SigLevel = Optional` because the packages are not signed yet; they are built
from the tagged sources with `packaging/PKGBUILD`.) An AUR package
(`yay -S omarchygram`) follows once AUR account registration reopens.

## Requirements (building from source)

Arch Linux with:

```
sudo pacman -S --needed gtk4 rust
```

## Build and run

```
cargo build --release               # optimized build
cargo run --release                 # real Telegram (credentials below)
bin/headless target/release/omarchygram --smoke --probe  # offline GUI check
cargo test                          # run the tests
```

Use `--smoke` for offline dummy data without logging in. Automated captures and
GUI checks use `bin/headless`; see [the showcase instructions](docs/showcase/README.md).

## Telegram API credentials (one-time)

Real Telegram access needs your own API credentials. They are personal; never
commit them.

1. Log in at <https://my.telegram.org/apps> with your Telegram account.
2. Create an application (any name, platform "Desktop").
3. Start Omarchygram and enter the API ID and API hash in the form it shows
   (they are saved to a private file). Or write the file yourself:

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

## Background operation and online status

Closing the window keeps Omarchygram running: messages, synchronization and
notifications continue without occupying a workspace. Launch it again to
restore the same window and chat. Disable this in **Settings → Messaging →
Keep running when the window closes** if you prefer closing to quit.

Use **☰ → Quit Omarchygram**, **Ctrl+Q**, or `omarchygram --quit` to stop it.
`omarchygram --background` starts with the window hidden; login still opens a
window when needed. This does not enable automatic startup at login.

**☰ → About** shows the running version and a clickable GitHub link. After
updating, quit and reopen the app to load the new executable; closing and
restoring its window keeps the previous process running.

Omarchygram reports online while its window is visible, focused and used within
the last five minutes. Hiding it, moving focus elsewhere or becoming idle
requests offline status. Ghost mode always requests offline. Telegram privacy
settings, network delays and your other Telegram clients can affect what
contacts see; a background connection alone does not request online status.

Recently opened chats display their cached messages immediately, then refresh
from Telegram. The account-specific cache keeps up to 24 recent pages of 50
messages each, at most 12 MiB of message payload. Cache files are private and
persist across restarts. A chat without cached history still needs its first
Telegram response. Failed refreshes keep the cached content and offer Retry.

Off-screen chat badges stop animating, and the vignette reuses its gradient
pixels. During scrolling and typing, the decorative vignette pulse and flicker
pause until input has been quiet for 350 ms. Your selected effects then resume.
Incoming update batches yield to input and drawing during synchronization.

## Profiles and media

Click a sender's name in a group to view their profile without leaving the
conversation. **Message** opens a private chat; **Call** starts a voice call
when calling support is enabled. Click a profile photo in these details or in
Chat info to view the larger photo. Escape returns to the details.

Photos and voice messages can recover their download details even when the
chat first opens from its local cache. Download and playback errors offer
**Retry**, which can replace a damaged cached file. Desktop message
notifications include the private chat's user photo or the group's photo;
notifications still arrive if a photo is unavailable or slow to download.

## Layout

Telegram Desktop's layout on the Omarchy skin: a resizable chat list (drag
the divider; `Ctrl+Shift+B` collapses it to an avatar strip; it collapses by
itself under 640px), a search box (chats, then messages) and the ☰ main menu
above it, folder tabs when your account has folders, an "Archived chats" row,
avatars (profile photos or initials in theme colors), a chat header with the
contact's status and a ⋮ menu (mute, pin, mark unread, jump to date, clear
history, delete chat), day separators, sender names in groups, inline
timestamps with ✓/✓✓ read ticks, and per-chat drafts that follow you between
chats and devices. Right-click a chat for the same actions.

## Messages

Search inside a chat (the magnifier in the header, `Ctrl+Shift+F`) with
"N of M" navigation; bold/italic/strike/code/links/spoilers render and can be
typed with the usual shortcuts or `**markers**`; link previews; reactions
(right-click a message for the quick row, click a pill to toggle); pinned
message bar with a four-line preview and Expand/Collapse; forward to one or several chats; full-window photo viewer with
arrow keys and Save/Open; video/GIF/audio cards that download and open in
your default app.

## More Telegram functions

The **i** button (or `Ctrl+Shift+I`) opens the chat info panel: photo, bio,
username, phone, notifications switch, members of a group, and the shared
photos/files/links/voice tabs. The sticker button next to the emoji one opens
your sticker packs and saved GIFs. The microphone records a voice note
(needs `ffmpeg`; Esc cancels). ☰ → Contacts opens a chat with a contact;
☰ → New group creates one. Right-click a message → Select for multi-select
(forward, delete, copy several at once). Attaching a file asks for a caption.
Muted chats never raise desktop notifications.

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
| `Ctrl+F`      | Search chats and messages       |
| `Ctrl+Shift+B`| Collapse the chat list          |
| `Ctrl+B/I/U`  | Bold / italic / underline       |
| `Ctrl+Shift+X/M/K/P` | Strike / monospace / link / spoiler |

Every shortcut can be changed in Settings → Keyboard.

## Settings

`Ctrl+,` (or ☰ → Settings) opens the settings pages: Account, Appearance,
Timestamps, Privacy, AI, Omarchy, Keyboard, Animations. Every option writes
straight to `~/.config/omarchygram/settings.toml` (also hot-reloaded if you
edit the file by hand). Everything below is off until you turn it on.

- **Account** — who you are, "Change…" for the Telegram API credentials
  (takes effect after a restart), Log out.
- **Appearance** — avatars, compact chat list, send on Enter, markdown
  formatting on send.
- **Keyboard** — every shortcut, rebindable; conflicts are flagged.

- **Timestamps** — seconds in message times, a live clock in the chat header,
  a custom time format.
- **Privacy** — *Ghost mode* (no read receipts, you stay "offline" while
  reading), *Keep deleted messages* (messages others delete stay, struck
  through), *Keep edit history* (right-click an edited message → Edit history).
  Messages are recorded in a local archive at
  `~/.local/share/omarchygram/accounts/<account-id>/archive.sqlite` (private to your user and separated by Telegram account). Older unscoped history can be imported from Settings → Privacy after confirming which account it belongs to; the original file is preserved.
- **AI** — enable the Assistant chat and the AI actions; auto-transcribe new incoming voice
  messages; pin a provider/model if you don't want auto-detection.
- **Omarchy actions** — enable the Omarchy chat; allow shell commands (each one
  is confirmed in a dialog before it runs).

Right-click any message for Copy, Reply, Edit, Delete, Copy message id / user
id, and — with AI on — Draft reply, Translate, Summarize. Voice messages get a
*transcribe* button. "Jump…" in the header jumps to a date; ▼ returns to the
latest messages.

**Auto-transcribe voice** applies only to new incoming voice messages received
while the app is running and the setting is enabled, including in the
background. Enabling it, reopening a chat, loading history, or receiving an edit
does not transcribe older messages. One automatic transcription runs at a time;
manual transcription remains available. Turning auto off cancels its queued and
active work. Groq uploads recognize the audio format even for older `.bin`
cache files, without converting the recording.

## Assistant and Omarchy chats

With AI or Omarchy actions enabled, two extra chats appear at the top of the
list. They are **local**: nothing you type there goes to Telegram.

**Assistant** — talk to a language model about your own conversations.
`/help` lists the commands: `/status` (which providers are available),
`/catchup [chat]` (what did I miss), `/translate <lang> <text>`,
`/summarize <text>`, `/search <question>`, or just type.

Click **Start** to open the assistant's introduction and composer. To summarize
a range in a real chat, right-click the first message → **Summarize from here**,
then the last → **Summarize to here**. Both endpoints and every message between
them are included, even across history pages. The private summary panel has
Copy, Cancel/Close, and Retry controls; it never posts the result to Telegram.
Voice notes in the selected range are transcribed one at a time, reusing cached
transcripts. Failed transcripts stop the summary with an error instead of
silently omitting speech. Long ranges are summarized in parts, up to 2,000
messages and one million characters. Closing the panel, switching chats,
clearing/deleting the chat, disabling AI, or logging out cancels the request.
Automatic transcription continues to apply only to new incoming voice notes.

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
- **API keys:** Settings → AI has one masked field per provider (Anthropic,
  OpenAI, Groq, Gemini) plus the whisper model path; keys are stored in
  `~/.config/omarchygram/config.toml` (private). Editing that file directly
  also works:

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

## Animations

Settings → **Animations** lists 54 optional effects in the terminal/phosphor
style — typewriter and decode message reveals, pager-wipe chat switches,
braille typing spinners, badge pops and rolls, CRT power-on, boot log,
scanlines, vignette, matrix rain in the empty state, cursor comet, theme
morph, and more. Every effect is an individual toggle with a Preview button;
three presets (Purist = all off, Subtle, Full phosphor) set a whole mood at
once. Screen flicker briefly dims the whole window every six seconds; Error
static flashes noise across it on errors. Both require an individual opt-in
and are excluded from every preset. Mutually exclusive
styles (message entry, send feedback, chat switch)
are one-of groups. All effects respect GTK's reduced-motion setting
(`gtk-enable-animations`). Everything is off by default.

Chat and message lists build full controls around the viewport while keeping
the complete history and accessible text available. Offscreen text controls
and photo textures can be released; selection, revealed spoilers, translations,
and keyboard focus survive returning to a message. Two recent jump destinations
stay ready for back-and-forth navigation. Photo originals remain on disk, with
an 8 MiB decoded-preview cache and nearby images restored ahead of scrolling.
Scanlines reuse a one-pixel-wide texture at the display's pixel grid, including
fractional scaling. Earlier rendering measurements are in
[the frame performance report](specs/100-fps-performance.md).

The offline benchmark suite measures chat opening, search, scrolling, resource
usage, cache/database operations and media decoding. It uses mock data and a
private headless compositor:

```sh
cargo build --release --bin omarchygram --example performance_audit --example frame_perf_probe
bin/run-performance-audit --output target/performance-audit-new
```

Run benchmarks without a simultaneous build or UI gate. Raw results and a run
manifest are written to the selected directory. The original baseline is in
[the 0.1.7 audit](specs/performance-audit-0.1.7.md), with the measured changes in
[the 0.1.8 validation](specs/performance-audit-0.1.8.md). Requirements and acceptance
results are in [the responsiveness report](specs/responsiveness-targets.md).

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

For a system-wide install there is a from-source Arch package recipe in
`packaging/PKGBUILD` (its header lists what to fill in before publishing to
the AUR).

## Bar widget for Omarchy

A separate Omarchy 4 shell plugin, `omarchygram-bar` (id `leoom.omarchygram`),
puts an icon on the bar with the unread count, shows the current voice call,
and focuses or launches the app on click. Install it like any Omarchy plugin:

```
omarchy plugin add https://github.com/leonaffi-byte/omarchygram-bar.git --enable
```

Settings go through the bar, e.g. `omarchy bar set leoom.omarchygram showCount false`
(`icon`, `showCount`, `countMuted`, `hideWhenIdle`, `appId`, `launchCommand`,
`statusPath`). The app feeds it through
`~/.local/state/omarchygram/status.json` (unread totals, call state, a 30 s
heartbeat; private to your user, never message text). If the badge never
appears, the app and the shell disagree on `XDG_STATE_HOME`: set `statusPath`.

## Voice calls (optional)

Voice calls (Wave 7) use [ntgcalls](https://github.com/pytgcalls/ntgcalls), a
separate library that handles the WebRTC transport, encryption and audio. It is
built behind the `calls` cargo feature (on by default). The prebuilt library is
not committed to this repository; fetch it once before building:

```
bin/fetch-ntgcalls
```

The Linux build uses the system GLib and FFmpeg libraries, avoiding duplicate
copies in the executable. The pinned engine requires GLib 2.88+ and FFmpeg ABI
versions avformat/avcodec 63, avutil 61, and swresample 7. The build checks these
versions; another FFmpeg ABI requires rebuilding the engine against matching
headers. Release executables on x86-64 Linux also require glibc 2.36+ for compact
ELF relocations. See [the build integration notes](vendor/ntgcalls-sys/README.omarchy.md).

To build without call support (smaller binary, no ntgcalls):

```
cargo build --no-default-features
```

### NOTICE

ntgcalls is licensed under the GNU Lesser General Public License v3.0
(LGPL-3.0-only), and it bundles libwebrtc, BoringSSL and Opus under their own
permissive licenses. Omarchygram links ntgcalls statically. Because this
project's source is published here, anyone receiving an Omarchygram binary can
obtain its source and rebuild it against a modified ntgcalls, satisfying the
LGPL relinking requirement. The ntgcalls source for the exact version used is at
its release page (the version pinned in `bin/fetch-ntgcalls`); its license text
and copyright notices are preserved there. To build against your own ntgcalls,
point `NTGCALLS_LIB_DIR` at your `libntgcalls.a`, or set `NTGCALLS_DYLIB=1` to
link it dynamically.

### Review improvements (September 2026)

Settings now supports search, a Messaging category, text sizes from 85–150%,
and compact/comfortable chat-list previews. Theme colors maintain readable
secondary text across dark and light palettes; narrow windows use a compact
chat and story rail.

The location dialog keeps coordinate entry and adds an explicit place search
(Photon by default; configurable in Settings → Privacy) and optional system
location through GeoClue. Nothing is searched automatically. Check or select
the pin before sharing. Active calls offer microphone/speaker selection under
Audio devices, with mute preserved during changes.
