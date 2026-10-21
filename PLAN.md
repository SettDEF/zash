# zash — the plan

**zash** (zett + shell): a terminal emulator written entirely in Rust, part of
zett and usable on its own. Name chosen 2026-10-16; clear on the US register,
EU/DPMA still to check by hand before anything permanent.

## The idea in one paragraph

One terminal that is correct, native-feeling, **among the fastest there are
and perfectly smooth**, with images and precise keyboard input, Lua all the
way down, sessions that never die, commands as blocks, a completion popup with
previews, smart output, triggers and instant replay, and instant new windows —
plus what no terminal has today: Vim everywhere, smart output from normal
commands, per-pane power limits, commands as notes in a workspace, and agents
you can watch. Local-first: no account, no cloud.

## Principles

1. **Correct first.** A terminal that draws one program wrong is broken.
   vttest and esctest pass; Neovim, tmux, htop, btop, less, fzf, Claude Code,
   mc, ssh and full-screen TUIs render exactly.
2. **Fast and smooth, measured.** Budgets below, checked on every release.
3. **Idle means idle.** Nothing changing = no frames, no timers, ~0 % CPU.
4. **Customizable to the bone, beautiful by default.** Every key, colour,
   animation, popup, layout and behaviour is a setting or a Lua function; a
   fresh install already looks finished (the Doom rule).
5. **Yours.** Local-first, no telemetry, no account. AI is optional and can be
   fully local.

## Features, by area

### 1. The core terminal
- **The environment programs see** is right: TERM, COLORTERM, TERM_PROGRAM, a
  UTF-8 locale, the start folder, and none of the variables of the terminal
  or multiplexer zash was started from (tmux, WezTerm, kitty…).
- VT/xterm emulation (vte parser, own grid), 24-bit colour, undercurl and
  coloured underlines, hyperlinks (OSC 8), clipboard (OSC 52), cwd (OSC 7),
  shell marks (OSC 133), notifications (OSC 9/777), synchronized output.
- the **Kitty keyboard and graphics protocols** (the standards for precise keys and images), Sixel, iTerm2
  images.
- Reflow on resize, unlimited compressed scrollback.
- Ligatures (rustybuzz), font fallback chains, emoji, Nerd Font symbols;
  box drawing and powerline glyphs drawn pixel-perfect by zash, not the font.
 

### 2. Speed and smoothness
- wgpu renderer with a glyph atlas and damage tracking: only changed cells redraw.
- Throughput and input latency among the best of any terminal (measured with vtebench and a latency probe).
- **Server mode:** one zash process serves every window, so a new window opens
  instantly.
- Pixel-smooth scrolling with momentum, at the display refresh rate.
- **Animated cursor**: a smooth trail, any curve, configurable or off.
 
- Smooth resize and font zoom.

### 3. Native feel
- Native-looking window and tabs, tinct-themed live (changes with the
  wallpaper), ii-decor window buttons, blur and transparency.
- Ships many themes; any theme can follow tinct instead.
- Zero config needed: good fonts, padding and keys out of the box.

### 4. Blocks: commands as units
- Shell integration (zsh first, bash and fish next) marks every prompt,
  command and output: a **block**.
- Jump between prompts, select or copy a whole output, collapse it, a sticky
  header while it scrolls, exit status, duration and cwd on each block.
- Rerun, edit-and-rerun, **compare two runs**, search all past output.
- Send a block to a zett page, link it, share it as a snippet.

### 5. The command line as an editor
- The line you type is edited by **zett-edit**: real Vim grammar (or plain
  editing for non-Vim users), with mouse selection, multi-line editing and
  syntax highlighting of the command.
- **Completion popup**: candidates from zsh's own completion system (every
  `_git`, `_cargo`, `_docker` works), with descriptions, groups, icons, a
  **preview pane** (file contents or image, branch log, the man-page line for a
  flag), fuzzy filtering, multi-select. Position, size, columns, icons and
  animation all styleable.
- **Ghost text** from history, accepted with →.
- Optional suggestions from an agent (local model or Claude), clearly marked.

### 6. Smart output from normal commands
- Recognised outputs become **tables you can sort, filter and click**: `ls`,
  `ps`, `git log`, `git status`, `docker ps`, `kubectl get`, `df`, JSON, CSV.
  The raw text is always one key away.
- **Hints**: two keys grab or open any URL, path, hash, IP or `file:line`.
 
- Hover a path for a preview (text, image, PDF page, directory).
- **Error lens**: compiler and test errors are highlighted; one key opens the
  line in zett (or Neovim), another hands the error to an agent.
- Custom renderers in Lua: "when a command prints X, show it as Y".

### 6½. Rich text from scripts: sizes, fonts, columns
Rows are not stuck at one font and one size. A script can say so, and zash
draws it — without breaking the grid for everything else.
- **Text sizes**: the published text-sizing escape (OSC 66: scale 1–7, fractional
  sizes, width in cells), so titles, banners and dashboards print big; other
  terminals that support it show the same thing.
- **Fonts per span**: a zash extension sets the family, weight, style and
  colour of a run of text (`sans` for prose, `serif` for a heading, the
  terminal font for code), falling back to plain text elsewhere.
- **Columns**: a row can be split into columns with their own widths and
  alignment (left, right, centre, decimal), so tables and side-by-side output
  line up even with proportional fonts.
- **From any script**, no escape codes to remember:
  `zash print --size 2 "Build finished"`,
  `zash print --font serif --weight bold "Chapter 1"`,
  `zash print --columns 30%,*,10ch "name" "description" "size"`,
  and a tiny library for Lua, shell and Python scripts.
- Selection, copying and scrollback still treat it as text; the grid
  underneath stays intact, so full-screen programs are unaffected.
- Needs zash's own pre-parser in front of the VT parser (unknown escapes are
  dropped there today): planned with T5, smart output.

### 6¾. Drawing and animation (2026-10-21)
More than text: shapes drawn by hand, animations from EffectCraft (Lottie) and VectorCraft (SVG),
things that react to the pointer, widgets. Always attached to text (they scroll and resize with
it), in the theme's colours, and plain text in every other terminal.
1. **Base** — the hand-drawn layer on the effects escape (OSC 7733;fx, `src/fx.rs`):
   - [x] `scribble`: the text written, then a loop drawn round it (2.2 turns, wobbling, each turn
     wider and turned, the centre drifting, a superellipse that never runs over the text, the pen
     thin at both ends, which do not meet); one loop per word, the same every time; stays drawn
     without asking for frames; strokes drawn as dots in the rectangle pipeline (no new GPU code)
   - [x] programs check TERM_PROGRAM=zash, or ask (over SSH too): OSC 7733;q is answered with the
     version, the effects, the marks and images=kitty
   - [x] more marks on one pen (smooth strokes through a few points, wandering, tapered, pen lifts):
     underline, strike, box (sharp corners, overshoot), check, arrow (shaft + head), bracket
   - [x] ZASH_FX_AT=SECONDS freezes the effects clock: exact, repeatable captures
   - [x] frames tested headless at set times against kept images: scripts/visual.sh (tests/visual:
     cases, bytes, images; the built-in look, no user theme; --update keeps new ones); proven to fail
     on a change (2.2 → 1.8 turns)
2. **Pictures and Lottie** — Kitty images exist (`graphics.rs`).
   - [x] Lottie (2026-10-21, `src/anim.rs`): velato reads it, vello_cpu draws each frame on the CPU
     (velato 0.12 is tied to vello 0.10 / wgpu 29, zash is on wgpu 30: its RenderSink drawn onto
     vello_cpu instead, no second GPU stack); OSC 7733;anim;file=…;rows=N;loop;speed;color.RRGGBB=
     accent|fg|red…|hex (theme colours swapped in); a thread per animation, at most 30 frames a
     second, stopping when done or gone; `zash anim check` (size, length, what will not play: text
     layers, pictures, effects, expressions), `zash anim play file --rows 4 --color ff0000=accent`;
     visual tests anim-0/anim-1 (the frozen clock drives it too)
   - [ ] text slots (the program's text drawn in the animation), SVG (VectorCraft), a scrubber in
     `zash anim play`, EffectCraft exports with `effectcraft-cli`; vello on the GPU once velato moves on
3. **Interaction** — hover and click on what is drawn, named states (idle, hover, active, done) with
   smooth changes, events back to the program, Tab and Enter reach everything.
4. **Widgets** — button, progress, table, list, input on the same layer; `zash ui …` for scripts.
5. **Shaders and type** — custom GPU effects within a cost limit, variable fonts, animated weight,
   fonts for titles.
6. **Recording, pen, made by the agent, sound** — **Drawings that move with the terminal (planned 2026-10-21):** inside zeit a stroke belongs to
   the zeit pane it was drawn on and moves, resizes, hides and scrolls with it (zeit reports pane
   rects and scroll over OSC 7733); a resize that re-wraps text keeps strokes on their text (see
   ink.rs). — **Pen brushes from PhotoCraft (planned 2026-10-21, not built):** drawing mode draws with
   photocraft-paint (github.com/storytold/photocraft, MIT or Apache-2.0, pure Rust; a git dependency
   pinned to a commit, never the craft-sync build cache): its built-in presets (General, Dry Media, Wet
   Media, Special Effects: pencil, ink, marker, airbrush, watercolour…), pressure / tilt / rotation
   response curves and smoothing (pulled string, catch-up). Strokes become dabs rendered into a picture
   per pane (the image layer), the dots of ink.rs stay as the light fallback; `[pen] brush = "Ink"`, a
   brush picker in drawing mode, and the same preset names Knurl uses, so a pen tested there draws the
   same here. Its Wayland pen reading (pen.rs) is what photocraft-tablet says it lacks: worth offering
   back. — video/GIF of what is drawn (tinct-cast), the Z13 pen
   as vector strokes, effects written by the agent from a sentence, optional small sounds.
   **The pen** (2026-10-21): winit 0.30 has no pen events, so zash binds the Wayland tablet protocol
   (zwp_tablet_manager_v2, already in wayland-protocols' unstable set) on its own connection, as the
   desktop mode does; strokes (points with pressure, tilt, rotation) are drawn with fx.rs's pen and kept
   as vectors attached to the cells they were drawn over (scroll, resize, scrollback, saved with a
   session); `[pen]` in zash.toml (on/off, colour, width from pressure, eraser end erases, a button
   clears, smoothing); zeit draws over panes, zett keeps handwriting and sketches in notes. The pen's
   own feel (curves, buttons, profiles, plugins) comes from Knurl's pen layer (knurl PLAN, M11).
Always: under 2 ms a frame, nothing drawn while nothing moves, paused when hidden, slower on battery,
reduced motion, no flashing, limits on size and time, graphics from SSH limited by default.

### 7. Scrollback and history
- **Vim in the scrollback**: `v`, `y`, `/`, `n`, text objects over output.
 
- **Instant replay / time travel**: scrub back through a session like a video
  and see the screen at any moment.
- Sessions **survive reboots**, scrollback included.

### 8. Panes, tabs, sessions
- Splits, tabs, workspaces, zoom a pane, swap and move panes, saved layouts
  per project, all on the `SPC` leader (zett's which-key tree).
- Background session server: closing the window never kills work; attach from
  another window, the TUI, or over SSH.
- **SSH that brings zash with it**: shell integration, completions and images
  work on remote machines (nuke).
- Broadcast input to several panes.
- Drop-down (Quake) terminal, scratch panes, a floating pane pinned over the
  desktop.

### 9. Programmable
- `~/.config/zash/init.lua`, Lua like Neovim, in the same shape as zeit's:
  a **preset** as the starting point, a Doom-style **module list** (words and
  +flags), `setup{…}` for details. Every setting, keymap, popup style, status
  segment, theme rule, renderer, trigger and hook. (zash.toml keeps working
  for the basics until init.lua lands.)
- **Presets are starting points, not modes**: a named bundle of modules and
  settings, loaded first, then changed line by line below. Shipped: *plain*
  (fast and quiet), *glass* (blur, tinct, centred text, the blurred popup,
  gliding cursor and scroll: today's look), *focus* (no chrome, larger text),
  *retro* (CRT scanlines and glow, amber or green), *ide* (splits, the Neovim
  UI, zeit and zett panes). Anyone can write one (one Lua file) and share it
  like a theme; `zash preset list`, `zash preset try NAME` previews one
  without touching the config.
- **Triggers**: a regex on output runs an action.
- **Command rules** (with zeit): Lua `z.on_command` decides where a typed
  command runs (a bottom pane for builds that closes on success, a float for
  servers, a new tab for ssh); the zsh integration asks before the line runs.
  Where it was typed, a compact small-font line stays (`▸ cargo build ·
  running 0:12 · in the bottom pane`), updating to `✓` or a red `✗ exit N`,
  and a dropdown: `▸` unfolds the output inline (live, errors first), `▾`
  folds it; it jumps to the pane and back and stays in the scrollback.
  The line has columns in the same small font: command left, status
  right-aligned (the rich-rows feature: own font size per row, columns).
- **Hooks**: `command.start`, `command.end` (notify when a long command
  finishes while you are elsewhere), `cwd.changed`, `prompt`, `output`, `bell`.
- **Remote control**: everything zash does is a zett command, so scripts,
  keys, the palette and agents all drive it.
- Plugins: Lua for quick ones, WASM for heavy ones (the same system as zett).

### 10. What only zash has
- **Vim everywhere**: scrollback, command line and popups, with the same engine
  and keymaps as the zett editor.
- **Per-pane Vitals**: a badge with each pane's CPU and watts; run a pane under
  a preset (`background`, `quiet`) so a big build cannot make the desktop lag.
- **Commands are notes**: blocks go into zett pages; a sequence of commands
  becomes a runnable notebook page.
- **Agents in a pane you watch**: an agent runs commands in a visible pane,
  each step shown and, if you want, approved.
- **Part of the desktop**: shell widgets (running jobs, last failure), a Dotos
  task from a failed command, Quarry and Reel previews inline.

## Starting config: from WezTerm

zash's defaults start from the user's WezTerm setup (`~/.config/wezterm/`,
about 1,500 lines across keys, fonts, glass, format, …), **important features
only**: fonts and sizes, tinct colours, padding, opacity and blur, the key and
pane/tab bindings, the tab/status bar look, the ii-decor button overlay. Not a
line-by-line port: each feature is re-expressed the zash way, and WezTerm-only
workarounds are dropped.

## Architecture (all Rust)

```
crates/
  zash-vt        VT state machine and grid: vte parser, scrollback
                 (compressed), reflow, Kitty keyboard + graphics, sixel,
                 OSC 7/8/9/52/133; no I/O; fully unit-tested
  zash-pty       ptys and processes (rustix); the session server; attach/detach
  zash-shell     shell integration plugins (zsh, bash, fish) and their socket
                 protocol: marks, the edit buffer, completion candidates
  zash-render    wgpu renderer: glyph atlas, shaping (rustybuzz), fallback,
                 box drawing, damage tracking, smooth scroll, cursor animation
  zash-ui        blocks, popups, hints, tables, previews, panes/tabs layout;
                 all Lua-styleable
  zash-smart     output recognisers (ls, ps, git, docker, JSON, CSV, errors)
  zash           the standalone app (winit + wgpu) and server mode
zett-app         embeds the same renderer as a pane in zett's window
```

The grid, the UI layer and the renderer are separate, so one terminal state
draws in the standalone window, inside zett, and (simplified) in the TUI.

## Performance budgets (checked on every release, on this laptop)

| What | Budget |
|---|---|
| key → pixels | < 8 ms p99 |
| throughput (vtebench) | among the best measured on this machine |
| scrolling / cursor animation | 120 Hz, no dropped frames |
| new window (server running) | < 50 ms |
| cold start | < 150 ms |
| completion popup after a key | < 30 ms |
| memory per tab (10k lines of scrollback) | < 30 MB |
| idle CPU | ~0 % |
| correctness | vttest + esctest pass |

## The bar

Before each release zash is measured against the best terminals installed on
this machine, on correctness (vttest, esctest, replayed sessions), throughput,
input latency, frame times while scrolling, start time, memory and idle CPU.
A release that is worse than the best on any of them is not done.

## Status (2026-10-17)

- Resolved (2026-10-20): "old colours after a wallpaper change" was not zash (it reloads its
  palette file): zeit never reloaded its look (tinct now runs `zeit source-file` after the tmux
  colours) and zsh read its colours only at start (a precmd check re-reads them now).
- Text effects (OSC 7733;fx, fx.rs / fx_draw.rs) and GLSL zfloat panels (panel_shader.rs,
  [zfloat.shaders]) are in. Measured with ZASH_BENCH: 30 frames/s at ~1 ms a frame; 20 % of a
  core on the software renderer, ~4 % on the GPU.

Done: T0 (vttest screens checked), most of T1 (pixel scrolling, cursor glide, a
cache keyed by row content), T2 (tinct, glass, tabs, splits with the user's tmux
keys, centring/offset), T3 basics (marks, failed-command bars, jump, copy last
output, search), the T4 popup (zsh's completions + history, Tab cycles, blur and
a look set from zstyle or zash.toml), the Neovim mode (zash as Neovim's screen,
gliding scrolls), files dropped on the window (own Wayland data device: paths
into a shell, `:e` in Neovim, the pane under the drop). zash is the desktop's
terminal.

**Later, after the zett editor (and its PDF viewer) are done:**
- images: the Kitty graphics protocol (icat, yazi, image.nvim), then sixel
- dragging out: a selection or a file name from the screen into other apps
  (a Wayland data source on the same data device as the drops)
- sessions that outlive the window: **zeit** (its own project, /mnt/storage/dev/projects/zeit), then Kitty keyboard protocol

## Milestones

- **T0 — a correct terminal.** vte + grid + pty + wgpu text; true colour,
  resize with reflow, scrollback; runs zsh, Neovim, tmux, htop, Claude Code
  correctly; vttest core screens pass.
- **T1 — fast and smooth.** Damage tracking, glyph atlas, ligatures, fallback,
  box drawing; pixel scrolling, cursor animation; server mode. Budgets measured.
- **T2 — native and themed.** tinct live theming, ii-decor overlay, tabs and
  splits, the WezTerm-derived defaults (fonts, keys, look), themes.
- **T3 — blocks.** zsh integration, marks, blocks, jump/select/collapse/rerun,
  sticky headers, exit codes and durations, search past output.
- **T4 — the command line.** zett-edit on the prompt, completion popup with
  descriptions and preview, ghost text; bash and fish integration.
- **T5 — smart output.** Hints, recognisers and tables, previews, error lens,
  Lua renderers; rich text from scripts (sizes, fonts per span, columns,
  `zash print`), on zash's own pre-parser.
- **T6 — programmable.** `term.lua`: everything configurable; triggers, hooks,
  remote control, plugins.
- **T7 — sessions.** Session server, attach, restore after reboot, SSH that
  brings zash along, broadcast input, drop-down and floating panes, instant
  replay.
- **T8 — zett and the desktop.** zash panes and live terminal blocks in zett,
  commands as notes, agents in watched panes, per-pane Vitals, shell widgets.

T0–T2 make it a terminal you can switch to; T3–T5 make it the smartest one;
T6–T8 make it something no other terminal is.

## Testing

- Unit tests on the grid and parser for every escape sequence handled.
- **Replay tests**: recorded sessions (Neovim, htop, tmux, fzf) replayed into
  zash's grid and compared cell by cell with a reference terminal's output.
- vttest / esctest runs; budgets checked by a perf script, like zett's.
- Visual checks headless (render to an image), never by capturing the screen.
