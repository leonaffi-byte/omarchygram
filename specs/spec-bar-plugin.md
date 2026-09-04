# Omarchy bar plugin for Omarchygram — spec (rev 2)

Decided 2026-09-04 (user: "the bar thing should be a plugin", "it can be a
different package"). Rev 2 folds in the codex pre-review (24 findings) and two
facts verified on this machine: Hyprland 0.56 parses `hyprctl dispatch`
arguments as Lua (a shell-string focus command is not portable), and the
Omarchy shell focuses windows through Quickshell's toplevel API instead.

Two deliverables:

- **Part A (app, orchestrator-owned, done):** Omarchygram writes a small status
  file whenever its unread totals or voice-call state change, plus a heartbeat.
- **Part B (plugin, separate package `~/Projects/omarchygram-bar`):** an
  Omarchy 4 shell plugin of kind `bar-widget` (Quickshell 0.3.1 / QML) that
  watches that file and shows an icon, an unread badge and the in-call state.
  Left click activates the app's window through the Wayland toplevel API, or
  launches the app when no window exists.

The plugin follows the first-party widget contract exactly; it never invents
UI idioms, colors or fonts.

## 1. Status file contract (shared by A and B) — version 1

Path: `$XDG_STATE_HOME/omarchygram/status.json` when `XDG_STATE_HOME` is set
AND absolute, else `$HOME/.local/state/omarchygram/status.json`. (Rust's
`dirs::state_dir()` applies exactly this rule; the plugin must too.) Directory
0700, file 0600, written atomically (per-process temp file + rename). No
fsync: the file is volatile state; rename already guarantees a reader never
sees a torn document.

```json
{
  "version": 1,
  "running": true,
  "heartbeat_secs": 30,
  "updated_at": "2026-09-04T06:12:33+02:00",
  "unread": 3,
  "unread_chats": 2,
  "unread_with_muted": 7,
  "unread_chats_with_muted": 4,
  "call": null
}
```

- `running`: `true` from the moment the UI is up, `false` on every normal exit
  (GTK `Application::shutdown`). **Liveness is `running && fresh`**, where
  fresh = `now - updated_at <= 3 × heartbeat_secs` (90 s). The app rewrites
  the file every `heartbeat_secs` while running, so a crash goes stale and a
  consumer must then behave as not running with `call: null`.
- Unread totals. **Archived chats never count** (Telegram's tray convention).
  `unread` / `unread_chats`: messages / chats in non-muted chats.
  `unread_with_muted` / `unread_chats_with_muted`: the same populations plus
  muted chats. A chat counts `max(unread, unread_mark ? 1 : 0)`. This
  intentionally differs from the sidebar's "All" folder badge, which counts
  everything. Local virtual chats (Assistant/Omarchy) always contribute 0.
- `call`: `null` when there is no call, including `Ended`. Otherwise

```json
{ "phase": "requesting|incoming|exchanging|connecting|active",
  "peer": "Marta", "outgoing": false, "muted": false,
  "connected_at": "2026-09-04T06:10:01+02:00" }
```

  Identity: the app mirrors what its own call view accepted (generation-keyed;
  a late `Ended` for an older call never clears a newer one; a colliding
  incoming call while active is rejected upstream and leaves the file
  unchanged). `connected_at` is `null` before `active`, set on the first
  `active` update and preserved across later updates of the same call.
- `peer` is display text: already capped at 128 chars with control, bidi and
  format characters stripped by the app. Consumers still render it as text
  only — never into a command, path or URL — and show "Unknown caller" when
  it is empty.
- On logout **success**: totals 0, `call: null`. A failed logout keeps the
  signed-in snapshot.
- Consumers ignore unknown fields; `version` bumps only on a breaking change.

## 2. Part A — app status hook (orchestrator; done, `src/status.rs`)

- Thread-local writer on the UI thread. Unread totals come from a provider
  the chat list registers (`ChatList::totals_of` over its summaries map);
  every mutation (`set_chats`, `upsert`, `set_summary`, `remove_chat`,
  `clear_unread`, `set_unread_mark`) calls `status::unread_changed()`, which
  arms ONE one-shot 150 ms timer; the timer pulls the totals, serializes and
  writes. Duplicate documents (timestamp aside) are not rewritten.
- Calls: after `CallView::update`, `status::set_call(view.current())`.
- `set_running(true)` in the GTK activate handler (only the primary instance
  runs it, so a second launch cannot clobber the live file);
  `Application::shutdown` → `status::shutdown()` writes `running: false`,
  `call: null` and freezes the writer. Logout success → `reset_session()`.
- Heartbeat: repeating 30 s timer forcing a rewrite while running.
- Write failures keep the change pending (retried on the next flush) and log
  once. Temp file is `status.json.<pid>.tmp`, unlinked then `create_new` +
  0600 (never follows a planted symlink). Missing HOME/XDG panics (project
  policy), never a relative path.
- `OMG_STATUS_PATH` overrides the path; `--smoke` sets a throwaway file.
- Probe (gate): "status file written" compares the file with the chat list's
  own `unread_totals()` exactly and requires a non-zero fixture and ≥ 1 write;
  "status file reports active call" requires phase/peer/outgoing/muted/
  `connected_at` and a new write; "status file clears call" requires `null`.
  Unit tests cover document shape, dedupe, forced heartbeat write, 0600,
  temp cleanup, shutdown freeze, Ended/None clearing, name sanitizing.
- Security pre-scan (opus) done: low risk; its three warnings are fixed above.

## 3. Part B — the plugin (codex; repo `~/Projects/omarchygram-bar`)

### 3.1 Files (exactly these; no symlinks, no binaries, do not commit)

```
manifest.json
BarWidget.qml
Model.js
tests/run.js
tests/fixtures/*.json         (one per state in §3.4)
README.md
LICENSE                        (MIT)
MARKETPLACE_SUBMISSION.md      (draft of the Omarchy marketplace issue form)
.gitignore
```

`CLAUDE.md`, `AGENTS.md`, `SPEC.md` already exist — do not modify them.

### 3.2 manifest.json

Validated by `omarchy plugin validate` (schema in
`/usr/share/omarchy/bin/omarchy-plugin-validate`): `schemaVersion: 1` (JSON
number), `id`, `name`, `version`, `kinds`, `entryPoints` required.

- `id` `leoom.omarchygram` (`omarchy.*` is reserved); `name` `Omarchygram`;
  `version` `0.1.0`; `author` `leoom`; `license` `MIT`;
  `description` "Unread badge and call state for the Omarchygram Telegram client."
- `kinds` `["bar-widget"]`; `entryPoints` `{ "barWidget": "BarWidget.qml" }`.
- `barWidget`: `displayName` "Omarchygram", `description`, `category`
  "Communication", `aliases` `["telegram", "omarchygram", "tg"]`,
  `allowMultiple: false`, `defaultSection: "right"`, plus `defaults` and
  `schema` for the settings below. Copy the entry shape (`key`, `type`,
  `label`, `description`, `defaultValue`) from
  `~/.config/omarchy/plugins/akitaonrails.ai-usagebar/manifest.json`.
  **`defaults` and `schema` must agree** (same keys, same types, same values)
  — acceptance checks it with `jq`.

Settings (read with the base `setting(name, fallback)`):

| key | type | default | meaning |
|---|---|---|---|
| `icon` | string | `""` (U+F1D8, paper plane) | bar glyph; must exist in the bar font |
| `showCount` | boolean | `true` | show the number; else a dot when > 0 |
| `countMuted` | boolean | `false` | badge uses the `*_with_muted` fields |
| `hideWhenIdle` | boolean | `false` | hide when live, 0 badge and no call (§3.4) |
| `appId` | string | `"dev.leoom.Omarchygram"` | Wayland app id of the app window |
| `launchCommand` | string | `"omarchygram"` | shell command line run when no window exists |
| `statusPath` | string | `""` | override of the status file path (empty = §1 rule) |

### 3.3 BarWidget.qml — behaviour

Extends `BarWidget` (`qs.Ui`), `moduleName: "leoom.omarchygram"`. Imports:
`QtQuick`, `Quickshell`, `Quickshell.Io`, `Quickshell.Wayland`, `qs.Commons`,
`qs.Ui`.

- **Path:** `statusPath` if non-empty; else `Quickshell.env("XDG_STATE_HOME")`
  when it starts with `/`; else `Quickshell.env("HOME") + "/.local/state"`;
  then `+ "/omarchygram/status.json"`. Empty HOME → treat as absent file.
- **Data — three `FileView`s** (pattern: `Commons/Color.qml` `userShellFile`
  and `plugins/bar/Bar.qml` lines 873–948, which watch state directories):
  1. `statusFile`: `path` above, `watchChanges: true`, `printErrors: false`,
     `onLoaded: root.apply(text())`, `onFileChanged: reload()`,
     `onLoadFailed: root.apply("")`.
  2. `stateDirWatch` on `…/omarchygram` (the file's directory) and
  3. `stateParentWatch` on its parent (`…/.local/state` or `$XDG_STATE_HOME`),
     both `watchChanges: true`, `printErrors: false`,
     `onFileChanged: statusFile.reload()`. This catches the file's first
     creation, the directory's first creation, and every atomic replacement
     (a rename swaps the inode under a bare file watch).
- **Model:** all parsing and state derivation live in `Model.js`; QML only
  binds to its result. `property double nowMs` is set on every apply and by
  the timer; the view binding depends on it.
- **Timer:** exactly one `Timer { repeat: true }`. `running` is bound to
  `model.live || model.callActive` and `interval` to
  `model.callActive ? 1000 : 60000`. `onTriggered: root.nowMs = Date.now()`.
  So it ticks once a second only during an active call, once a minute while
  the app is alive (to expire a stale file after a crash), and not at all
  otherwise.
- **Rendering:** one `Row` is the sole content child; root
  `implicitWidth/implicitHeight` bind to it. Inside, an unanchored
  `BarIconButton` (`bar: root.bar`, `slotSize: Style.bar.statusSlot`,
  `fontSize: Style.font.caption`, `text: model.glyph`,
  `tooltipText: model.tooltip`, `onPressed: root.activateOrLaunch()`) and a
  badge `Text` (`font.pixelSize: Style.font.caption`, visible when
  `model.badge !== ""`, color from the `Color` singleton — read
  `Commons/Color.qml` and use its foreground token; the in-call glyph uses
  its accent token). No literal colors, shadows, animation or custom fonts.
  Vertical bars (`root.vertical`): icon only.
- **Click → `activateOrLaunch()`:** iterate `ToplevelManager.toplevels`
  (`Quickshell.Wayland`, as `plugins/bar/widgets/ActiveWindow.qml` does) and
  call `.activate()` on the first toplevel whose `appId === setting("appId")`.
  If none matches, `root.bar.run(setting("launchCommand"))` — the setting is
  a shell command line (arguments allowed, it is the user's own config, same
  trust as first-party widgets' `bar.run` strings). This is the ONLY
  `bar.run` call site. `peer` never enters any command.
- **IPC:** `IpcHandler { target: "leoom.omarchygram" }` with
  `function refresh(): void { root.broadcast("reload") }`; `reload()` calls
  `statusFile.reload()`. Mirrors `SystemUpdate.qml`.

### 3.4 Model.js — pure, strict, testable

```
parse(text)                    → normalized status | null
view(status, settings, nowMs)  → { live, visible, glyph, badge, tooltip, callActive }
formatElapsed(connectedAtIso, nowMs) → "MM:SS" ("H:MM:SS" past an hour) | ""
```

`parse` returns `null` for invalid JSON, `version !== 1`, or a missing/
non-boolean `running`. Counts: finite, non-negative integers, else 0.
`heartbeat_secs`: positive number, else 30. `updated_at`: parseable, else
treated as stale. `call`: an object with a phase in the allowed set, string
`peer` (empty → "Unknown caller"), boolean `outgoing`/`muted`, `connected_at`
parseable or null — anything else degrades to `call: null`.

States (`view`):
- absent / invalid / `running:false` / stale (`nowMs - updated_at > 3 ×
  heartbeat_secs × 1000`) → `live:false`, icon only, tooltip "Omarchygram is
  not running", always visible (it is the launch affordance).
- live, badge 0, no call → icon; hidden only if `hideWhenIdle`.
- live with unread → badge = `unread` (or `unread_with_muted` when
  `countMuted`), rendered as the number or "•" when `!showCount`; tooltip
  "N unread in M chats" (M from the matching `*_chats*` field).
- `incoming` → phone glyph `""` (U+F095), "Incoming call from PEER";
  `requesting`/`exchanging`/`connecting` → "Calling PEER…" when outgoing else
  "Connecting to PEER…"; `active` → "In call with PEER · MM:SS" (elapsed
  from `connected_at`, clamped at 0 for future timestamps; omitted when
  `connected_at` is missing, and then `callActive:false` so the 1 s tick does
  not run) plus " · muted" when muted. A stale file shows no call.

`tests/run.js` (plain Node, no dependencies) loads every fixture, calls
`parse` + `view` with fixed `nowMs` and settings, and asserts exact
`live/visible/glyph/badge/tooltip/callActive`. Fixtures: absent (empty
text), invalid-json, wrong-version, not-running, stale (old `updated_at`),
idle, idle+hideWhenIdle, unread, unread-hidden-count, unread-count-muted,
malformed-counts, incoming, outgoing, active, active-muted,
active-no-connected-at, active-future-connected-at, unknown-phase (→ no
call), empty-peer. Plus `formatElapsed` edge cases (0 s, 59 s, 3599 s,
3600 s, null, garbage). Exit 0 on success, non-zero with the failing case.

### 3.5 README.md (user-facing)

Install: `omarchy plugin add https://github.com/leoom/omarchygram-bar.git --enable`
(placement prompt; or `omarchy bar put leoom.omarchygram --after omarchy.clock`).
Requirements: Omarchy 4 shell; Omarchygram with the status file (link to the
app). Settings with `omarchy bar set leoom.omarchygram <key> <value>` and the
table above. Troubleshooting: the app id is in `hyprctl clients -j`
(`initialClass`); a badge that never appears usually means a different
`XDG_STATE_HOME` between the app and the shell → set `statusPath`;
`omarchy plugin validate .` for authors. Short and plain; no marketing copy.

### 3.6 Acceptance (machine-checkable — the verifier runs these)

1. `omarchy plugin validate ~/Projects/omarchygram-bar` → exit 0.
2. `node tests/run.js` → exit 0.
3. `jq -e '.id=="leoom.omarchygram" and .kinds==["bar-widget"] and .entryPoints.barWidget=="BarWidget.qml" and .barWidget.defaultSection=="right" and ((.barWidget.defaults|keys|sort) == ([.barWidget.schema[].key]|sort)) and all(.barWidget.schema[]; .defaultValue == $d[.key])' --argjson d "$(jq .barWidget.defaults manifest.json)" manifest.json`
   (defaults and schema agree, key by key).
4. `qmllint BarWidget.qml` reports no `Error`/syntax line (unknown `qs.*`
   module warnings are acceptable).
5. `grep -c "bar.run(" BarWidget.qml` = 1; `grep -c "Process {" BarWidget.qml`
   = 0; `grep -c "Timer {" BarWidget.qml` = 1; `grep -c "FileView {"` = 3;
   no `#[0-9a-fA-F]{3,8}` color literals in any `.qml`; `find . -type l` empty;
   `grep -c "ToplevelManager" BarWidget.qml` ≥ 1.
6. `git status --porcelain` lists only the files in §3.1.
7. Live (orchestrator, mandatory): installed as a git checkout under
   `~/.config/omarchy/plugins/leoom.omarchygram`, enabled;
   `journalctl --user _COMM=quickshell` shows no error or warning mentioning
   `leoom.omarchygram`, `PluginRegistry` or a QML error since enabling; the
   widget reacts within 1 s to a hand-written status fixture (unread → badge,
   active call → phone glyph + elapsed, stale → not running); a bar screenshot
   goes to the `ui-reviewer`.

### 3.7 Worker rules (codex)

Run checks directly (no `systemd-run` inside the sandbox). Read-only
references: `/usr/share/omarchy/shell/Ui/{BarWidget,BarIconButton,BarIndicator}.qml`,
`/usr/share/omarchy/shell/plugins/bar/widgets/{SystemUpdate,Workspaces,ActiveWindow,KeyboardLayout}.qml`
and their `.manifest.json`, `/usr/share/omarchy/shell/plugins/bar/Bar.qml`
(lines 873–948: directory `FileView` watches), `/usr/share/omarchy/shell/plugins/bar/README.md`,
`/usr/share/omarchy/shell/Commons/{Color,Style}.qml`,
`/usr/share/omarchy/bin/omarchy-plugin-validate`,
`~/.config/omarchy/plugins/akitaonrails.ai-usagebar/manifest.json`,
`~/.config/omarchy/plugins/io.github.sahzudin.omarchy-chat/` (third-party
layout, README and marketplace form). Do not modify anything outside
`~/Projects/omarchygram-bar`. Do not commit.

## 4. Later (not in this spec)

Right-click actions (mute/DND) need an app IPC surface that does not exist;
a popup panel listing unread chats would put chat titles on disk — decide
separately. AUR packaging (`omarchygram`, `omarchygram-bin`) is a separate task.
