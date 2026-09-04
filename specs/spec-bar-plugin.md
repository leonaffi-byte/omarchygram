# Omarchy bar plugin for Omarchygram — spec

Decided 2026-09-04 (user: "the bar thing should be a plugin", "it can be a
different package"). Two deliverables:

- **Part A (app, orchestrator-owned):** Omarchygram writes a small status file
  whenever its unread total or voice-call state changes.
- **Part B (plugin, separate package `~/Projects/omarchygram-bar`):** an Omarchy 4
  shell plugin of kind `bar-widget` (Quickshell/QML) that watches that file and
  shows an icon, an unread badge and the in-call state on the bar. Left click
  focuses the app window (or launches it).

Target: Omarchy 4.0 alpha, Quickshell 0.3.1 (verified on this machine at
`/usr/share/omarchy/shell`). The plugin follows the first-party widget contract
exactly; it never invents UI idioms, colors or fonts.

## 1. Status file contract (shared by A and B) — version 1

Path: `${XDG_STATE_HOME:-$HOME/.local/state}/omarchygram/status.json`
(the same state dir as `os-audit.log`). Directory mode 0700, file mode 0600.
Written atomically: `status.json.tmp` in the same directory → fsync → rename.

```json
{
  "version": 1,
  "running": true,
  "updated_at": "2026-09-04T06:12:33+02:00",
  "unread": 3,
  "unread_all": 12,
  "unread_chats": 2,
  "call": null
}
```

- `running`: `true` from app start; rewritten `false` on a normal exit. A crash
  leaves it `true` — accepted (the next start rewrites it; a click still just
  focuses/launches).
- `unread`: unread messages in chats that are NOT muted and NOT archived — the
  badge number, matching Telegram's own convention. `unread_all`: every chat.
  `unread_chats`: number of non-muted, non-archived chats with unread > 0.
  Per chat the count is `max(unread, unread_mark ? 1 : 0)`, the same rule
  `chatlist::folder_unread` uses. Local virtual chats (Assistant/Omarchy) count
  like any other chat.
- `call`: `null` when there is no call, including the `Ended` phase. Otherwise:

```json
{ "phase": "requesting|incoming|exchanging|connecting|active",
  "peer": "Marta", "outgoing": false, "muted": false,
  "connected_at": "2026-09-04T06:10:01+02:00" }
```

  `connected_at` is `null` until `active`. Phases map 1:1 from `tg::CallPhase`.
- On logout: `unread*` → 0, `call` → null. On `Ended`: `call` → null.
- **`peer` is untrusted display text.** Consumers render it as text only —
  never into a shell command, a file path or a URL.
- Unknown fields must be ignored by consumers; `version` bumps only on a
  breaking change.

## 2. Part A — app status hook (orchestrator; `src/`)

- `src/tg/mod.rs::paths`: add `state_dir()` → the path above (honour
  `XDG_STATE_HOME`; reuse whatever `src/os` already does for `os-audit.log`).
- New `src/status.rs`: `StatusWriter` (UI thread, `Rc`). API:
  `set_unread(unread, unread_all, unread_chats)`, `set_call(Option<&CallInfo>)`,
  `set_running(bool)`, `flush_now()`. Every setter marks dirty and arms ONE
  one-shot `glib::timeout_add_local_once(150 ms)` that serializes and writes;
  the timer is never repeating (Wave 8E discipline). A write is skipped when the
  serialized document equals the last one written. `set_running(false)` and
  logout call `flush_now()` synchronously.
- Path override: `OMG_STATUS_PATH` env var; `--smoke` sets a throwaway file
  (same pattern as `OMG_SETTINGS_PATH`), so probes and the gate never touch the
  real state file.
- Hooks in `src/ui/shell.rs`: after `dialog_upsert` (recompute the three totals
  over the live `summaries` map — O(n), fine behind the debounce),
  in `handle_call_changed`, on logout, and on app shutdown (`running:false`).
- Probe (gate coverage): counter `probe_status_writes: Cell<u64>` incremented on
  every actual write. Add two steps: after the chat-list settles, `poll_until`
  the file exists, parses, and `unread >= 0`; during the mock call flow (after
  the accept step), `poll_until` `call.phase == "active"` and `peer` non-empty;
  after hang-up, `call == null`. The 12-run `bin/gate` must stay green.
- Security: 0600/0700 as above; `serde_json` only; no user text in paths.
  Reviewed by `security-reviewer` before merge (user data on disk).

## 3. Part B — the plugin (codex; repo `~/Projects/omarchygram-bar`)

### 3.1 Files (exactly these; no symlinks, no binaries, do not commit)

```
manifest.json
BarWidget.qml
Model.js
tests/run.js
tests/fixtures/{absent,not-running,idle,unread,unread-muted-only,incoming,outgoing,active,active-muted}.json
README.md
LICENSE            (MIT)
MARKETPLACE_SUBMISSION.md   (draft of the Omarchy marketplace issue form)
.gitignore
```

`CLAUDE.md`, `AGENTS.md`, `SPEC.md` already exist in the repo — do not modify.

### 3.2 manifest.json

Validated by `omarchy plugin validate` (schema mirrored from
`/usr/share/omarchy/bin/omarchy-plugin-validate`). Required: `schemaVersion: 1`
(JSON number), `id`, `name`, `version`, `kinds`, `entryPoints`.

- `id`: `leoom.omarchygram` (the `omarchy.*` namespace is reserved).
- `name`: `Omarchygram`; `version`: `0.1.0`; `author`: `leoom`; `license`: `MIT`.
- `description`: "Unread badge and call state for the Omarchygram Telegram client."
- `kinds`: `["bar-widget"]`; `entryPoints`: `{ "barWidget": "BarWidget.qml" }`.
- `barWidget`: `displayName` "Omarchygram", `description`, `category`
  "Communication", `aliases` `["telegram", "omarchygram", "tg"]`,
  `allowMultiple: false`, `defaultSection: "right"`, `defaults` and `schema`
  for the settings below. Copy the entry shape (`key`, `type`, `label`,
  `description`, `defaultValue`) from
  `~/.config/omarchy/plugins/akitaonrails.ai-usagebar/manifest.json`.

Settings (read with the base `setting(name, fallback)`):

| key | type | default | meaning |
|---|---|---|---|
| `icon` | string | `""` | Nerd Font glyph (paper plane; present in the bar font) |
| `showCount` | boolean | `true` | show the number, else a dot when unread > 0 |
| `countMuted` | boolean | `false` | badge uses `unread_all` instead of `unread` |
| `hideWhenIdle` | boolean | `false` | hide the widget when 0 unread, no call |
| `windowClass` | string | `"dev.leoom.Omarchygram"` | Hyprland window class to focus |
| `launchCommand` | string | `"omarchygram"` | run when no window matches |

### 3.3 BarWidget.qml — behaviour

Extends `BarWidget` (`qs.Ui`) with `moduleName: "leoom.omarchygram"`.

- **Data:** one `FileView` on
  `(Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state") + "/omarchygram/status.json"`
  with `watchChanges: true`, `printErrors: false`,
  `onLoaded: root.apply(text())`, `onFileChanged: reload()`,
  `onLoadFailed: root.apply("")` — exactly the pattern in
  `/usr/share/omarchy/shell/Commons/Color.qml` (`userShellFile`). Parsing and
  all state derivation live in `Model.js`; the QML only binds to the result.
- **Rendering:** the icon through `BarIconButton` (`slotSize: Style.bar.statusSlot`,
  `fontSize: Style.font.caption`, `bar: root.bar`, `tooltipText`) — the same
  component and sizes as `plugins/bar/widgets/SystemUpdate.qml`. The count is
  a text glyph beside it drawn the way `Workspaces.qml` draws its numbers
  (`Style.font.caption`, colors from the `Color` singleton in
  `Commons/Color.qml` only). While a call exists the glyph becomes `""`
  (phone) tinted with the shell's accent token. No literal colors, no shadows,
  no animation, no custom fonts.
- **States (from `Model.js.view`)**
  - file absent or `running:false` → icon only, tooltip "Omarchygram is not running".
  - idle (0 badge, no call) → icon; hidden entirely if `hideWhenIdle`.
  - unread → icon + number (or dot); tooltip "N unread in M chats".
  - `incoming` → phone glyph, tooltip "Incoming call from PEER";
    `requesting`/`exchanging`/`connecting` → "Calling PEER…" (outgoing) or
    "Connecting to PEER…"; `active` → "In call with PEER · MM:SS" plus
    " · muted" when muted. Elapsed comes from `connected_at`.
- **Timer:** one `Timer { interval: 1000; repeat: true }` whose `running` is
  bound to `phase === "active"` only — it must stop the moment the call ends.
  No other `Timer`, no `Process` blocks at all.
- **Click:** `onPressed` → `root.bar.run(cmd)` with
  `cmd = "hyprctl dispatch focuswindow " + Util.shellQuote("class:^(" + escapeRegex(windowClass) + ")$") + " || " + Util.shellQuote(launchCommand)`
  (`Util` from `qs.Commons`, as `KeyboardLayout.qml` does). This is the ONLY
  `bar.run` call site. Only setting values (the user's own config) ever enter
  the command; `peer` never does.
- **IPC:** `IpcHandler { target: "leoom.omarchygram" }` with
  `function refresh(): void { root.broadcast("reload") }`, mirroring
  `SystemUpdate.qml`. `reload()` re-reads the file.
- `implicitWidth`/`implicitHeight` from the content row; `visible` from the
  model. Vertical bars: fall back to icon-only (`root.vertical`).

### 3.4 Model.js — pure, testable

```
parse(text)                       → status object | null   (invalid JSON → null; version !== 1 → null)
view(status, settings, nowMs)     → { visible, glyph, badge, tooltip, inCall, callActive, muted }
formatElapsed(connectedAtIso, nowMs) → "MM:SS" (HH:MM:SS past an hour)
escapeRegex(s)                    → s with regex metacharacters escaped
```

`tests/run.js` (plain Node, no dependencies) loads each fixture, calls
`view`, and asserts the expected `visible/glyph/badge/tooltip` — exit 0 on
success, non-zero with a message on the first failure. Cover: absent,
not-running, idle, idle+hideWhenIdle, unread with/without `showCount`,
`countMuted` switching `unread`→`unread_all`, incoming, outgoing, active
(elapsed formatting), active+muted, and `formatElapsed` edge cases (0 s, 59 s,
1 h+, null).

### 3.5 README.md (user-facing)

Install: `omarchy plugin add https://github.com/leoom/omarchygram-bar.git --enable`
(placement prompt; or `omarchy bar put leoom.omarchygram --after omarchy.clock`).
Requirements: Omarchy 4 shell; Omarchygram with the status file (link to the
app). Settings with `omarchy bar set leoom.omarchygram <key> <value>` and the
table above. Troubleshooting: find the window class with `hyprctl clients`
(`class:` line) and set `windowClass`; `omarchy plugin validate` for authors.
Keep it short and plain; no marketing copy.

### 3.6 Acceptance (machine-checkable — the verifier runs these)

1. `omarchy plugin validate ~/Projects/omarchygram-bar` → exit 0.
2. `node tests/run.js` → exit 0.
3. `jq -e '.id=="leoom.omarchygram" and .kinds==["bar-widget"] and .entryPoints.barWidget=="BarWidget.qml" and .barWidget.defaultSection=="right"' manifest.json`.
4. `grep -c "bar.run(" BarWidget.qml` = 1; `grep -c "Process {" BarWidget.qml` = 0;
   `grep -c "Timer {" BarWidget.qml` = 1; no `#[0-9a-fA-F]{3,8}` color literals in
   any `.qml`; `find . -type l` empty.
5. `git status --porcelain` lists only the files in §3.1 (nothing else touched).
6. Live (orchestrator): installed as a git checkout under
   `~/.config/omarchy/plugins/leoom.omarchygram`, enabled, and
   `journalctl --user _COMM=quickshell` shows no error or warning mentioning
   `leoom.omarchygram`, `PluginRegistry` or a QML error since enabling; a bar
   screenshot goes to the `ui-reviewer`.

### 3.7 Worker rules (codex)

Run checks directly (no `systemd-run` inside the sandbox). Read-only references:
`/usr/share/omarchy/shell/Ui/{BarWidget,BarIconButton,BarIndicator}.qml`,
`/usr/share/omarchy/shell/plugins/bar/widgets/{SystemUpdate,Workspaces,KeyboardLayout}.qml`
and their `.manifest.json`, `/usr/share/omarchy/shell/plugins/bar/README.md`,
`/usr/share/omarchy/shell/Commons/{Color,Style}.qml`,
`/usr/share/omarchy/bin/omarchy-plugin-validate`,
`~/.config/omarchy/plugins/akitaonrails.ai-usagebar/manifest.json`,
`~/.config/omarchy/plugins/io.github.sahzudin.omarchy-chat/` (third-party
layout, README and marketplace form). Do not modify anything outside
`~/Projects/omarchygram-bar`. Do not commit.

## 4. Later (not in this spec)

Right-click actions (mute/DND) need an app IPC surface that does not exist yet;
a popup panel listing unread chats would need chat titles in the status file
(more user data on disk) — decide separately. AUR packaging of the app
(`omarchygram`, `omarchygram-bin`) is a separate task.
