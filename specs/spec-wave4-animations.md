# Spec: Wave 4 — Animations (all 54 Motion Lab effects as settings toggles)

## Context — read first
The user reviewed a standalone HTML "Motion Lab" and chose to keep EVERY
effect as an individual toggle. `Settings.animations: BTreeMap<String,bool>`
(id → on) already exists; missing = off. The settings panel (wave 1) gains an
"Animations" section. Design authority: CLAUDE.md (terminal-adjacent, flat,
tokens only, JetBrainsMono). Reduced motion: if the GTK setting
`gtk-enable-animations` is false, every effect is a no-op regardless of toggles.
Rules: spec-ui.md C1–C15; no threads; canvas effects use `gtk::DrawingArea`
with `add_tick_callback` (drop the tick when the effect is off or the widget
is unmapped — never leak a tick).

## Architecture (create `src/ui/anim/`)
- `anim/mod.rs`: `pub struct Effects { settings: Rc<SettingsStore> }` with
  `fn on(&self, id: &str) -> bool` (toggle AND gtk-enable-animations) and
  hook methods called by the existing views (add the calls where noted):
  `message_added(row: &gtk::Widget, msg: &Msg, is_live: bool)`,
  `message_sent(row)`, `chat_switched(list: &gtk::Box, rows: &[gtk::Widget])`,
  `sidebar_selection_moved(from: Option<&gtk::Widget>, to: &gtk::Widget)`,
  `badge_changed(badge: &gtk::Widget, old: i32, new: i32)`,
  `mention(row: &gtk::Widget)`, `typing_frame(label: &gtk::Label, name: &str) -> Option<glib::SourceId>`,
  `image_loading(placeholder: &gtk::Label) -> Option<glib::SourceId>`,
  `reaction_added(chip: &gtk::Widget)`, `receipt_drawn(label: &gtk::Widget)`,
  `message_edited(row)`, `message_deleted(row) -> bool /* true = caller waits 500ms before removing */`,
  `theme_switched(overlay_host: &gtk::Overlay)`, `launched(overlay_host)`,
  `window_focus(app_root: &gtk::Widget, active: bool)`, `composer_idle(cursor: &gtk::Widget, idle: bool)`,
  `error_flash(overlay_host)`, `scroll_to_bottom_pressed(button)`,
  `unread_divider_added(row)`, `date_chip(chip: &gtk::Widget, show: bool)`,
  `send_pressed(button) -> glib::Promise-like: returns a Future<()> that resolves when the charge is done (chargesend) or immediately`,
  `attach_menu_opened(popover)`, `composer_typing(eq: &gtk::Box, typing: bool)`.
  Each hook checks its effect ids and does nothing when off. One CSS class
  per effect (`omg-anim-<id>`) toggled on the root `omg-window` mirrors the
  settings so CSS-only effects need no code path.
- `anim/css.rs`: a static CSS string appended by the theme engine? NO — theme
  CSS is orchestrator-owned; put all animation CSS into `src/theme/style.css`
  under a clearly marked `/* ---- animations ---- */` block using ONLY var()
  tokens and `@keyframes`. GTK4 CSS animates: opacity, colors, margins/padding,
  borders, `background-image`, `-gtk-icon-transform`, `box-shadow`. It does
  NOT support `transform`, `clip-path`, or `text-shadow` — effects that need
  those use code (labels swapped per frame, DrawingArea, or margin animation).
- `anim/overlays.rs`: overlay widgets added to the Shell's `gtk::Overlay`:
  scanlines (DrawingArea, cheap repeating lines, redraw only on resize),
  vignette (DrawingArea, radial gradient, breathe via tick 5s cycle),
  flicker (opacity tick, rare), static burst (DrawingArea noise, 300ms),
  scanline sweep, CRT power-on beam, boot log (a `gtk::Label` monospace over
  a dark box, lines appended every 120ms then removed), matrix rain
  (DrawingArea in the empty state only), grid shimmer (CSS gradient on the
  empty state, animated background-position via tick).
- Settings panel: "Animations" section listing every id below with its
  label + one-line description (verbatim from this spec), a `gtk::Switch`,
  and a "Preview" button that runs the effect once via the matching hook
  (using a throwaway widget or the live view, as the Motion Lab does).
  Presets row: "Purist (all off)", "Subtle", "Full phosphor" (same sets as
  the lab: subtle = fadeup, ticksweep, crossfade, receiptdraw, editripple,
  ellipsis, badgepop, selbaron, jumprocket, datefloat, composercursor,
  tsreveal, liveclock, onlinebreathe; phosphor = everything except
  typewriter/lineprint/instantcursor/borderdraw/entrynone and
  sendnone/switchnone which are radio alternatives).
- Radio groups (exactly one on): entry style {typewriter, decode, lineprint,
  fadeup, instantcursor, borderdraw, entrynone}; send feedback {invert,
  ticksweep, sendspin, sendnone}; chat switch {wipe, crossfade, cascade,
  switchnone}. The settings section enforces one-of by turning the others off.

## The effects (id — label — how to implement in GTK4)
Incoming message style (radio):
- typewriter — text types out with a block cursor — code: label text grows 1 char/18ms via timeout; cursor = "▌" appended, removed 350ms after.
- decode — glyphs scramble then settle — code: per-frame label text with random glyphs from `░▒▓#$%@&*+=-<>` for unsettled positions, 16ms.
- lineprint — reveals top-down in steps — code: a `gtk::Revealer` with `SlideDown` 280ms, `set_reveal_child(true)` after add.
- fadeup — soft rise, 160ms — CSS: `omg-anim-fadeup` keyframes opacity 0→1 + `margin-top: 4px→0`.
- instantcursor — instant + cursor blinks twice — code: cursor label blink via 2 timeouts.
- borderdraw — border traces around — code: animate `border-color` via CSS keyframes cycling `transparent→accent` on each side in sequence (4 keyframe steps).
- entrynone — hard cut.
Send feedback (radio): invert — CSS keyframe 90ms swapping bg/fg; ticksweep — a 1px accent box under the bubble grows `margin-end` 100%→0 via CSS; sendspin — Send button label cycles ⠋⠙⠹⠸⠼⠴ 60ms until the send resolves; sendnone.
Chat switching (radio): wipe — overlay dark box slides via `gtk::Revealer` SlideRight in, then out (260ms); crossfade — messages box opacity keyframe 100ms; cascade — each row `omg-anim-fadeup` with a per-row delay (i*20ms, via timeout adding the class); switchnone.
Reactions & receipts: reactburst — chip `omg-anim-pop` (opacity+margin) + 6 tiny `gtk::Label("·")` particles animated outward via margin keyframes then removed; reactroll — count label swapped after a 200ms opacity dip; receiptdraw — "✓" then "✓✓" 400ms later with a fade; editripple — `box-shadow` keyframe 0→6px transparent-accent 500ms; deletedissolve — 6 frames of glyph noise (code) then opacity→0 300ms, caller removes after 500ms.
Typing & badges: braille — typing label cycles ⠋⠙⠹⠸⠼⠴⠦⠧ 70ms; ellipsis — "typing." ".." "..." 240ms; badgepop — CSS pop keyframe (opacity + margin); badgeroll — old count fades up/out, new fades in from below (two labels in an overflow box, 220ms); badgepulse — CSS `box-shadow` glow keyframe 2.4s infinite on badges with count>0; bellshake — row `margin-start` ±3px keyframes 400ms; asciiload — placeholder text `[▓▓░░░░░░] photo` frames 110ms; selbaron — accent bar widget in the sidebar overlay whose `margin-top` animates to the selected row's y (180ms, tick-interpolated).
Scroll & navigation: jumprocket — ▼ button opacity dip + `margin-bottom` 6px keyframe 400ms; unreaddivider — divider row opacity 0→1 + width via revealer 400ms; datefloat — date chip revealer crossfade, auto-hide 900ms after last scroll.
Composer extras: chargesend — Send button gets a child fill box growing over 600ms (CSS margin keyframe); send fires after; attachunfold — popover children each `omg-anim-fadeup` with 40ms stagger; equalizer — 5 tiny boxes in the composer whose heights cycle via tick while the user types (text-changed → 1.5s window).
Phosphor atmosphere: bootlog — overlay label appends 7 fixed lines (app name/theme, gtk version, session ok, theme monitor, dialogs ok, updates live, ready) 120ms each, removed 450ms later — runs at launch; phosphorburn — new rows get `omg-anim-burn` (color bright→fg + box-shadow glow 1.1s); flicker — window overlay opacity 0/0.06/0.12 for 2 frames every ~6s; scanlines — permanent overlay DrawingArea (1px lines every 3px at 16% black); vignette — overlay radial darkening, breathing 5s; staticerror — noise DrawingArea 300ms on a failed send (hook `error_flash`); cursorcomet — composer cursor gets `box-shadow` trail class; thememorph — `omg-anim-thememorph` adds `transition: background-color .5s, color .5s, border-color .5s` to the main surfaces (GTK supports transitions on these).
Chrome & ambient: poweron — beam overlay: 2px box widens (margin keyframe) then heightens to full and fades, at launch; scansweep — 56px gradient box moves top→bottom via tick 600ms on theme switch (hook `theme_switched`); composercursor — blinking ▌ in the empty composer (CSS blink keyframe 1.06s steps); focusdim — root gets `omg-anim-dim` (opacity .72 on side+chat) on window inactive; hovertrace — CSS `:hover` underline via `border-bottom` width transition on chat rows (approximate with `border-bottom-color` transition); glitch — on hover, row title color flickers accent/yellow 2 frames (CSS keyframe on color); matrixrain — DrawingArea behind the empty state (columns of glyphs falling, 66ms tick, only while empty state shown); gridshimmer — empty-state background repeating gradient, `background-position` animated via tick 14s loop; tsreveal — CSS: `.omg-msg .omg-meta { opacity:0 }`, `:hover` → 1 with .18s transition.
Header & presence: liveclock — already exists (wave 1 header clock) — this toggle mirrors `header_clock` (turning either on turns both on); onlinebreathe — presence dot (a 6px accent box next to the title, shown when the mock/real backend reports online — add `online: bool` to ChatSummary? NO (backend is orchestrator-owned) — show the dot only in the header for chats with a recent incoming message (<5 min) as a stand-in; document it); unreadcomet — a small accent dot in the sidebar overlay animating from the top to the row's y with a fading trail on a background NewMessage.

## Files to create/modify (ONLY these)
`src/ui/anim/{mod,overlays}.rs` (NEW), `src/ui/mod.rs`, `src/ui/shell.rs`,
`src/ui/messages.rs`, `src/ui/chatlist.rs`, `src/ui/settings_view.rs`,
`src/theme/style.css` (animations block, tokens only).

## Probe traversal additions
1) With all animations OFF (default): existing traversal unchanged.
2) `store.update` with the "Full phosphor" preset → repeat: open chat,
   receive (send "hi"), react (if the UI has a react action; else skip),
   switch chat twice, mention path via the mock (a second chat's incoming),
   theme switch hook called directly, then `launched` hook; assert no panic,
   the app stays responsive (a `glib::timeout_future(200ms)` between steps),
   and every tick callback registered by effects has been removed when the
   effects are switched OFF again (keep a counter of live ticks in `Effects`
   and assert it returns to 0). 3) Toggle each radio group through all
   options once. Whole traversal < 45s.

## Must NOT touch
`src/tg/*`, `src/ai/*`, `src/os/*`, `src/local/*`, `src/settings.rs`,
`src/theme/mod.rs`, `src/main.rs`, `src/lib.rs`, `Cargo.toml`, `tests/`,
`specs/`, git state. No new dependencies.

## Acceptance criteria
As spec-wave2.md (build 0 warnings, both probes < 45s, tests, greps, real
settings untouched, latency probe) plus: `grep -c "add_tick_callback" src/ui/anim/` ≥ 1 and every one has a matching removal path (reviewer checks).
