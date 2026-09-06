# README showcase

These are captures of Omarchygram 0.1.8 running its offline `--smoke` backend.
No Telegram login, personal conversations, desktop screenshots, or real
contact photos are used. The app runs only inside `bin/headless`, with private
configuration, cache, data, state, and Wayland runtime directories.

## Reproduce

On an Omarchy development machine with the project's build dependencies,
Weston, Vulkan, FFmpeg (including libx264 and drawtext), ImageMagick, Python 3,
and JetBrainsMono Nerd Font installed:

```sh
cargo build --offline --locked --release --bin omarchygram
bin/capture-showcase
```

The tool reads the installed Tokyo Night, Gruvbox, Catppuccin, and Catppuccin
Latte palettes from `/usr/share/omarchy/themes`. It never changes the desktop
theme. `--only stills`, `--only speed`, `--only animations`, and `--only themes`
recapture one set; `--only encode` rebuilds the videos from existing raw frames.
Stills use Vulkan. Clips use GTK's Cairo renderer to reduce snapshot/readback
overhead; `OMG_SHOWCASE_RENDERER=vulkan` overrides that capture choice. This
does not change the user's app settings or the separate Vulkan benchmarks.

Raw screenshots, recording frames, settings, logs, and timestamp manifests stay
under ignored `target/readme-showcase/`. Selected screenshots, GIFs, MP4s, and
compact recording manifests are written here. The source artwork is in
`artwork/`; `OMG_MOCK_IMAGE_DIR` maps numbered PNGs onto mock photo/avatar IDs.
This optional override is used only by the mock backend. The normal fixtures
and real Telegram media path are unchanged.

## What the clips demonstrate

- **speed:** first visits and returns to chats, polls, saved messages, and a
  bot. Chat-switch and message-arrival effects are disabled.
- **animations:** optional staggered chat transitions, animated typing dots,
  and typewriter/phosphor reveal on newly arriving dummy messages. Whole-window
  flicker and static-error effects are disabled.
- **themes:** the same running window follows four palette changes through
  its normal theme-file watcher. The conversation and contact panel stay open.

Chat and animation clips request 30 capture samples per second; the theme clip
requests 12. The lower theme sampling rate leaves time for GTK's style and
paint work between expensive full-window snapshots. PNG capture is additional
work and may skip samples during relayout. Encoding uses each source PNG's
filesystem modification timestamp to preserve elapsed time, including gaps;
the output is resampled to 25 FPS. Frames are not retimed to make navigation
look faster. Filesystem timestamps approximate capture completion rather than
display presentation time, so these clips are demonstrations, not latency or
FPS benchmarks. The adjacent JSON files record capture cadence and output size.

Screenshots contain actual GTK widgets and app-rendered media. The snapshot API
leaves window-background gutters transparent; encoding fills only those
transparent pixels with a conversation-background color sampled from that
same frame. Videos add an external caption strip identifying the offline demo
and 1× playback speed. No controls or timings are painted into the app view.
MP4 alternatives preserve full color; GIFs use a 256-color palette.

The numerical README claims come from the separate
[0.1.8 performance validation](../../specs/performance-audit-0.1.8.md),
committed in `2a4eeec599b9282ca80a9547f21244176bdd58e0`. Those benchmark
fixtures and binary predate the optional showcase-artwork hook. Recording
cadence is unrelated to the roughly 118 FPS scrolling measurement.

## Artwork provenance

The two raster assets were created with the built-in image-generation tool,
then copied into this repository. They are fictional demo content, not
photographs supplied by a user. The SVG group/avatar/sticker illustrations
were authored directly as vector assets. The interface itself was not
generated or redesigned for these captures.

`artwork/ridge.png` — exact generation prompt:

> Create one beautiful, photorealistic landscape photograph to be used as a sample photo shared in a fictional photographer's Telegram chat. Wide landscape composition, about 3:2 aspect ratio. A quiet alpine ridge at dawn with layered slate-blue mountains, a narrow warm sunlit trail leading into low cloud, delicate amber light grazing the rock, a small still lake in the lower third reflecting the pale peach sky. Refined natural color, exceptionally clear details, editorial outdoor photography, realistic atmospheric depth, soft film-like grain but no heavy filters. Beautiful at a small chat-photo size. No people, no text, no logos, no interface, no borders, no watermark. This is artwork inside the demo chat, not a screenshot or app mockup.

`artwork/marta.png` — exact generation prompt:

> Create a square editorial portrait photograph for a fictional adult user's avatar in a private offline messaging-app demo. An adult woman about 30 with a short dark chestnut bob, freckles, and a relaxed natural smile, wearing a simple slate-blue hiking jacket, softly blurred alpine background in warm early morning light. Face large and centered with generous space around the head so the image works as a small round profile photo. Warm, candid, understated outdoor photography, realistic natural skin texture, crisp eyes, beautifully balanced muted blue and amber colors. No text, logos, borders, interface, or watermark. The person is entirely fictional.

## Validation

See the [build, media, and full GUI gate results](validation.md).
