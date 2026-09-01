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
