# Changelog

All notable changes to norte are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow
[Semantic Versioning](https://semver.org/). The wire protocol is versioned
independently through `PROTOCOL_VERSION`.

## [Unreleased]

### Added

- **Organizing a directory** (ADR 0122, protocol 0.77.0, bridge 72), phase 8 of
  the WOW programme. `pane.organize` — in the palette, the File menu and the
  help in both locales — asks for a plan that puts the files of the current
  directory INTO FOLDERS, and comes back with a tree you review before anything
  moves. A tree and not a list of pairs, because what changes is the shape of
  the directory: forty rows of `a.pdf → facturas/2026/a.pdf` do not let you see
  how many folders appear, which ones, or what ends up inside each. Above the
  tree goes the count — "creates 3 folders and moves 12 files" — which is what
  you read to decide without counting lines, and which survives a box taller
  than the terminal, since those are cropped from the bottom. A folder that
  ALREADY existed is not painted as new: that is the difference between "this
  creates three folders" and "this puts things into folders you already had".
  Each line says what it is twice, with a role or a CSS class AND a marker
  glyph, because a colour does not survive a monochrome theme or a screen
  reader; the marker is never part of the name and the indentation is never
  spaces in the text, so a file called `+ facturas` cannot disguise itself as a
  new folder. Approving requires having reached the end of the tree, with
  scroll in both frontends and a mouse gesture in the window — otherwise that
  requirement made the screen unapprovable without a keyboard.

  Applying it is ONE batch: `fs.organize` creates the missing folders and moves
  everything under a single `batch_id`, so undoing it puts the files back and
  takes away the folders nobody else filled, in one step. `fs.create` plus
  `fs.move` from a client would have left a batch nobody owns. A destination
  that escapes the directory — an absolute path, a `..`, a segment that is not
  legal, a duplicated origin or destination — rejects the plan WHOLE, never
  halfway: a plan is an intention approved in one go.

  Plans come from a model (`ai.organize_plan`, through the same AI gate as
  renaming) or from an extension of the new kind `organizer`
  (`plugin.organize_plan`, WIT package `norte:organizer@0.1.0`) — the plugin
  proposes and the core executes, exactly as ADR 0095 set out for renamers, and
  the two plans are indistinguishable downstream because what makes the
  operation safe is not where the names came from. An organizer appears in the
  palette with its own label, as `PluginCommandKind::Organizer`. The plan's
  token travels WITH the plan rather than in a second call, which keeps the
  digest in one place and removes the window where a human stares at a plan
  that cannot yet be approved.

- **The journal timeline, and undoing back to a point** (ADR 0121, protocol
  0.76.0), phase 7 of the WOW programme. A new `timeline` panel — in the panel
  bar for every preset, and in the View menu — lists what has been done on this
  machine, newest first: the time, who did it (you, an agent or an extension,
  by the dot's colour), the verb and what it was done to. A batch is ONE row
  and says how many entries it carries, because it is undone whole or not at
  all. Point at a row, press Enter, and it asks whether to undo what YOU did
  after it — the row you pointed at stays, since it is the state you want back.
  The question carries the count before you answer, in three numbers that do
  not add up to one: what will be undone, what will be skipped, and what is not
  yours and is never touched. Two RPCs back it: `journal.list` (paginated
  backwards by `seq`, capped at 200 rows) and `journal.undo_after` (a Task,
  reported through the existing `policy.undo_report`, because it is the same
  undo with a different selection). Both are refused to an agent connection
  before their params are even parsed: the journal names everything touched on
  this machine, which for a scoped agent is an existence oracle, and undoing
  the human's work is not an agent's decision. A batch the cut falls inside is
  excluded WHOLE — slicing one would revert half a `fs.rename_batch` believing
  it whole — and a cut that names no entry is refused rather than read as
  "everything since the beginning". Paths on the wire are sanitised and carry a
  `hostile` flag, the same treatment `fs.search` gives its lines, because a
  filename is chosen by whoever creates the file and this is the screen where a
  human decides what to revert.

- **`app.goto` — go anywhere from one screen** (ADR 0120), phase 6 of the WOW
  programme. `ctrl+g` in `orthodox`, `cua` and `vim`, and first in the Go menu
  for every preset, opens one list with sections: the path you are typing,
  this panel's history, the places you return to, your bookmarks, your
  connections, the commands, and whatever the semantic index finds. Typing
  filters by subsequence, the arrows skip the section titles, and Enter
  either navigates or runs the command through exactly the same dispatch its
  key would. It replaces none of the six screens it draws from — each keeps
  its key and the things only it can do — it is the one for when you cannot
  remember which of them held your answer. A source is a trait
  (`norte_frontend::goto::GotoSource`) and the model owns the order, the
  filter, the headers, the per-section cap and the cursor, so a seventh
  source is one `impl` and one line. The index is asked from three characters
  on, lands in its own section without moving the cursor, is dropped when the
  answer is to a query that is no longer typed, and passes the same
  validation belt as `ai.search`. A typed path counts as one when it starts
  with `/`, `~` or a scheme — never relative, because where you are going
  cannot depend on where you were — and `~` is the process home, not the
  panel's. Only the focused panel's history carries that panel's name
  reinterpretation; every other section is built without one, because a
  panel's reinterpretation applied to another's paths invents mojibake. The
  four imported presets leave `ctrl+g` unbound and say why in their headers:
  none of the managers they transcribe has an equivalent key.

- **`Tab` walks the extension manager in the terminal** (ADR 0119). The
  manager's card (ADR 0104) came with a row of buttons — enable, approve,
  settings, uninstall, help — that only the mouse could press as buttons:
  `tab` resolved to `dialog.pane`, the command was live in the catalogue, and
  the screen ignored it in silence. It now moves the focus from the list to
  each button and back to the list; `Enter` fires the focused one, the button
  lights up, and the list's cursor dims while the focus is away, because two
  equally bright cursors do not say which one gets the keys. The ring's stops
  are the buttons THE FRAME PAINTED, so a terminal too narrow for the card
  has nowhere to go and `tab` does nothing, and a focus left pointing past
  the painted buttons fires nothing at all rather than a verb the reader
  never read — one of those verbs is uninstall. Moving the list cursor hands
  the focus back to the list, since the buttons belong to the selected
  extension. `tab` is deliberately NOT printed in the footer: at 80 columns
  that footer already held five verbs in 67 of its 71 cells, and a sixth cut
  the fifth mid-word. The `plugins` help topic says so instead, in both
  locales. The window needed no change — its buttons are real `<button>`
  elements and the browser's own focus ring already walked them.

- **The terminal viewer shows an image as an image**, not as a hex dump
  (ADR 0118). At startup the TUI asks the terminal, once, whether it speaks
  kitty's graphics protocol (an APC query followed by a DA1, so a terminal
  that stays silent on the first still answers the second). `[ui] images`
  picks what happens next: `auto` (the default) uses real pixels when the
  probe said yes and falls back to coloured half-blocks otherwise; `kitty` and
  `blocks` force one of the two without asking the terminal again; `off`
  leaves the viewer on hexview. The key is re-read live but does NOT change a
  viewer that is already open: the mode is pinned when it opens, because
  switching midway would leave placed pixels nobody knows how to erase. The
  pixels are fed by the same `thumbnail`
  plugin kind the window already uses (`image-thumb`, protocol 0.73.0, ADR
  0107) and are written to the terminal after each frame, outside ratatui,
  because a graphics escape does not fit in a cell — erased the moment the
  viewer closes, moves to another file, the terminal is suspended, or norte
  exits. The half-blocks were already there, painted by an approved
  `previewer` plugin (`image-ansi`); what is new is that the viewer now SAYS
  when the extension it needs — `image-thumb` for pixels, `image-ansi` for
  half-blocks, they are not interchangeable — is not approved and enabled,
  instead of silently showing raw bytes. Only **PNG** thumbnails are placed:
  kitty's protocol cannot announce a JPEG or a WebP, and a `thumbnail` plugin
  may return any of the three (this repository's falls back to JPEG when the
  PNG does not fit its size cap). One that arrives in another format is
  discarded rather than sent wrong, and the viewer says THAT, with a notice
  distinct from "approve one" — pointing a reader at F12 to approve what is
  already approved is a dead end. Inside tmux without
  `allow-passthrough` the probe correctly reports no pixel support and the
  viewer falls back to half-blocks on its own — and still warns exactly as it
  would outside tmux if no `image-ansi` is approved yet: falling back to
  half-blocks does not by itself mean the file is shown. Sixel is out of
  scope: it needs a colour quantizer this project has no real use for. The
  window is unaffected — it paints images through its own webview and does
  not read `[ui] images`.
- **The core can measure what a directory is made of** (protocol 0.75.0,
  ADR 0117). `fs.dir_usage` walks one directory as a cancellable task and
  `fs.dir_usage_report` collects what it measured: one entry per child with the
  size of its whole subtree, which is what a listing cannot tell you and what a
  disk map is drawn from. **The terminal paints it**: `alt+z` opens a disk map
  panel where every child is a rectangle sized by what it takes up, the arrows
  walk them, Enter goes into the selected one and a click does both at once. It
  answers "where did my space go?", which a listing sorted by size cannot —
  there a directory weighs what its own node weighs, not what is inside it.
  **The window has it too**: the same panel, measured by the host and painted
  from the same repartition, so the rectangle you see and the one a click opens
  are the same one. It measures what the listing is showing, once per directory
  rather than once per keystroke, and a click walks into the child under it.
  It declares what it does not know instead of rounding it off: a child whose
  subtree could not be fully read is marked rather than reported short, a
  listing the provider itself admits it truncated is never announced as
  complete, and a map that was cancelled mid-listing says so instead of looking
  like a small directory. Past 4096 children the largest travel and the rest
  are counted, so the big rectangle is always the one you can see. Reading it
  needs the same permission as listing the directory — it opens no file — and
  an agent does not get to measure the directories it is not allowed to walk.
- **A plugin can paint a whole panel** (protocol 0.74.0, WIT
  `norte:panel@0.1.0`, window bridge 70). A consented plugin contributes a
  panel kind, `plugin:<id>:<kind>`, that a layout can place like any other:
  the terminal and the window both declare it, give it the keyboard, offer it
  in the layout picker and paint the frame its guest describes — styled lines
  plus clickable zones. The guest does not draw; it DESCRIBES, and the border,
  the title and the focus ring stay ours, so a plugin cannot impersonate
  another panel. A zone runs a command from the catalogue, filtered to the
  same scope the panel's keys have: the plugin chooses the label and the
  command, and nothing ties them together, so a zone labelled "Refresh" cannot
  name something that copies files. What the guest remembers between repaints
  is an opaque blob it gets back untouched; the permission to read is minted
  per call and dies with it. A slow or broken plugin keeps its last frame
  instead of blinking, and one that is gone leaves a box with its title rather
  than a silent gap.
  `[ui] splash` is `brief` (a cover any key takes away, with the build, the
  core it talks to and a compass), `home` (it stays until a key, with the
  directories you go to most and your bookmarks, each opened by its number) or
  `off`; `--no-splash` and `NORTE_NO_SPLASH` turn it off for one run, and the
  first-run wizard still comes first. `[ui] processes_panel = "auto"` opens the
  processes panel when a task starts and closes it a few seconds after the
  last row finishes — the seconds a finished row stays on the board, so the
  panel never vanishes at the very moment that says something failed. It takes
  the keyboard from nothing, and never closes a panel you opened yourself.
  Its rows now say how fast the task is going and how long is left, computed
  from successive snapshots because the protocol carries no rate. Searching,
  comparing or checksumming never open it: each has a surface of its own, and
  the rule that tells work from observation is shared by both frontends.
  In the window the same screen opens at start-up and goes away on any key,
  any click or its own deadline — which travels on the bridge, since nothing
  there wakes on a clock — its numbered rows open a place with a click as well
  as with `1`..`9`, the row of a listing carries the progress bar of the task
  working on that file, and the pane without the keyboard is dimmed.
- **Chrome that was lying.** `[ui] dir_indicator` drops the `/` in front of a
  directory when the icon column already says what the row is (`auto`, the
  default), the pane footer is painted in its own pane's border colour instead
  of always dimmed, and a function-key cell reads `2 Copiar` instead of
  `2Copiar` when it has room.

- **Navigation history, the whole feature** (ADR 0114). The history list
  starts with where you are ("here") and marks what `nav.forward` can still
  reach; inside it, `Del` removes an entry, `Shift+Del` clears the panel's
  history, `Alt+Enter` opens the entry in the other panel, `a` saves it as a
  bookmark and `/` filters the list. In the window, the mouse's side buttons
  are back and forward. New commands in
  both frontends: `pane.popular` (the directories you visit most, one list for
  the session), `pane.history-left` / `pane.history-right` (the history of one
  side), and a jump point per panel (`nav.set-jump-point`, `nav.jump-back`).
  `[ui] history_size` sets how many directories a panel keeps (5–64, default
  30); history, jump point and popular directories persist in the session.
- **Krusader preset: Alt+←/→ go back and forward**, as in Krusader itself.
  The preset had copied a stale docs table that called them bookmark menus;
  it now binds Ctrl+Alt+←/→ (history of the left/right panel), Ctrl+J (jump
  back) and Ctrl+Z (popular directories) from Krusader's source. orthodox
  adds mc's `Alt+Y` and `Alt+Shift+H`, vim adds `H`/`L`, and every preset
  says in its header why it binds what it does not.

## [0.3.0-alpha.4] - 2026-09-15

### Added

- **Your own themes by name.** A theme saved as
  `~/.config/norte/themes/<name>.toml` is offered by the theme picker, the
  first-run wizard and the settings screen in both frontends, previews live,
  and can be set as `[ui] theme = "<name>"`. The order is fixed: a bundled
  preset first — a stale `themes/nord.toml` cannot change what `nord`
  means — then your themes directory, then the value as a path. A file that
  does not parse is left out of the list rather than breaking the picker.
- **`norte theme import`** turns a Visual Studio Code colour theme into one
  of yours: `norte theme import OneDark-Pro.json --use` writes
  `themes/one-dark-pro.toml` and sets `[ui] theme`. It follows the theme's
  `include` chain, paints it over `vscode-dark` or `vscode-light` so what the
  theme leaves undefined is not monochrome, flattens translucent colours over
  the editor background, and refuses a name that a bundled preset would
  shadow. Comments and trailing commas in the JSON are fine;
  `tokenColors` is ignored. See `docs/theming.md`.
- **Seti file icons.** The `file-icons` extension has a fourth style,
  `seti`: the icons Visual Studio Code shows by default, one per language
  where `nerd` has one per class, so a Python file and a Go file no longer
  look alike. They come from the same Nerd font, so the window's bundled
  subset grew from 18 to 73 glyphs (16 KB) and a Nerd-patched terminal font
  already has them. Twenty more extensions are recognised in every style
  (`html`, `css`, `vue`, `svelte`, `dart`, `scala`, `tex`, `xml`…).

- **Alt on its own opens the menu bar.** In the window always (bridge 68):
  press and release Alt with nothing in between, as on any desktop; Alt+F4,
  Alt+Tab, an Alt-drag and AltGr do not count, and a dialog in front keeps
  the keyboard as it does for F9. In the terminal behind `[ui] alt_menu`,
  off by default: a lone modifier can only be reported under the kitty
  keyboard protocol (kitty, foot, WezTerm, Ghostty — not tmux, xterm or
  GNOME Terminal), and in that mode the terminal sends keys rather than
  text: a letter typed with a dead key (é) or a symbol typed with AltGr
  (@, #) arrives as its base key.
- **Column widths by dragging in the terminal too.** Drag the separator
  that opens a column in the header; the width follows the pointer and is
  written to `[ui.columns]` when you let go, the same key the window writes.
- **Scrolling the window no longer flickers.** Every update rebuilt the
  menu, panel and key bars, the tabs, the title, the column header and
  every visible row even when only the rows at the edge had changed; what
  paints the same now stays the same node. The overscan grew from 8 to 24
  rows, so a quick wheel gesture no longer shows blank rows at the edge
  while the new ones travel.

- **`vscode-dark` and `vscode-light` presets** (spec 2026-09-11),
  transcribed from Visual Studio Code's Dark Modern and Light Modern and
  from the editor's built-in colour registry — a VSCode theme JSON is not
  a complete palette, so `list.*` and `scrollbarSlider.*` come from the
  registry, not from any file in the `include` chain. Each preset's header
  records where every role came from and where it diverges: on white,
  VSCode's own `error` (3.35:1) and `warning` (3.12:1) fall under the
  4.5:1 that norte requires of a signal you have to read when something
  has gone wrong, so both are darkened and the original values are named.
- **The window paints a theme's file colours** (ADR 0108, bridge 66).
  `[files.kind]` and `[files.ext]` — half of what a theme file declares —
  had never reached the window: every entry came out the same colour,
  which reads as a broken theme rather than a plain one. An entry's
  colour is resolved by the host against the filename's raw bytes and
  travels in its row, because extensions are an open set and no CSS class
  could name them. Four of `Style`'s six attributes cross; `bg` and
  `reverse` stay behind so a theme cannot hide where the cursor is. The
  scrollbar, row hover, widget surfaces and control focus rings now come
  from the theme too, and the pane footer stops painting its text with a
  border colour.
- **Entry colours follow the desktop's colour scheme** (bridge 67). With
  `theme_light`/`theme_dark` set, flipping the desktop repainted the
  chrome from one variant and left the file names coloured by the other.
- **Ten chrome roles** — `hover`, `input-background`, `input-border`,
  `widget-background`, `widget-shadow`, `badge`, `scrollbar-slider`,
  `separator`, `focus-border` and `muted` — for the surfaces a modern
  editor separates by elevation rather than by borders. A preset is not
  required to define them (`Role::CORE` is what completeness asserts):
  the window's stylesheet derives each from a colour the theme already
  has, so the eight existing presets gain nothing and change nowhere.

- **The window, polished** (spec 2026-09-11). Bundled typography —
  JetBrains Mono for cells, Inter for chrome, 14 px on 22 px rows — with
  `[ui] font`/`mono_font`/`font_size` still in charge; column widths by
  dragging a header's edge, written to `[ui.columns] width` and honoured
  by both frontends, with the terminal's discard rule when the name would
  fall under ten cells; keycaps on the key bar, small-caps headers, an
  accent on the cursor, a mark checkbox on hover; breadcrumbs in the pane
  title, a toast for notices, pills for persistent warnings, a two-pixel
  gauge of the volume's usage in the footer; `[ui] theme_light` /
  `theme_dark` follow the desktop's colour scheme live, and a theme's
  `[effects] backdrop = "blur"` blurs what lies behind a dialog (the
  default preset asks for it). Bridge 64 and 65.
- **A `thumbnail` plugin kind** (ADR 0107), in its own WIT package
  `norte:thumbnail@0.1.0` so no installed guest needs a rebuild. A guest
  lists mimetypes like a previewer, gets the file's bytes (capped at 8 MiB)
  and the longest edge allowed, and answers a PNG/JPEG/WebP raster that
  the plugin-host verifies — magic, declared mimetype, dimensions, edge —
  before it crosses. `plugin.thumbnail` (protocol 0.73.0) carries it; the
  window's viewer asks for one when it has no picture of its own and paints
  it labelled «via ‹plugin›». `org.norte.image-thumb` is the first guest:
  a photo too big for the viewer's cap now gets a picture.
- **Plugins parametrised.** `file-icons` paints one-cell Nerd Font glyphs
  (`style = "nerd"`, bundled in the window as a 3.8 KB subset) and takes
  `dir-icon` and `unknown-icon`; two new columns plugins, `size-bar` (a
  `█░` bar per file; `scale`, `width`, `relative-to`) and `age` (a glyph
  per bucket and a short figure; `thresholds`, `glyphs`, `format`);
  `git-status` takes `glyphs` (letters or symbols) and `ignored`.
- **The chrome of an orthodox manager, derived and configurable** (ADR
  0106). A function-key bar on the last row of the terminal and a strip at
  the bottom of the window, read from the keymap of the screen that owns the
  keyboard; the panel bar names its buttons with the access letter
  underlined; every listing carries a footer with counts, what is marked and
  the free space of its volume; a dialog's key line is painted as clickable
  buttons; a status notice expires into the log after eight seconds and
  leaves a `!n` badge that opens it; the modified column prints local time
  with the precision the distance asks for (`smart`); the palette reads the
  human label first and keeps the last five commands on top; the focused
  cursor takes the theme's accent and the other pane's stays grey. Six
  `[ui]` keys (`key_bar`, `panel_bar_style`, `pane_footer`, `date_format`,
  `notice_seconds`, `dialog_buttons`), each a row on the settings screen and
  a section of the new `appearance` help page.
- **A first-start wizard.** With no `norte.toml` of your own, both frontends
  ask three things once — which file manager you have in your fingers, which
  theme (previewed live), and whether your terminal shows icons — and write
  the answers as configuration. Esc keeps the defaults and never asks again;
  `ntc --setup` asks again; `NORTE_NO_WIZARD=1` keeps it closed.

### Changed

- **F9 opens the menu** in six presets, as in mc, FAR, Norton Commander and
  Total Commander; the theme picker moves to `Alt+9`. Krusader keeps F9 as
  the terminal, as its source attests.
- **The mtime column is 12 cells** (was 10), to fit `09-10 14:02`.
- **A plugin may name a meaning, not a piece of chrome** (ADR 0108,
  amending ADR 0037). `role` on a span or a decoration is validated
  against `Role::REQUESTABLE` — what a piece of content MEANS — rather
  than against every `Role`. The window's chrome and its state
  (`selection`, `status-bar`, `mark`…) are no longer requestable: a badge
  in the cursor's colour would lie about where the cursor is. A
  non-requestable name degrades to `None`, exactly as an unknown one
  already did. No WIT bump; the twelve bundled plugins are unaffected.
- The window's bridge is version 67.

### Fixed

- **A `lua:` key in the window says it is not available.** Lua runs in the
  terminal frontend only (ADR 0110), but a `lua:` binding in your keymap
  layer was offered by the window's reference sheet, which-key and palette,
  and pressing it did nothing. It is now "not available here", like any
  other command the window does not have; `ntc` runs it as before.
- **A double click opens a directory in the window.** The renderer waited
  for the engine's own `dblclick`, the only door into a directory with the
  mouse; it now counts two presses on the same row itself, the way the
  terminal does. And the host no longer refuses an activation that names a
  pane other than the focused one: it focuses it first, so a double click
  on the pane next door works.
- **A column a plugin contributes is named by its manifest.** The `header`
  every manifest declares was parsed, carried over the wire and read by
  nobody, so the listing showed the column's id (`acme.git/status`). Both
  frontends now install the catalogue's label into the shared model, under
  the user's `[ui.columns] header` and above the id.
- **The terminal's extension manager answers the mouse.** It was the one
  overlay where a click did nothing: no row selected, and none of the
  buttons the window has. The detail pane now opens with a row of buttons —
  enable or disable, approve or revoke, settings, uninstall, and help when
  the extension ships a page — each firing exactly the command its key
  fires. Clicking a row selects it, clicking the selected row opens its
  settings as Enter does, and the wheel moves the cursor. Choosing another
  row with a plugin's settings open closes them, so the pane never shows one
  extension and the settings of another. The keys were always there and
  still work.

### Changed

- **The menu bar has ten groups, by what the reader wants to do.** File
  (what reads a file: view, edit, open, properties, size, copy the path,
  quit), Operate (what writes: copy, move, rename, batch and AI rename, new
  folder, delete, permissions, pack, unpack, test, split, combine,
  checksums), Mark, Go (parent, back, forward, history, hotlist, volumes,
  connect, disconnect, refresh, command line, terminal), Panels, Tabs, Find,
  View (what the listing shows and the side panes), Tools (extensions,
  agents, settings, profiles, the palette) and Help. Every built command is
  in exactly one menu — 32 of them were in none: permissions, packing,
  checksums, the batch rename, the hotlist, the history, the agents… were
  keyboard- and palette-only. In the terminal the bar tightens to one space
  between titles when ten do not fit in the width, instead of dropping the
  last one.
- **An upper-case letter under a modifier is labelled `Shift`.** `alt+C`
  printed `Alt+C` in the menus, the palette, the help and the reference
  sheet, and nothing said the case mattered — while `alt+c` is another
  command. It now prints `Alt+Shift+C` everywhere the chord is painted; the
  stored chord is unchanged. And `mark.files` moves from `alt+F` to `alt+f`
  in the orthodox, vim and cua presets, because the lower case was free: a
  Shift that buys nothing is a Shift the reader should not have to press.
  The imported presets keep their transcribed chords.
- **The terminal's extension manager has the window's detail pane.** Two
  columns when the terminal is 64 cells or wider: the list on the left,
  compact — name, version, ✓ or «not approved» — and the selected extension
  on the right: version · publisher · category, its state as two facts, its
  description, its capabilities as chips, how many commands and columns it
  brings and whether it ships help, then its settings table when opened with
  Enter — inside the pane now, with the cursor and the key's description —
  and the commands it contributes. Narrower terminals keep the single list
  with the settings in their own box. Same keys as before; the decisions
  behind the two managers were already the host's and the terminal's shared
  ones, only the screen differed.
- **A daemon a frontend started stops with its last client.** The window,
  `ntc --daemon` and a one-shot `norte --daemon …` start a daemon when none
  answers, and it used to outlive them by five minutes: a process nobody
  could see and nobody had asked for. They now start it with
  `--idle-timeout 2`, so two seconds after the last client disconnects — with
  no task running — it exits; a client that reconnects within that margin
  finds the same daemon, and a second client keeps it alive. `norte daemon
  run` by hand keeps its five minutes. The argv is built once, in the SDK
  (`daemon_run_argv`), instead of in four places.

- **A detached window says so quietly, and explains itself on demand.** A
  terminal that started while another window held the session greeted the
  reader with «another window owns the session; this one runs on its own» in
  the message bar — jargon to anyone opening norte for what they thought was
  the first time, and gone at the next key. Now nothing is announced: the
  persistent indicator in the status bar is the whole signal, shortened to
  `session not saved` (plain text: the badge now drives a mouse hit-test, and
  this frontend keeps its badges ASCII for exactly that reason), and it
  appears and disappears by itself as ownership
  changes (a daemon handover no longer produces a message each way either).
  Clicking the indicator opens the help on the panes page, which gains a
  section on the session: who keeps it, the three reasons a window may not be
  the one, and that no file is at risk. The window shows the same indicator in
  its status bar — it showed nothing at all before, so a detached window closed
  and lost every panel's place in silence (ADR 0077).
- **A terminal modal can have a hierarchy now** (ADR 0103)**, and the copy
  dialog is the first to use it.** A modal's body was ONE string painted as a flat
  paragraph, so the editable field, the paths, the hint and the keys all came
  out in the same colour and the same weight: the last thing you found was the
  only thing you could touch. Each line now declares its ROLE — data, label or
  hint, destination, field, warning, error — and the theme decides how it is
  painted. A modal that declares nothing looks exactly as it did, so the 26 of
  them migrate one at a time.
  In `TransferName`: the destination stands out and the source dims, the label
  sits ABOVE the field (it was below — you read the name and then found out
  what it was), and the field is painted as a field, its background running to
  the border. The source now shows the DIRECTORY instead of repeating the file
  name, which appeared twice in a five-line body. Labels «From»/«To» replace
  the arrow: `→` is legitimate inside a name and is not masked, so
  `docs → /home/BURN` manufactured a line that reads as two paths — that is
  the corpus's `arrow_join_spoof` fixture, and what the host already did in
  `DialogView::destination`. A test finally ties the declared height to the
  body that gets composed: the module's own rustdoc had warned from the start
  that the two halves drift apart and the modal gets clipped, and nothing
  checked it.
  Two more things reviews caught. **A rename now names the file it renames**:
  a rename opens with `to_dir = from.parent()`, so showing only the directory
  made «From» and «To» identical and the name being changed vanished from the
  screen the moment you typed — confirming a mutation whose operand is not
  visible, which is what ADR 0070 forbids. And **`ConfirmTransfer` loses its
  arrow too**: it marked its destination with `→` directly above a list of
  somebody else's file names, and dropping `⟨file⟩` made that line cheaper to
  forge — slash homoglyphs (U+2215, U+2044, U+FF0F) are legal on ext4, APFS
  and NTFS, so `→ ∕srv∕publico` is a legal file name that renders a complete
  destination line. What distinguishes it now is its ROLE, which a name cannot
  write. Both spoofs are in the corpus.
- **A modal's height is derived from its body**, and `modal_height` — a table
  of 131 lines of hand-written formulas, one per variant — is gone. This
  module's own rustdoc had warned from the start that the two halves drift
  apart and the modal gets clipped; when a test was finally written for one
  variant, it turned out the formulas did not even agree with each other:
  some added 2 to the line count, some 3, some 4, and `TrustHostKey` declared
  9 fixed rows for "five lines". Deriving it makes the drift impossible —
  there are no longer two numbers that can disagree. The one modal that lets
  `ratatui` wrap its body still declares its height by hand, because counting
  wrapped rows needs ratatui's own rule (`Paragraph::line_count` knows it, but
  it is an unstable feature and is not worth turning on for one modal); a test
  now checks that its message reaches the screen.
- **`tail_window` budgets in CELLS, not chars.** Fifty chars of CJK are a
  hundred cells, so a Japanese name overflowed its box anyway and the overflow
  ate the cursor at the end — you kept typing and the screen stopped changing.
  It affected all six free-text fields; the first snapshot of the transfer
  modal is what made it visible.
- **`⟨file⟩` stops announcing itself on local paths.** It is the default case
  — this machine, this disk — so its label distinguished nothing at all, and
  it was painted on every path of every listing, header and modal, spending
  eight columns exactly where room is scarce. What informs is the scheme that
  is NOT the usual one: `sftp`, `s3`, `zip` and friends still say so, and a
  `file` WITH an authority does too — that one is another machine.

### Added

- **File icons are a column left of the name, folders included** (ADR 0105,
  protocol 0.72.0, bridge 62, WIT `norte:plugin` 0.10.0). A decorator's
  manifest now says which slot it fills — `icon`, a fixed-width column left
  of the name, or `badge`, the git-status place right of it — and a row can
  carry one of each from two plugins, where before the first plugin silenced
  the second. The column opens for the whole listing the moment one row has
  an icon, so names stay aligned, and the terminal's header moves with it.
  `decorate` receives each entry's kind along with its name, so
  `file-icons` gives folders the folder icon whatever they are called, links
  the link icon, and files what their name says — spreadsheets and slides
  included. The package bump means every installed plugin is rebuilt
  (`just plugins force`) and re-approved in the manager; a guest built
  against 0.9.0 is listed as such with both versions.
- **An extension is uninstalled from the manager, on both frontends** (ADR
  0104, protocol 0.71.0). `plugin.uninstall` does what `norte plugin
  uninstall` did on disk — delete the directory, leave the state switched off
  and unapproved — and then what the CLI could not: the daemon forgets the
  plugin in its in-memory registry, so it stops being listed, and stops
  decorating listings, at once instead of at the next restart. Human
  connections only, like approving. The manager binds it to `dialog.remove`
  (`d` in the bundled presets; the imported four inherit the dialog block) and
  always asks first, naming the extension and saying the two things that go:
  its files, and its approval — a plugin installed later under the same id
  starts unapproved. Cancelling sends nothing.
- **The window's extension manager has a detail pane and buttons** (bridge
  61). Two panes: the installed extensions on the left — name, version, a
  state pill that says both facts, publisher and category, description,
  capabilities as chips — and the selected one on the right, with **Approve**
  or **Revoke**, **Enable** or **Disable**, **Help** when it ships a page, and
  **Uninstall**, followed by its settings sheet or a hint saying how to open
  it. A header counts what is installed and what is on. The buttons carry no
  logic: each sends the row and the change, and the host walks the same path
  the key does — approving opens the consent dialog that enumerates the
  capabilities, enabling an unapproved extension is refused with the same
  message (the button is disabled and its tooltip says why), uninstalling
  asks. `Help` closes the manager and opens the help at that extension's page,
  as `F1` over the row does in the terminal.
- **The window writes settings** (bridge 60). F11 in the window was a showcase:
  it listed the shared registry with each effective value and told you it did
  not write. Now `enter` (or a double click) does what it does in the terminal:
  a boolean, an enumeration, the theme or the keymap preset cycle to the next
  value and are written at once; a text or a number opens the window's
  one-field prompt, prefilled with the current value, and the shared editor
  validates it — a font size outside `[8, 32]` is refused with the range and
  writes nothing. The write goes to the layer the window already writes
  (the active profile, else the user's), off the actor, and the configuration
  is re-read and applied through the SAME path a profile switch uses: theme,
  keymap, columns, favourites and layout change without restarting; what that
  path cannot apply (language, fonts, reduced motion, and what is fixed when a
  pane is created) keeps its "restart required" badge, and the saved message
  says so. `SettingsView.read_only` is gone from the bridge: it was a phase-4
  promise and no longer true. Out of scope, and said in the parity
  classification: the terminal's search filter over the settings, and the
  plugins section, which is informational in both frontends.
- **The viewer scrolls sideways** (`viewer.left`, `viewer.right`, bound to
  `left`/`right` in all seven presets and to `h`/`l` in `vim`, both taking a
  count). The viewer does not wrap, so a minified HTML file, a wide CSV or a
  log had its right-hand half nowhere at all: painted clipped, unreachable.
  The cut is made once, in the shared model, on the already-rendered line, and
  it counts CELLS of terminal — by bytes the text jumps at the first accent,
  by characters any line with CJK misaligns against its neighbours. A wide
  character straddling the cut goes entirely **and leaves its cell blank** —
  dropping it without a filler slides that row one column against its
  neighbours, and the grid is the whole point. A zero-width mark that would
  open a row is dropped, as the truncators beside it already do with a tail: a
  split ZWJ cluster would otherwise paint a glyph that is not in the file. The
  stop is the longest line, measured when decoding, leaving one column always
  in view. The hex dump scrolls too, with its own 77-cell width: in a split
  pane its ASCII gutter did not fit, so refusing the axis made it unreachable.
  The status line and the window's header marks say the column in words, which
  in the docked viewer is the only thing that says the view is shifted.
- **The viewer says there is more, and the wheel moves it.** Both scrollbars
  in the terminal, drawn over the frame's borders and never when everything
  fits; the coupled preview gets only the vertical one, because its bottom
  border carries the line that says WHAT is being read. The wheel reached
  neither the full-screen viewer (it sits behind the overlay cutoff) nor the
  coupled one (its slot is not a listing, so the hit test returned nothing).
  Bridge 59 carries `total_cols`/`first_col` and the `viewer_scroll` action.
- **A hook may write a sidecar** (ADR 0101, protocol **0.70.0**,
  `norte:hook@0.2.0`). A hook plugin can now return `write-sidecar`: a file
  with one of the exact names its manifest declares in
  `fs-write = { sidecar = [...] }` — the only form `fs-write` takes; the old
  reserved `"scoped"` is rejected — written by the core **as a plugin actor**
  in the parent directory of the event, through the policy engine (a rule
  `actor = "plugin", action = "deny"` stops it, and the reader is told once
  with the new `plugin.notice` kind `effect-denied`; an `ask` rule on a
  plugin is a deny) and through the journal (`created`; `replace` trashes
  the previous file first, so its content has a way back; a directory with
  the name is never touched). Rows a plugin writes never come back to any
  hook as events. Approval shows `fs-write:<name>` badges. `org.norte.rename-log` now keeps a
  `.norte-renames.log` next to what it renamed, carrying the previous log
  forward. A 0.69 client ignores the new notice kind.
- **Operation hooks: a plugin can observe what the journal recorded, and
  say so** (ADR 0100, protocol **0.69.0**). A new plugin kind, `hook`, with
  its own WIT package `norte:hook@0.1.0`: the manifest names the journal
  events it listens to — `after-created`, `after-removed`, `after-trashed`,
  `after-renamed`, `after-mode-changed`, a closed vocabulary — and the guest
  receives the committed entries in batches and may return one kind of
  effect, `notify(text)`. There is no `before-*` and there is no veto: what
  decides whether a mutation happens is the policy engine, and a hook sees
  the entry after it is durable. The source is the journal's commit path,
  so a hook fires for a human's rename, an agent's, a batch and an undo
  alike, from the daemon and from an embedded `ntc`. The dispatcher runs off
  the critical path with a bounded queue, tells the guest how many events
  it lost, and three consecutive failures switch that plugin's hooks off and
  say so (disabling the plugin re-arms them). A hook may not declare `net`,
  is shown nothing under norte's own state directory, and its events appear
  at approval as `hook:<event>` badges. The sentence reaches both frontends
  as the new `plugin.notice` notification (humans only, `kind` ∈ {`notify`,
  `hooks-disabled`}), masked, capped, rate-limited and prefixed with the
  plugin id. Declaring a hook no longer rejects the manifest; an unknown
  event does. First hook: **`org.norte.rename-log`** (`plugins/rename-log`,
  `just plugin-rename-log`), which says how many files a rename touched. A
  0.68 client ignores the notification: the hook ran, its sentence reached
  nobody.
- **Every binary says which build it is.** `ntc --version`, `norte --version`
  and `ntc-gui --version` print the workspace version followed by the tree's
  `git describe` (`0.3.0-alpha.3 (v0.3.0-alpha.3-10-g674b0eb9-dirty)`); the
  TUI shows the same line in the frame of the help screen (F1) and the window
  carries it in its title. The revision is fixed at compile time by
  `norte-frontend`'s build script, falls back to `NORTE_REVISION` for
  packagers and to `unknown` without git. On a development machine `just link`
  points `ntc` at `target/debug`, and «0.3.0-alpha.3» was the same string ten
  commits after the tag: now the binary tells you.

### Fixed

- **Switching a decorator off in the manager left its icons and badges on
  the rows** until the next `cd`, on both frontends, and the reader concluded
  that switching off does not switch off. Any governance change — approve,
  revoke, enable, disable, uninstall — and any plugin setting written now
  make every open listing forget what the plugins said and ask again; a
  batch already in flight lands with an older generation, is dropped, and
  asked again. The window's rows go bare at once and fill back in; the
  terminal's event loop drains a flag the manager raises.
- **The terminal's initial listings carried no icons or badges until the
  first `cd`.** Both panes are built at startup outside the path a `cd`
  takes, and only that path asked the decorators; a freshly opened `ntc`
  showed bare rows and the reader concluded the plugin did not work. The
  event loop now requests decorations for the initial panes through the same
  function every `cd` uses.
- **`F10` and `q` close the window.** `app.quit` was classified as "not
  applicable to a window — the window manager closes it", which left the quit
  key of all seven presets, and the menu's own "Quit" entry, doing nothing.
  It now asks to close through the same path as the close button, with the
  same `[ui] confirm_quit` question. With it, no command the orthodox preset
  binds is left dimmed in the window's key sheet.
- **A menu's dropdown hung one title too far to the right.** Its left edge
  was a guess in cells — `12ch` per title — while the titles are painted with
  pixel padding and do not measure the same, so the error added up towards
  the right until "Help" opened under the next title. The renderer now
  measures the painted title and hangs the list from it; the cell count stays
  only as a fallback.
- **The dialog field lost the keyboard on every key.** Each key sends
  `dialog_input`, the host answers with a patch, the dialog box is rebuilt and
  the reused input is MOVED into the new box — and moving a node takes it out
  of the document for an instant, which drops its focus. Deleting a digit in
  "Font size" left the field unfocused and the next key went to the host as a
  chord. The renderer now gives the focus back once the new box is mounted.
- **The window's setting prompt was painted under the settings.** `enter` on
  a text or number row opened the one-field prompt, but `#dialogs` came
  before `#settings` in the document and this window has no `z-index`
  anywhere: the veil darkened and no field appeared, while the invisible
  dialog kept the keyboard. Dialogs now come after every selector and panel,
  before only the help and the fatal notice, and a test pins that order
  against `index.html`.
- **"restart required" was on sixteen of eighteen settings rows.** The window
  re-reads the whole configuration after a write, and almost everything is
  read at the moment it is used — the bars on every snapshot, the editor and
  the diff tool when launched, the search mode when searching, confirm-on-quit
  when quitting — so it changes at once. The badge now marks only what the
  host resolves once at startup (language, fonts, reduced motion) and what a
  pane fixes when it is created (hidden files, the `..` row).
- **`F10` and `q` quit from inside a side panel.** With the keyboard in the
  tree, the places sidebar, the processes panel or the log, `app.quit` was
  not in the panel's allowlist, so the panel swallowed it and only `Ctrl+C`
  got out. The consequence was worse than a dead key: the reader pressed
  `F10`, nothing happened, closed the terminal window believing the program
  had exited, and the `ntc` stayed alive holding the session lock — every
  `ntc` opened afterwards started detached and saved nothing, for as long as
  the ghost lived (a week, in the case that surfaced this). Quitting now goes
  through the same chrome funnel as the menu key, honouring
  `[ui] confirm_quit` exactly as it does from a listing.
- **The window remembers its panels.** Open the places sidebar, pick a layout
  template, split a pane, close the window, open it again: everything was back
  to the configured layout. The window never wrote its layout tree into the
  session and never read one from it — a comment justified that by citing
  ADR 0058 D5, which is about screen size not rewriting a stored tree, while
  D8 of the same ADR asks for exactly the opposite: close one frontend, open
  the other, carry on where you were. And the window only wrote the session
  at all on shutdown, and only when the close reached the host in time: the
  shared write policy was instantiated and never ticked. Now the window keeps
  its tree under the same session key as the terminal (`default`, or the
  active profile's name), applies the saved one at startup above `--layout`
  and the configuration — the terminal's order —, writes the session every
  second like the terminal when something changed, and writes it AT ONCE after
  any change of the tree: a panel toggled, a template chosen, a split, a
  resize. A detached window still writes nothing. The age seal of each slot is
  now remembered between writes, as in the terminal; stamping every capture
  with "now" would have made every tick a write.
- **The window's viewer header was invisible on a light status bar.** The
  theme projection carried `status-bg` and never its foreground, so anything
  painted on top had to GUESS the text colour — the viewer header guessed
  `title-fg`, which on a theme whose status bar is light is light on light:
  the path, the encoding, the EOL, the lossy mark and the column all
  disappeared. `Role::StatusBar` is a PAIR, and the terminal has always used
  it as one. `status-fg` now crosses with it, and a test pins the whole key
  list — it is an agreement with a stylesheet that shares no types, so
  dropping one has to turn red rather than be found by looking at the screen.
  Found by painting the window, not by a test.
- **And the scrollbar track was a solid band across the window**, for the same
  reason: it used `status-bg`. A track is chrome — what informs is the thumb —
  so it takes the dimmed border, the same pair the terminal uses.
- **An image in the window's viewer had lost its height, and grew bars that
  described a hex dump nobody could see.** One branch of the image painter
  rebuilt the whole frame where the other only replaced the body, so the new
  canvas was thrown away; it replaces the body in place now, like its twin.
  No bars over an image: the counts describe the hex view underneath, and the
  picture is scaled to fit — there is nothing to scroll.
- **A double click on a file does something again, in the terminal.** On a
  file `nav.enter` does not navigate: it resolves the desktop's program and
  leaves it armed for whoever owns the terminal to launch. The mouse arm ran
  the command and never launched what it armed, so double-clicking a `.jpg`
  did nothing at all and did not say why — while `Enter` on the same row
  worked. `on_mouse`'s own rustdoc had promised "launch the opener a double
  click resolved" since it was written. The fix is that all three mouse arms
  now leave through the same function, so none of them can forget the third
  thing again. The window was already right: its double click goes through
  the host, which opens a local file externally.
- **The tree follows the panel that navigates** (ADR 0102). Opening the tree
  and walking around left the panel pointing at the folder you were in when
  you opened it. Not a loose wire: both frontends anchored at open and never
  again, on purpose, because anchoring EMPTIES the tree and re-anchoring on
  every `cd` would have closed every open branch. What was missing was the
  ability to reveal without emptying — `Tree::follow` expands the ancestors
  and moves the cursor, and a sibling branch you opened stays open. It is
  wired into the one funnel each frontend already had for a `cd`, plus the
  focus change, and not into each gesture: that list went stale once already,
  which is why the funnels exist.
  The rule is about the ROOT: what has been read stays valid as long as the
  new root is an ancestor of the old one. So going UP a level (Backspace) and
  alternating between two sibling panels with `Tab` move the root up and keep
  every branch, where before each of them emptied the whole tree on every
  press — which made the tree useless with the two gestures it most needs to
  survive. Only another provider anchors and empties.
- **`Tab` reaches the third listing, and stops only at listings** (ADR 0102).
  In the terminal it was `focus ^= 1`, a count of two: after `alt+v` split a
  panel the key silently did nothing from the third one, because `PaneSlots`
  clamps out of range instead of panicking. In the window the opposite —
  `pane.switch` shared an arm with `layout.focus-next`, so with the places
  bar, the tree and the viewer open it took five keystrokes to get back to the
  listing beside you. Now they are two rings: `pane.switch` is "the other
  panel" and cycles the listings, `layout.focus-*` walks the whole screen.
  `Tab` still takes you out of a side panel.

## [0.3.0-alpha.3] - 2026-09-08

### Fixed

- **`[profile.start]` finally does something** (ADR 0098). Both frontends
  *wrote* it — `save_profile` records where every slot sits — and two files
  promised it is what makes a freshly saved profile useful. Neither frontend
  read it, so entering a profile left both panels where they were and the
  profile changed the colours and nothing else. Because nothing read it,
  nobody had noticed the two ends disagreed about the type either: the writer
  emits a `VPath` in wire form and the loader parsed a `std::path::PathBuf`,
  with a paragraph of rustdoc reasoning about `~` and relative paths that
  never occur. The values are wire-form `VPath`s now — a profile's slot may
  sit on sftp or inside a container — and the **session wins over them**:
  `[profile.start]` is where a slot opens the first time, not every time.
- **A profile's unparseable lines are now said out loud.** `profile_warnings`
  was computed since profiles existed and displayed by nobody. That is the
  other half of the same defect: being strict about `[profile.start]` without
  it would be a trap — you write `/tmp`, the slot opens wherever it likes, and
  nothing tells you. The count goes to the status bar and each reason to the
  log, in both frontends, at startup and on every profile switch.
- **`[ui] font`, `mono_font`, `font_size` and `reduce_motion` were dead in
  both frontends** — loaded, validated, offered in the settings screen with
  "applies live", and read by nobody. Deleting them was the wrong answer:
  `reduce_motion` is an accessibility commitment from spec §17, and the other
  three land directly on a webview. They now travel in the window's startup
  catalogue, the same package the theme rides in, and the renderer plugs them
  in as CSS variables. Two things that were not obvious: the size also moves
  the **grid** — this window is laid out in cells, so a bigger letter inside a
  row of the same height overflows it — and the cell *width* is measured with
  the font in place, because a monospace advance is not a fixed fraction of
  its size and a wrong width skews every column. `reduce_motion` can only
  **add** the request: `false` does not switch off the desktop's own
  `prefers-reduced-motion`. A terminal applies none of the four and says so.
- **A `[profile.start]` naming a slot no layout places is now said out loud.**
  It used to fall through in silence — the same shape of defect as the key
  itself before ADR 0098: you write something in the profile file and nothing
  happens, with nothing explaining why.
- **Copying a single file now warns about the destination in the terminal
  too** (#343). The terminal split by item count: several items opened the
  confirmation dialog and armed the destination check, one item opened the
  *name* dialog and armed nothing. So copying a lone file said neither "it
  does not fit" nor "this destination cannot confine its writes", while the
  window said both. The second one is the one that matters: its **absence
  means** the destination does hold its writes, so staying quiet asserts
  something nobody checked — and since #219 that reaches a single leaf too.
- **The agent approval dialog says the same thing on both surfaces.** The
  deadline was shown only by the window — a decision with an expiry that does
  not show it reads like one that waits forever, and whoever comes back later
  hits approve on something the daemon already denied. The badge for a hostile
  path *outside* the shown window was only in the terminal: it cannot point at
  a specific path (that one is not on screen) but it can say something out
  there would be painted altered, which is what decides whether to expand
  before approving. Both now cross the bridge (58), and the rule for when a
  redacted path counts as hostile moved to `norte-frontend` — the two surfaces
  were answering it with different functions over the same paths.
- **A profile's `[ui] theme` can be a path in the window too** (ADR 0020). It
  worked at startup and nowhere else: both places the window resolves a theme
  — the host, which keeps it for its own theme screen, and the process that
  turns it into CSS variables — looked only at bundled presets, so a profile
  carrying `theme = "…/mio.toml"` silently kept the old colours while the
  terminal applied the new ones. Resolving reads a file, and neither the
  host's actor nor the native-effect pump may read where they run, so the
  split is now `norte_frontend::theme::is_preset` — shared — and the path
  branch goes to a blocking thread and comes back through the mailbox, the way
  saving a theme already did. A file that cannot be read does not leave the
  window colourless: the previous theme stays and the status bar says which of
  the two things went wrong.
- **Two schema comments claimed the window ignores keys it reads.**
  `[ui] menu_bar` and `[ui] panel_bar` both reach the window (`MenuView.bar`,
  `PanelBarView.bar`). A comment that lies is what the next audit believes.
- **The processes panel highlighted one task and cancelled another.** A
  finished task leaves the board on its own after ten seconds, which shifts
  every row below it. The window speaks in patches, and the panel's cursor was
  not in any of them — it travelled only in a full snapshot, so the renderer
  kept the highlight on row *N* (by then a different task, or none) while the
  host clamped and cancelled something else. The cursor now rides inside
  `ViewChange::Tasks` (bridge 57), for the same reason `total_rows` rides
  inside the row patch: it is the extent of what travels beside it, and the two
  move together. The bridge does not tolerate version skew — a mismatch is a
  fatal screen, not a half-parse — so the field always travels, and `null`
  means "no row selected", which is what an empty board says.
- **Choosing a task by position let the selection slide onto another one.**
  Both frontends stored *the row number* and clamped it when reading. That
  survives a shrinking board, but not a row leaving from **above**: row 1
  quietly became a different task while the reader watched, and the cancel key
  stopped a copy nobody chose. `Processes` now remembers the selected task's
  **id** and falls back to the remembered position only once that task is gone
  — the same distinction the listing draws between a `RowKey` and an index.
- **The rule underneath was written twice.** How the processes cursor survives
  a moving board lived in `norte-tui` and by hand in five places in the window.
  It is now `norte_frontend::processes::Processes`, shared (ADR 0077). The two
  copies had already started to drift: `up()` subtracted from the *stored*
  index, so once the board had shrunk, pressing Up moved nothing visible and
  the key looked dead.

- **The details sheet and the docked viewer were empty on the `..` row.**
  Both asked `PaneState::selected()`, which answers `None` on the parent row
  on purpose — that row is not an operand, and this is what keeps F8 from
  deleting the parent. But those panels do not operate, they *describe*, and
  the cursor is born on `..`: the window's "Detalles" therefore read "nada
  bajo el cursor" at every start and after every `cd`, which is how a working
  panel looks broken. A second question now has a second answer:
  `PaneState::cursor_entry()` returns the row under the cursor, `..`
  included, and is documented as never an operand. On `..` the sheet says
  `..`, `folder`, and where it leads (new `metadata-target` key in both
  locales), and the viewer says `directory`.
- **`Enter` on an archive or a symlink navigates in the window too.** The
  terminal browsed into `zip+file://…/!/` and followed a link; the window
  looked at `kind != Dir` and handed both to `xdg-open` — while its own
  comment, three lines below, claimed to be making "the same decision as the
  TUI" (ADR 0077), which is exactly the false claim that ADR exists to
  prevent. The window already knew `archive_root_for`: it uses it to unpack
  and to test a container, just not to open one. The decision now lives once,
  in `norte_frontend::nav::enter_target`.
- **The panel bar now follows the screen.** Open panels come in the order they
  are laid out — top to bottom, and left to right within a row — and closed
  ones trail in registry order, because they have no position and inventing
  one would say where something is that is nowhere. Before, the row was the
  registry's order and you had to translate between two lists every time you
  looked. The *letter* is deliberately resolved before the sort: it de-dupes
  against the letters already handed out, so if it depended on the order,
  opening one panel could change another's letter and the row would stop being
  learnable.
- **The panel bar read backwards.** A *closed* panel was styled with
  `Role::StatusBar` — which in half the presets is a live background with dark
  text — while the bar itself is cleared with the base background. So closed
  buttons came out as lit blocks and open ones as plain text: the visual
  weight inverted, and looking at the bar answered the opposite of what you
  were asking. Closed is now the bar's own text, dimmed. The state was always
  derived correctly from the resolved layout on every frame; what was wrong
  was which state got the loud style.
- **You could not resize panes in the window.** The drag handles are
  `position: absolute` and were inserted *before* the panes, which are
  absolute too — and this stylesheet uses no `z-index` anywhere on purpose, so
  stacking follows document order. Every pane covered the handles and the
  `pointerdown` never reached them. They are built last now. A second bug in
  the same feature: a horizontal drag sent `clientY` straight through, without
  subtracting the menu and panel bars that `#screen` is pushed down by, so the
  border jumped by the height of the chrome the moment you grabbed it. The X
  axis matched by coincidence — the board starts at column 0 — which is why
  only horizontal borders looked broken.
- **The window painted in the process's language, not its own.** `norte-ui-host`
  itself was clean — all 131 of its calls pass `self.lang` — and every leak was
  a *shared* helper translating through the global. The worst was the date:
  every cell of the listing came out in the process's language under a header
  in the host's, and configuration could not dodge it because the window
  ignores `time-format`, so the relative branch is always live. Four more:
  the settings screen (section titles in one language, each option's name and
  description in the other), the status bar's "this build does not do that"
  sentence, the palette's `[Extension]`/`[Renamer]` prefixes — which is what
  breaks the disguise of a plugin titling itself like a built-in, so it cannot
  read as part of the title — and the `Folder`/`Yes` cells. Each helper gained
  an `_in(lang)` variant with the ambient one delegating, which is the pattern
  `header_label` already used in that crate.
- **The collision dialog dropped the badge on an altered name, on the one
  screen where overwriting a file is approved.** It masked over
  `display_lossy()`, which had already put the U+FFFD in — so `display_name`
  received impeccable UTF-8 and declared the name *faithful*. It also ignored
  `pane.names-encoding`: in a cp866 pane the terminal asks about `Папка` and
  the window asked about `??????`. Approving a name that is not the one you
  have been looking at is not approving. The encoding is now captured **when
  the operation is launched**, not read when the answer arrives: a collision
  turns up asynchronously, on top of whatever the reader is doing, and between
  the send and the question there is room to change slots — which is why the
  terminal has carried it in its `RetrySpec` since #98.
- **With no trash, the window deleted permanently without saying so.** The
  warning was the terminal's alone; the window offered a destructive button,
  which says that answer deletes, not that there is no way back. It now reads
  the slot's cached capabilities — and with **three** states, not two. "Not
  known yet" is not "there is no trash": capabilities arrive behind the
  listing and on their own, so there is a window (and, if the request fails,
  a whole session) in which nothing is known, and turning that into "no trash"
  really deleted in a place that has one. Only an explicit *no* makes the
  delete permanent; guessing trash where there is none costs an `Unsupported`
  and a `shift+F8`, and guessing the other way costs the bytes.
- **An AI rename plan can no longer be approved unread.** The window already
  required reaching the end and the terminal did not, so a plan of two hundred
  renames could be signed having seen the first ten — and the ones that matter
  can be on row a hundred and eighty. The strict rule was the right one, so
  the terminal moved: one shared `approval_ready`, over a *high-water mark*,
  because scrolling back up does not un-read what was read.
- **A slow listing gave the window no signal at all** (#323): `aria-busy` and
  not one CSS rule painting it. A waiting slot now says what it is doing and
  where it is going — the verb from the closed vocabulary the two frontends
  share, so a remote connect says "connecting", not "loading". The 250 ms
  threshold travels from `busy::THRESHOLD` instead of being a number in a
  stylesheet. There is deliberately **no "Esc cancels"**: nothing in the
  window aborts an in-flight listing, and the repo has that doctrine written
  three times — never a false affordance.
- **The log panel spoke three vocabularies at once** — `TRACE`, `trace` and
  «traza», all on screen together. The wire id is an identity that gets
  compared and the label is what gets read; both travel now. `TRACE` stays
  untranslated on purpose: it is what you write in `RUST_LOG` and what you
  scan for in a long list. The level buttons stay translated — they are a
  control, and the terminal has none to disagree with.
- **A volume row was written three times, two of them in the same crate**, and
  all three said "unknown" when the only thing missing was the *total* —
  throwing away the one number there was. How much is left is the half you
  look at before copying.
- **An empty AI instruction ate the dialog.** The terminal leaves the modal
  open with the error underneath; the window put the message in the status bar
  over a screen with nowhere left to type. Its twin, the semantic query, had
  been fixed carefully three files away.
- **A search that FAILED read as one that finished with no hits.** The host
  marked every terminal state as "no longer running" and painted
  `search-status-done`, so a search that broke on the second directory and one
  that walked the whole tree said the same thing: "0 hits". That is not an
  imprecision in the UI — it is a false claim about the disk, and whoever
  reads it stops looking. The outcome is now a four-state value and the
  sentence comes from the terminal's own family, failure and cancellation
  included. A search that never got queued at all is fixed on the way: with no
  task there is no progress to carry the outcome, so the view sat on
  "searching…" forever while the error went past in the status bar.
- **Everything that says a listing is not what it looks like now lives in one
  place** (`norte_frontend::notes`): entries the provider skipped, names being
  reinterpreted, marks a refresh dropped, what is marked, a listing still
  filling, what hiding puts aside. Each frontend wrote its own wording and
  they had already drifted — the window used a key of its own ("N entries were
  skipped", without the ⚠ that makes it read as a warning) and painted it
  **also when N was zero**, announcing an incomplete listing that was
  complete and spending the one signal there is for when something really is
  missing. The window's header gains the four notes it never had (bridge 55),
  in the terminal's order: the warnings before the counter, because a
  truncated warning stops warning while a truncated counter only stops
  counting.
- **The destination badge is back to meaning something.** With two panes the
  destination is "the other one" and the window marked it anyway; a mark that
  shows up always stops being read, and then it is not there with three panes,
  where a copy toward whichever slot the engine breaks the tie on is silent
  data loss (ADR 0058 D7). The count is now one shared decision, applied by
  the renderer — the DTO's role stays the model, and telling the host to lie
  about it broke three tests that read it as one.
- **The window's copy dialog never said "this will not fit" or "this
  destination cannot confine writes".** Both lines are the terminal's since
  #149 and #164 and the window had neither: you found out from a failed task,
  or you did not find out. The dialog now opens without them and a task fills
  them in — the same split the terminal makes — and the two fail differently
  on purpose: not being able to enumerate volumes says nothing (silence is
  the honest answer), while not being able to read the destination's
  capabilities *warns*, because there the silence would mean "this place holds
  its writes down" and swallowing the failure would assert it without knowing.
  That same reasoning is why the dialog says **"checking the destination…"**
  while it waits (bridge 54, `DestCheckView` — three states, not a list of
  warnings): with only an empty list, "I have not asked yet" and "I asked and
  there is nothing to say" reach the renderer identically, and the human can
  confirm in that gap. The files-dropped dialog asks too, and it is the path
  that can least afford not to: its operand list is composed by another
  process. The all-or-nothing rule for the total moved to
  `norte_frontend::space::total_to_write`: the two sentences were already
  shared and only the arithmetic behind them was private to the terminal,
  which is how a warning ends up appearing in one frontend and not the other.
- **The window's help knew nothing about a location that refuses writes.**
  `source_read_only` and `dest_read_only` were wired to `false`, with a
  comment declaring that the host does not keep that count. It does keep it —
  since #268 it asks each slot for `capabilities` when a listing lands — and
  it threw everything away but the fold mode, with the `READ_ONLY` flag one
  field away. Inside a container the terminal dimmed F5/F8 and the window
  offered them lit: help that invites writes the backend will refuse. The
  slot now keeps the whole `Capabilities`, `source_read_only` asks the active
  slot and `dest_read_only` the one holding the target role, and the flag
  falls back to the scheme the way `App::pane_read_only` does — through one
  shared `availability::read_only`, since two spellings of that two-step
  answer is exactly the shape ADR 0077 exists to stop. The startup listing
  never asked at all, so the first directory of every slot was uncharted
  until the reader navigated somewhere else — the fold check of #268 was
  quietly missing there too. And the answer is now tied to the path it was
  asked about instead of being dropped whenever another one is requested: a
  landing re-freezes the help's facts three lines after asking, so every
  re-listing under an open help read "not known" and lit the row back up.
- **`enterable` said "directory" in the two places that navigate more.**
  The terminal open-coded the same list `nav::enter_target` decides and the
  window asked `kind == Dir`, so with the cursor on a `.zip` the archives
  page — whose first sentence is "Enter on a compressed archive enters it" —
  offered that very row greyed out. The terminal also asked through
  `selected()`, which answers `None` on the `..` row on purpose, so the help
  dimmed `Enter` exactly where the cursor is born after every `cd`; it now
  asks `nav_enter_target`, which is the predicate that actually runs.
- **`[ui] confirm_quit` asks in the window too.** Closing it never asked: the
  `CloseRequested` handler dumped the session and closed. With
  `confirm_quit = "always"` the terminal guards F10 and the window walked away
  from a half-finished copy without a word — and `always` is precisely the
  value that asks for the guard. The three-way decision is the shared one
  (`settings::quit_needs_confirm`), whose rustdoc already named a
  `confirm_quit_should_open` on the window side that did not exist; what each
  frontend computes for itself is what counts as pending work, and here it is
  a live task on the board. The dialog says how many.
- **`[ui] quick_search` picks the mode in the window too.** The host started
  the incremental search hard-wired to `filter`, so `quick_search = "jump"`
  moved the cursor in `ntc` and narrowed the listing in the window — one key,
  two behaviours. The DTO already knew how to report both modes; what was
  missing was reading the key.
- **`[ui] theme` accepts a path to a `.toml` in the window (ADR 0020).** It
  called `Theme::preset` alone, so a theme of your own themed the terminal and
  left the window on the default palette without saying anything — the same
  shape as the `--layout` bug. Startup now goes through the shared resolver;
  a profile switch still applies presets only, and that is written down where
  it happens.
- **`NORTE_LANG` beats `[ui] lang` in the window, as in the terminal.** Both
  surfaces documented *opposite* rules and both obeyed their own, so with
  `NORTE_LANG=en` and `lang = "es"` set, `ntc` came up in English and
  `norte-gui` in Spanish. The terminal's rule wins: `NORTE_LANG` is
  norte-specific and set for one run, the same class of thing as `--layout`,
  which beats `[ui] layout`.
- **`[ui.columns]` styles the window's columns too (#108).** The window asked
  for `ColumnStyle::default_for_id` — the factory style — in both the header
  and the cells, so only the column list and its order survived from the
  config: a custom `header`, `format`, `align` and `width` were dead here
  while the terminal honoured them. As a side effect this unblocks the
  configurable half of a separate debt: the date cell is translated with the
  process locale, and until now you could not even sidestep it with
  `format = "iso"`, because the key did nothing.
- **`norte-gui <DIR>` no longer loses to the saved session.** The directory
  typed on the command line was overwritten by `aplicar_sesion`, which writes
  the location of every slot, so the window opened where you were yesterday
  and ate the argument without saying anything. The terminal closed the same
  hole in `eb237c61` with `pin_start_dir`; the window never got it. It wins in
  the active panel only — the other stays where the session left it, which is
  half a screen of memory nobody asked to throw away.
- **`openers.toml` now applies in the window too (#28).** The table was read
  only by the terminal, so a rule saying "PDFs open in zathura" held in `ntc`
  and was skipped in `norte-gui`, which handed everything to the desktop
  handler — a whole documented feature honoured by one surface. The desktop
  handler stays as the last resort: writing configuration cannot be a
  requirement for opening a PDF.
- **`[ui] editor` now applies in the window (F4).** `pane.edit` was mapped to
  `pane.open` unconditionally. The deliberate half of that — not launching
  `$EDITOR`, a terminal editor in a window that has no terminal (#290) —
  stands. Ignoring `[ui] editor` was not deliberate: it names an explicit
  program that can perfectly well be graphical, and its sibling key
  `[ui] diff` was already honoured here by the same machinery.
- **The window's panel title did not follow `pane.names-encoding`.** The
  rows were re-transcoded and the header kept the old reading — the mojibake
  stayed at the top and the reader could not tell whether the command had
  done anything, which is exactly the half-fix #57 and #293 rule out. The
  header travelled only in a full snapshot; it now travels with the rows, in
  a `browser_header` change that also carries the "N skipped" and "N hidden"
  notes (stale after `pane.toggle-hidden` for the same reason) and the mark
  count (not painted yet, and wrong on every marking gesture — a trap for
  whoever paints it).
- **The window could not scroll past row 100 in a large directory.** A
  listing arrives as a first page of 100 and then drains in batches of 500,
  and every batch — the last one included — answers with a rows patch. But
  `total_rows` travelled only in a full snapshot, and it is what the renderer
  sizes its scroll canvas from (`total × cell height`, plus `aria-rowcount`).
  So a directory of 5 000 files stayed capped at 100 rows for the wheel, with
  no way to ask for the rest, because the visible range is computed from the
  scroll position. The total now travels with the rows. The existing drain
  test missed it because it dispatches `Resync` on every loop — which is
  exactly what the real renderer does not do.
- **`Enter` on `..` now lands the cursor where you came from.** It went up in
  both frontends but left the cursor on the first row, while the dedicated
  "go up" command put it on the directory you had just left — the same
  navigation, two results, depending on which door you used. The terminal
  says so with its own verb (`EnterAction::Up`) rather than a plain `Cd`,
  because only the caller that knows it is *going up* can set the landing
  hint.
- **A truncated attribute value is marked.** `sanitize_cell` cut at 32
  characters and appended nothing, so in the details sheet — which exists to
  show the whole value — a cut value and a complete one painted identically.
- **The window's column headers follow the window's locale.** The details
  sheet took an explicit language while `header_label` read the process
  global, so `Name`/`Kind` could come out in one language and `Mode`/`Owner`
  in another, in the same panel. `header_label_in` takes the locale; the
  terminal keeps the global one, where the two always agree.
- **A session slot the current layout does not place is kept, not
  re-adopted.** The "does this layout have the slot?" question was put to the
  pane store, which still holds orphans, so it answered yes — and the slot
  went through the adoption door instead of being written back untouched.
  The saved state for that panel was lost outright, so going back to
  yesterday's layout did not return the panel where it was.
- **Clicking a row did not move the window's details sheet.** The sheet only
  ever travelled inside a *full* snapshot, and a click answers with a rows
  patch — so it rode along on the snapshot the docked *viewer* produced when
  its note changed, and a layout with a details panel and no viewer left the
  sheet frozen on whatever it showed at startup. It now has a probe of its
  own, `sondear_hojas`, run after every message exactly where the viewer's
  already was. The terminal never had this: it recomputes the sheet each
  frame.
- **The details panel says which listing it follows.** "Detalles" alone does
  not say what the details are *of*, and with two listings open the only way
  to find out was to move the cursor and watch. The title now carries the
  followed pane's path in both frontends (`follows_display` over the bridge),
  truncated with an ellipsis rather than clipped in silence.
- **Opening the quick search could make the `..` row an operand.** In
  `Mode::Filter` the real cursor does not move and the filter chooses the
  row, and a filter's empty query is born selecting index 0 — so the guard
  in `PaneState::selected()`, which tested `self.cursor`, was bypassed and
  the parent directory came back as "what is selected". `marked_paths()`
  falls back to `selected()` when nothing is marked, and F8 takes the first
  of that list: two keystrokes from a fresh pane, the delete confirmation
  named the parent directory. The guard now tests the row the screen is
  actually pointing at, so it covers both sources. Found by review; it
  predates this branch.
- **The TUI silently turned `[ui] parent_entry` off after the first saved
  session.** `App::apply_session` and `session_push::restore_slots` both
  replaced the pane wholesale and put back only `sort` and `show_hidden`, so
  the `..` row was lost on every restore — the same configuration produced a
  window with the row and a terminal without it. Both now go through one
  door, `App::adoptar_pane`, which stamps the session's configuration on any
  listing born outside `App::nuevo_pane`.
- **`norte-gui --layout <name>` could not open a user layout.** The window
  looked only at the factory presets while `ntc` tried
  `layouts/<name>.toml` first — and the same window offers those files in
  its own layout picker. The rule now lives once, in
  `norte_frontend::layout::config::or_preset` (user file wins; a missing one
  falls back silently, a broken one falls back with a warning), and the
  window keeps the name's bytes instead of rejecting a non-UTF-8 filename
  (#246). A *broken* user layout now reports its parse error instead of
  "value does not exist", and the warning reaches the status bar rather than
  only the log. The TUI stopped filtering the name `orthodox` before
  loading, which had just become a divergence: a user's
  `layouts/orthodox.toml` was honoured by the window and ignored by the
  terminal.
- **A layout name in an error message is masked and marked.** It reaches
  `norte-gui.log` and the webview, and `valid_profile_name` does not reject
  control characters, so `--layout $'a\x1b[31mb'` put a live escape sequence
  in the log; `$'\xff'` and `$'\xfe'` produced the same message with nothing
  saying the text was not the bytes. The terminal already did this; the
  window did not.
- **The window's panel bar did nothing on click.** The renderer sent
  `panelbar_activate`; the host's wire name is `panel_bar_activate`
  (`UiAction` is `snake_case`, and `PanelBar` is two words). The action
  failed to deserialize at the Tauri boundary and the click died in the
  console. `gui-ci` was green because the renderer's test checked what the
  renderer said it sent, not what the host understands. A new test in
  `norte-gui-tauri` reads every `action:` literal in `types.ts` and
  requires it to be a wire name in ui-host's `actions.json` golden, so the
  next hand-typed name turns the gate red instead of a button inert.
- **An order the host rejects at the boundary now shows in the status
  bar.** The renderer used to log "el host no aceptó la acción" to the
  webview console and nothing else, which is how the panel bar stayed
  dead unnoticed. Now the status bar says "the window sent an order the
  core does not understand: `<action>`" until the next accepted order; the
  error detail still goes to the console.

### Changed

- **The details sheet's field list is shared.** It was written twice —
  `norte-tui/src/ui/panels.rs` and `norte-ui-host/src/controller/places.rs`
  — and the copies had already diverged: the window marked a hostile
  attribute value and the terminal did not, while the equivalent *column*
  marked it in both. `norte_frontend::metadata::sheet` decides the rows now
  and the two frontends only paint them.
- **The parity harness runs every scenario twice**, with the `..` row off
  and on. It only ever ran with the row off, which is the one state nobody
  starts in.
- **Protocol 0.68.0: a renamer that refuses says why (#332).**
  `AiRenamePlanResult` gains `refused: Option<String>`, omitted when
  absent, so every plan that existed is byte-identical to 0.67. When a
  `renamer` plugin returns `Err(text)`, `plugin.rename_plan` now answers an
  empty plan carrying the sentence instead of a bare `Unsupported`; the
  core masks terminal hazards and caps it at 200 characters (third-party
  text), and both the TUI and the window show it in the status bar as
  "the extension proposes nothing: …" without opening a review. A 0.67
  client ignores the field and says "the model proposed no changes" —
  imprecise, not broken. Source break for anyone building the result
  literal: one more field.
- **Protocol 0.67.0, `norte:renamer@0.1.0`: a plugin can propose a batch
  rename.** A sixth plugin kind, `renamer` (ADR 0095): the plugin gets the
  marked names (and, with `location = "read"`, a token to `stat` or read
  them) and returns `{ current, proposed }` pairs. It never renames: the
  plan goes through the review the AI plan already has, in both frontends,
  and from there through `fs.rename_batch` with the core checking every
  target, the journal and undo. The package is its own
  (`wit/deps/renamer/renamer.wit`, world `norte-renamer`) so adding it did
  not move `norte:plugin` and no installed previewer needs a rebuild. On the
  wire, `plugin.rename_plan` takes `{ plugin_id, renamer_id, dir, names }`
  and answers the existing `AiRenamePlanResult`; a renamer appears in
  `PluginInfo.commands` with the new `PluginCommandInfo.kind = "renamer"`,
  omitted when `command`, so a 0.66 client lists it as a command it cannot
  run and a 0.67 client shows it in the palette under `[rename]`. The
  manifest gains `[[contributions.renamer]] id, title` (digest tag 6). The
  host caps a plan at 10 000 proposals and the usual byte budget; the core
  drops identity pairs and names it did not ask about. The demo is
  **`org.norte.date-prefix`** (`plugins/date-prefix`, `just
  plugin-date-prefix`): `YYYY-MM-DD_name` from each file's modification
  time, leaving already-dated names alone, and refusing with a sentence
  instead of guessing when it has no location. The gate installs it and
  runs a plan over real files. Two things the protocol review added:
  `plugin.rename_plan` caps `names` like `ai.rename_plan`, and
  `plugin.run_command` on a plugin whose kind does not export `command`
  (decorator, columns, provider, renamer) answers `INVALID_PARAMS` before
  instantiating anything, where it used to burn a wasm instantiation and
  answer "internal error". The guest's refusal sentence stays in the
  daemon log for now (#332).
- **Protocol 0.66.0, `norte:plugin` 0.8.0 → 0.9.0, bridge 50: a span has a
  background and the viewer says how wide it is.** `SpanWire` gains
  `bg: [r, g, b]` and `plugin.preview_styled` takes `columns`, both optional
  and both omitted when absent, so the wire a 0.65 peer sends and reads did
  not move: a 0.65 daemon ignores the width and never sends a background; a
  0.65 client drops the background and paints the foreground it always did.
  The same two fields land on `span` and `preview-input` in the WIT, which
  is why the package moves — **plugins built against `norte:plugin@0.8.0`
  need a rebuild** (`just plugins force` rebuilds the official ones; until
  then the catalogue lists them as broken). The TUI paints `bg` as an RGB
  background and sends the terminal's width; the window paints it as the
  span's `background-color` and sends its viewport's. It is what an image
  previewer needs to draw two pixels per cell with `▀` and to shrink a
  picture to the viewer (ADR 0037 amendment). Source breaks for anyone
  building on the crates: the SDK's `RemoteBackend::plugin_preview_styled`
  and `norte-ui-host`'s `HostBackend::plugin_preview_styled` take the width
  as a second argument, and `norte_frontend::ansi::StyledSpan` and
  `norte_ui_host::dto::SpanView` gain a public `bg` field. The daemon
  clamps a requested width to 1024 cells, and the per-line span cap counts
  each span's colours towards the 4 MiB total, not only its text.
- **`norte:location` 0.1.0 → 0.2.0: `read-prefix`.** `read` reads a file
  whole and charges it whole against the page's budget, so a column that
  only needs a header — the dimensions of a PNG, the bitrate of an MP3 —
  spent the session's 64 MiB in a dozen rows and never got a cell for a
  video. `read-prefix(token, rel, max)` returns and charges at most `max`
  bytes, capped by the host's per-read bound. This is the first WIT bump
  since ADR 0094 wrote down what a bump does: **plugins built against
  `norte:location@0.1.0` need a rebuild** — the catalogue lists them as
  broken with both versions until then (`just plugins force` rebuilds the
  official ones). `norte:plugin` stays at 0.8.0: none of its interfaces
  changes name, only the `norte-columns` world's import, so a previewer
  built yesterday still loads (ADR 0094 amendment).

### Added

- **A guard that the golden corpus cannot change shape without a bridge bump.**
  The contract test already caught the Rust and TypeScript version constants
  drifting apart; nothing caught re-blessing the corpus with neither of them
  moving. On this bridge every shape change is breaking — the version is
  compared for exact equality and a renderer of another one gets a fatal
  screen — so a field added, renamed or removed without a bump is a stale
  renderer reading `undefined` in silence. The guard summarises the *shape*
  (the set of key paths), not the values, so a different example filename
  costs nothing and a new field stops the build.
- **A test that every panel following the cursor can reach the renderer on its
  own** (`crates/norte-ui-host/tests/sondas.rs`). It opens each panel the bar
  offers, moves the listing cursor, and checks whether that panel's view
  changed; if it did, the change must have arrived unprompted. The docked
  viewer (#291) and the details sheet both shipped frozen once, riding along in
  whatever full snapshot another panel happened to trigger — this is the guard
  for the third one. A new panel is covered without touching the file, since
  the enumeration comes from the panel bar.
- **The parity harness can now enter an archive and a symlink.** Its own header
  named the gap: the test tree held only directories and files, so the
  scenarios could not touch the inventory's number-one divergence. The
  primitives leg also carried the *window's* rule written out by hand, so that
  comparison could not fail; all three legs now ask
  `norte_frontend::nav::enter_target`. A parity harness catches divergence, not
  shared error: verifying a new scenario means sabotaging one leg, since
  narrowing the shared primitive narrows all three.
- **The window's viewer asks the previewer for its measured width
  (bridge 53).** It sent the whole viewport, which counts the chrome, so
  a picture shrunk to it ran off the right edge; the renderer now reports
  the viewer body's columns the way it reported its rows, and the next
  styled preview is asked for that width.
- **The window compares two files (#312, bridge 52).** The last command
  in the parity matrix's deferred list; the list is now empty. Which two
  files and which program are the decisions the TUI already shares
  (`norte_frontend::diffpair`, `[ui] diff`, `diff -u` by default — the
  default now lives in the shared crate). What changes is how it runs:
  the terminal suspends and waits for a key; the window hands a
  `RunProgram` native effect to the process that hosts it — the program
  resolved to an absolute path before any `cwd` (ADR 0082), the two paths
  interpolated, everything in bytes — which launches it detached when
  `[ui] diff_detached` says the differ opens a window, and otherwise runs
  it, waits with a deadline, captures what it printed and hands it back
  as an action. The host masks it line by line and shows it in a program
  output panel until Esc closes it.
- **The window renames in batch by template (#310).** `pane.rename-batch`
  opens the template prompt, prefilled with `[N].[E]` like the TUI, over
  what is marked or under the cursor. The template is checked with the
  reader in front of it (an empty one, or one that would leave a name
  empty or with a `/`) and the prompt comes back with what was typed and
  the reason in the status line; a good one generates the plan without a
  model and puts it through the same review as the AI plan, with the
  core's verdict and `plan_hash`, so approving executes exactly what was
  shown. Out of the parity matrix's deferred list; the only command left
  there is comparing two files (#312).
- **The window has the docked viewer (#291).** The last of the seven
  ADR 0058 slot kinds the window painted in grey: `layout.preview` now
  opens a `viewer` slot beside the listing, at equal width, that follows
  the cursor and shows the same viewer the full-screen one shows —
  including a plugin's styled preview, so a picture paints as half blocks
  next to the file list. The rule is the TUI's (ADR 0077): a slot the
  layout does not place reads nothing, a directory or a special file is
  said and not read, and a reply travels with its slot and its token so a
  late one for a cursor that moved is dropped. The slot asks the
  previewer for its own width. The window of lines that fits the slot
  travels, from wherever the viewer is scrolled: with the focus on the
  slot the viewer's keys move it (the same keymap as the full-screen
  viewer), the wheel moves it through the host, and `viewer.close` hands
  the focus back to the listing without closing the slot, as the TUI does.
- **The window has the panel bar (#324, bridge 51).** The TUI got it
  first and the window did not, which is exactly the drift ADR 0077 is
  about. The same row of buttons — one per panel that opens and closes,
  derived from the kind registry so a plugin's panel shows up by itself —
  with the same three states (closed, open, open with the keyboard) and the
  same attention mark (the log with unread warnings, jobs on the board).
  What the bar holds and in what order is decided once, in
  `norte_frontend::panelbar`; the host only gathers state and translates,
  and the letter now comes from the session's language, not the process
  one (`Sitios` carried the `P` of `Places`). A click travels as the
  button's index and opens the panel through the same dispatch as its
  shortcut (ADR 0069); the bar rides as a patch on any update that changes
  it, so a panel opened by key, menu, palette or the bar itself refreshes
  it alike. `[ui] panel_bar = false` hides it, as in the TUI.
- **`org.norte.image-ansi`: pictures in the viewer.** A `previewer` for
  `image/png`, `image/jpeg` and `image/gif` (first frame) that paints the
  picture as `▀` half-block cells, the upper pixel in the foreground and the
  lower in the background — the two fields `norte:plugin@0.9.0` added — and
  shrinks it to the width the viewer reports, never enlarging; transparency
  is blended over black. Decoding is the pure-Rust `image` crate with its
  dimensions and allocation bounded; a file at the host's 1 MiB read cap is
  refused with a line rather than rendered from a truncated prefix. The
  host learned `png`/`jpg`/`jpeg`/`gif`/`webp` → `image/*`, and the
  per-line span cap moved from 64 to 256 because a photo is one span per
  cell (ADR 0037 amendment). `just plugin-image-ansi`.
- **`org.norte.markdown`: Markdown as styled lines in the viewer.** A
  `previewer` for `text/markdown` — headings in the theme's title role with
  the hashes dropped, emphasis and strong in their own colours, inline and
  fenced code in the info role with the fences replaced by a `[lang]` line,
  bullets and numbered lists, quotes with a bar, links as `text (url)`,
  tables as `a | b`, task-list boxes. Rendering is a pure function over the
  CommonMark event stream (`pulldown-cmark`, no default features) with its
  own tests; the gate installs the real guest and checks the roles and the
  fences end to end. Two host changes came with it: `.md`/`.markdown` are
  `text/markdown` instead of `text/plain`, and **an exact mimetype now beats
  a glob** when two previewers match (ADR 0037 amendment) — with syntect's
  `text/*` also installed, the Markdown plugin paints Markdown and syntect
  keeps the rest, whatever the alphabet of their ids. `just plugin-markdown`.
- **`org.norte.media-info`: `dims` and `duration` columns from file
  headers.** A `columns` plugin with `location = "read"` and no root marker:
  for a file whose extension claims an image (PNG, JPEG, GIF, WebP) or an
  audio track (WAV, MP3, FLAC) it reads at most 64 KiB through `read-prefix`
  — plus a `stat` for MP3's constant-bitrate estimate — and answers
  `1920×1080` or `3:41`. Anything else is not opened; a header that does not
  parse is an empty cell, never a guess. Each parser is a pure function over
  bytes with its own tests on hand-built headers; the gate installs the real
  guest, runs both columns over a temp directory that includes a 200 KiB
  file with a valid header in front, and checks that the location-less call
  answers nothing. `just plugin-media-info`; add the columns as
  `plugin:org.norte.media-info/dims` and `…/duration`.
- **`org.norte.file-icons`, the first demo plugin.** A `decorator` that puts
  a badge on each row saying what kind of file it is — code, script,
  document, image, audio, video, archive, configuration, and the names that
  mean something on their own (`Cargo.toml`, `Makefile`, `.gitignore`,
  `Dockerfile`, `LICENSE`, `README`) — from the name alone: no capabilities,
  no path, no idea whether an entry is a directory. The extension is the tail
  after the last dot, in bytes, and a dotfile has none — the rule
  `mark.extension` and the rename template already use. Its one setting,
  `style`, switches emoji for ASCII glyphs in the extension manager. Lives in
  `plugins/file-icons/`, installs with `just plugin-file-icons` (and
  `just plugins`), and the gate builds it, installs it, runs it over the
  hostile-name corpus and flips the setting.

- **The window paints a plugin preview's roles and colours** (bridge **49**).
  The TUI has painted styled previews since ADR 0037; the window flattened
  the same spans to plain lines, so a syntax-highlighted preview arrived in
  grey. `ViewerView` now carries `styled`: one entry per visible line, each
  the ordered list of its spans with the theme role's kebab name or the
  plugin's own `#rrggbb`. The renderer paints one `span` per fragment, the
  role through the theme's variables (as row badges already do) and the
  colour inline only when there is no role — the reader's theme wins over a
  plugin's fixed palette, as in the TUI. Text is masked once at entry and
  stays text in the DOM; `lines` is unchanged for the raw view.

- **A plugin says which WIT it was built against, and the host says whether
  it serves it** (ADR 0094). The version of a WIT package is part of every
  interface name a component imports or exports, so any bump made a compiled
  plugin fail inside wasmtime with an error naming one interface — after the
  manager had shown it approved and enabled. The catalogue now reads the
  `norte:*` packages a binary names (with `wasmparser`, no compile) when it
  reads the binary for the approval digest, and a version the host does not
  serve lists the plugin as **broken with both versions**: in the manager, in
  `norte plugin list`, and in `norte doctor` as its own warning,
  `plugin-wit-mismatch`, because the fix is a rebuild and not an edit. The
  state file is untouched; a rebuilt binary is approved again (#241). The
  host serves one version of each package, kept equal to the `.wit` files by
  a structural test; there is no compatibility window, and the policy for
  bumps — minor for any change, named in this file with "plugins built
  against `norte:plugin@X` need a rebuild" — is written down for the first
  time. Verified on this machine: the syntect previewer installed in August
  was built against `norte:plugin@0.7.0` and had been silently dead since the
  0.8.0 bump; doctor now says so.
- **A plugin author guide, a template, and `just plugins`.**
  [`docs/plugins.md`](docs/plugins.md) is the whole story for a third party:
  the five kinds, the manifest field by field, what each capability grants
  and what approving shows, `[config]`, layout, building, installing and
  consenting, diagnostics, help pages, WIT compatibility, and a walk-through
  from an empty directory to a running command. [`plugins/template/`](plugins/template/)
  is the smallest previewer+command guest that builds, every file commented,
  built and installed by an end-to-end test so it cannot rot.
  `just plugin-git-status` installs the official columns plugin the way
  `plugin-syntect` does; `just plugins` installs every official one.

- **A provider plugin serves the scheme it declares** (ADR 0093). A
  `[[contributions.provider]]` could be declared, approved and enabled, and
  nothing ever resolved it: the connection manager matched schemes by hand
  against the core's providers and an embedded FTP guest, so a third-party
  provider was inert on arrival — the hook lie, one floor up, and the opposite
  of what ADR 0041 promised. The manager now asks the catalogue first for any
  scheme that is not the core's: an approved and enabled plugin declaring
  `webdav` serves `webdav://host`, instantiated under its own manifest's
  capabilities and configured with the connection's endpoint and credentials.
  The guest has no DNS, so when its manifest declares `net` the host resolves
  the endpoint through the same anti-SSRF filter the FTP guest uses and adds
  exactly `ip:port` to the allow-list the human approved — the URL's port or
  the contribution's `default-port`, never the whole host, and with neither
  the connection is refused. What runs is what was approved: the bytes of
  `plugin.wasm` are hashed and compared with the digest the catalogue anchored
  before the guest is instantiated. `file`, `sftp`, `ftp` and `s3` are
  **reserved**: a manifest claiming one is rejected, and the registry never
  answers for them, so approving a plugin can never put it in front of a
  backend with trash, resume and TLS — or in front of the stored FTP
  passwords. The scheme a provider claims now shows among its capabilities as
  `provider:webdav`, because that is what approving grants. The connection
  parser accepts any scheme a `VPath` can carry instead of a closed list, the
  CLI routes an argument as a URL when an installed provider declares its
  scheme, and `norte doctor` warns about a connection whose scheme nobody
  serves — the typo the closed list used to catch. Proven end to end with the
  `provider-mem` guest installed for real: unapproved, the scheme does not
  exist; consented, it lists; with `net`, a metadata-range endpoint is refused
  and a binary swapped after approval stops serving.
- **`norte plugin list` and `norte plugin uninstall <id>`.** The CLI could
  install a plugin and never remove it. `list` prints what the extension
  manager shows — id, category, the two facts (approved, enabled), the
  capabilities approving would grant — and counts broken ones without listing
  them. `uninstall` removes the directory **and withdraws the approval**, for
  the reason `install --force` already did: the state file is merged on write,
  so a removed key would survive and a plugin installed later under the same id
  would inherit consent given to another binary. The id is validated as a
  plugin id before it becomes a path.

### Changed

- **A manifest declaring `ai` is rejected, not accepted and ignored** (ADR 0022
  amendment). The capability parsed, entered the approval digest and painted a
  badge in the manager, and no host interface honoured it: a human approved
  "AI access" and granted nothing — the declared capability nobody honours
  (ADR 0088), with the same shape as the hooks A2 closed. Same remedy: the
  manifest is refused with the reason, the field stays because spec §7.1 names
  it, and digests of manifests without `ai` do not move.

- **The daemon's log can be read over the wire** (#328, protocol **0.65.0**,
  ADR 0092). Two read-only methods: `log.tail { cursor, max }` answers with the
  lines after `cursor`, the next cursor, how many that cursor missed, the level
  currently being captured and how deep the ring goes; `log.level { level }`
  raises what the daemon captures and answers with the level that actually
  took. This is the wire half of the hole #326 named — a frontend with its own
  daemon paints its own process's lines, and the providers, the journal, the
  policy and the reason a connection failed are all on the other side of a
  socket.
  It is **pulled with a cursor and not pushed**, because the ring already owns a
  monotonic counter: the daemon keeps no per-client state, and where a dropped
  notification would be a silent hole, a stale cursor is arithmetic — the answer
  says exactly how many lines fell off the back. A `null` cursor means "whatever
  you have", which is not the same as `0`: a zero would make a ring that has
  already wrapped report a gap nobody actually missed.
  Raising the level is a **method** and not a field the client applies, so the
  cap that keeps an FTP password out of the panel (`suppaftp` logs
  `PASS <password>` at TRACE, #43) stays in the only process that can enforce
  it. That cap is a whitelist, and it now matches by **module segment**: it used
  to accept any target starting with `norte`/`ntc` as a raw prefix, so a future
  dependency named `nortex` or `ntcp` would have silently earned TRACE into a
  ring any local client can raise and read. Agents are refused on both: the daemon's ring names paths, connections and
  other sessions, so for a scoped agent it is an existence oracle for everything
  outside its sandbox.
  A daemon with no ring to serve — built without the `logging` feature, or one
  whose mount failed because a subscriber was already installed — knows both
  methods and answers `Unsupported`, never an empty success, and the panel falls
  back to its local ring **saying why**: an empty log and an absent log must not
  read alike. A 0.64 daemon is not that case: a newer client is refused at
  `initialize` with `VERSION_MISMATCH` and never gets as far as asking. And a
  `level` outside the vocabulary is `-32602`, not `Unsupported`, so that "you
  sent a typo" stays distinguishable from "this daemon has no log".

- **The window's log panel reads the daemon too** (#328, bridge **48**). The
  panel now merges two rings by timestamp and marks every line with the process
  it came from — in a mixed list "the provider failed" and "the window could not
  paint it" read alike, and they are two different faults. A selector cycles
  between this window, the daemon and both; it is **not painted at all** when
  the daemon has never answered its log, because a control that switches between
  three views of one ring promises something that does not exist. For the same
  reason the panel reports the *effective* source rather than the stored
  preference: with no second ring, "both" is shown as "window".
  The remote half hangs off the 500 ms tick the panel already re-arms, so there
  is no second timer to forget to stop, and the request carries the panel's
  **epoch**: between asking and answering there is room for a close and a
  reopen, and lines from the previous session landing in the new panel would be
  history nobody asked for, in front of the history they did.
  When the daemon has no log to serve the panel falls back to the local ring and
  **says so** — the same rule #326 wrote for the process, applied to the other
  shore. The level *marked* is always the one the panel **shows**, in every
  source, because that is what the buttons control and what filters the list;
  the daemon's own level rides the "capturing" line, which names whose ring it
  describes. Marking the daemon's there was the worst bug of the first pass:
  with the daemon at `trace` and the panel at `info`, the header said `trace`
  while every `debug` line crossed the socket and was dropped in silence.

- **`ntc --socket` reads the daemon's log too** (#328). The same hole, the same
  answer, in the other frontend — a decision one takes and the other does not
  diverges in silence (ADR 0077). The panel merges the two rings by timestamp,
  rules the daemon's lines down the margin, and `s` cycles this terminal, the
  daemon and both. `s` is a key of the panel and not a command of the keymap,
  like the five level keys and `/` beside it: it only exists while the panel
  holds the keyboard, so **no preset binds it**, and it is only offered at all
  when a daemon is serving its log.
  A plain `ntc` — no daemon, which is the default start — is **unchanged from
  #326**: nothing is asked over any wire, and the panel says nothing about an
  origin, because with one process and one ring there is nothing to tell apart
  and the absence of the segment is the answer. The embedded core answers
  `Unsupported` to `log.tail` for a good reason (its ring is the one this panel
  is already reading), and reading that as a fact about a daemon is how the
  border ended up saying "this daemon does not serve its log" where there was
  no daemon at all.
  There is no timer: the terminal already repaints per frame, so the pull hangs
  off the loop it already runs, with a 500 ms floor and one request in flight —
  ten a second to paint the same thing would be the cost of having no floor. The
  request carries the panel's **epoch**, so a late answer cannot land its cursor
  in the next opening. The two miss counters stay apart and are never summed:
  the local ring's counts what it has evicted since the process started, the
  daemon's what *this* opening missed. And raising the level asks the daemon
  too, announcing on the status bar that the ring being raised is global to
  every client of that daemon and never lowers — the announcement goes to the
  bar and not to the panel's border because a border is one line that `ratatui`
  clips in silence, and the clipped thing was the sentence naming the daemon's
  level.

- **The window has the log panel** (#326, bridge **46**). It has been in the
  TUI since #323, and everything shared was already built — the in-memory ring
  and its `tracing` layer in `norte-config`, the presentation state (level
  filter, text filter, following the end) in `norte-frontend`. What the window
  did was fall through to "unsupported kind", in grey: opening a slot that only
  paints greyed out is not opening it. The level buttons raise what the ring
  *captures* and never lower it, because filtering on screen what was never
  recorded is impossible, and because dropping back to errors and climbing again
  would show a hole the size of the time you spent down there.
  It also says **whose** log it is, and that line is the point: `norte-gui`
  starts its own daemon, so this ring holds this process's lines and not the
  daemon's — where the providers, the journal and the policy live. In the
  embedded TUI they are the same process and it never came up. Saying nothing
  would make the panel look broken: you open it while a connection is failing,
  do not find the line that explains it, and conclude the panel does not work
  rather than that you are looking somewhere else. Carrying the daemon's lines
  over the wire is a separate piece of work.
  Lines the ring had to drop are counted on screen, for the same reason: a log
  with a silent hole lies about what happened, because a missing line is
  indistinguishable from an event that never occurred.

- **The window asks for a connection's password too** (#327, bridge **45**,
  ADR 0091).
  The TUI has done this since #325: a connection with `secret = "prompt"` whose
  three sources have all come up empty suspends the navigation, asks, hands the
  answer to the core and retries. The window did not — it painted the text of
  `err-secret-needed`, which names an environment variable, and that was the end
  of the road. It is the parity hole ADR 0077 exists to close: a decision one
  frontend takes and the other does not diverges in silence.
  What is typed never crosses to the painting layer, and — the part that took a
  second pass — **the host does not know what is being typed at all**. Every
  other dialog field sends its whole contents to the host on each keystroke,
  which is right for a filename and wrong for a password: it puts `h`, `hu`,
  `hun`… across the bridge, each in a heap block nobody overwrites. The password
  now crosses once, with the answer. The window field is masked by the browser
  itself, so there was never anything for the host to paint.
  The buffer type that holds the one remaining copy moved from the TUI to the
  shared crate: it is a security type — a `Debug` that redacts, a wipe on drop,
  capacity reserved up front so growing the string never leaves a half-typed
  password behind on the heap — and two copies of one are two places for a
  guarantee to be forgotten. Its reserve is now counted in bytes, which it was
  not: an accented passphrase fitted the character cap and not the byte reserve,
  so the string grew, and growing is exactly the thing that leaves the old
  contents behind.
  A password too long to fit is refused rather than truncated: handing over the
  first 256 characters of a longer passphrase fails authentication with no
  indication of why, and a masked field gives the reader no way to suspect it.
  Confirming an empty field is inert: it neither hands anything over nor closes
  the dialog. An empty secret is not an empty session — it makes the connection
  authenticate with the ambient chain, which is #320.
  The question names the connection **and where it connects to**, each in its
  own field and never interpolated into the sentence. The name was chosen by a
  configuration file, and a configuration file can arrive from someone else's
  dotfiles.

- **A connection that fails now says why** (#322, protocol **0.64.0**,
  ADR 0090). Until now every failed dial reached the screen as the same three
  words — permission denied — whether the secret was missing, empty, not text,
  unreadable from the store, rejected by the server, or simply lacking a user.
  The sentence that told them apart ("the secret for «rosetta» is defined but
  EMPTY") was written to the daemon's log and dropped. Worse, it was
  diagnosable or not depending on the *transport*: the embedded CLI printed it
  to its own stderr, the daemon did not, and inside the TUI the alternate
  screen ate even that.
  The reason now travels as its own notification, `connection.failed`, with a
  closed vocabulary you can compare by equality and an optional human sentence
  beside it. The error keeps its category and changes in no way — the taxonomy
  is what code decides with, and a sentence is not a category.
  The sentence is an allowlist, not a `to_string()`: only the reasons norte
  composes out of its own fields cross the wire. The ones that wrap third-party
  text, paths, or the configuration file stay behind, because rule 10 does not
  distinguish between "a secret" and "something that may contain a secret".
  The authority travels without userinfo, masked and capped, in its own field
  and never interpolated into the phrase — a failed connection is the place
  where a host like `bank.example@evil.example` has the most to gain from being
  read as the other one.
  Both frontends print it from the same composer, so the TUI and the window
  cannot drift apart on it, and `norte connect` — the command you type
  precisely to find out why something will not connect — prints it too.
  The reason also survives a retry: a failed dial goes into a cooldown, and the
  attempt inside that window used to be served from the cache without ever
  passing the explanation on, so the diagnosis disappeared at the one moment
  someone was looking for it.
  A peer on 0.63 drops the notification in silence and keeps exactly what it
  had: the category, without the sentence.

- **A connection can ask for its password** (#325, protocol **0.63.0**,
  ADR 0015 amended). `connections.toml` holds references, and the secret is
  looked for in three places: the environment variable, the system keyring, an
  encrypted `secrets.age`. When none of them has it the connection simply
  failed — which is correct on a CI runner and useless on a laptop, where the
  person who knows the password is sitting in front of the screen. Add
  `secret = "prompt"` to the entry and norte asks instead: a dialog that does
  not show what you type, only appearing once those three have come up empty.
  Opt-in and last for that reason — a machine with the variable set never sees
  it, and a headless daemon never blocks on a question nobody will answer.
  A typed URL with no entry behind it never prompts either; teaching people to
  type passwords into whichever dialog appears is how phishing works. It
  applies to `password` and `access-key` auth only, and `norte doctor` now says
  so when the key is set where it does nothing.
  The dialog names the connection **and where it connects to**. That second
  line is the point: the name was chosen by the config file, and a config file
  can arrive from someone else's dotfiles or a single edited line, so "work"
  tells you nothing about whether that entry still points where it did
  yesterday. It is the same reason the host-key dialog shows you a fingerprint.
  What you type lives in memory for as long as the daemon stands and is
  written nowhere — not the keyring, not `secrets.age`, not the config file —
  so the next session asks again; the variable or the keyring are still the
  place to stop typing it. Offering to remember it is deliberately left out:
  on Linux the keyring backend is not even compiled in. Mistyping it does not
  trap you either: when the server rejects a password you typed, norte forgets
  it and asks again, instead of shadowing the environment variable you would
  reach for with a value nothing can clear short of stopping the daemon.
  You can paste into it, which is how most people will answer it.
  The mechanism is the host-key TOFU flow reused whole. A new
  `Error::SecretNeeded` suspends *that* navigation, the answer returns through
  `connection.provide_secret`, and the navigation is retried — so it resumes in
  the pane that started it, which is not necessarily the focused one. Like
  trusting a host key, an agent connection cannot call it: injecting session
  credentials would be choosing which identity you act under on the remote
  host. A peer one version behind does not understand `SecretNeeded` and will
  report it as an unknown error; nothing else changes for it — but note that an
  older binary reading a config file that already has `secret = "prompt"`
  rejects the whole file, so remove the key before downgrading.
  The password crosses the daemon socket in cleartext, which is accepted and
  written down: the socket is 0600 and owned by you, and anyone who can read it
  can read the daemon's memory, where the secret has to live anyway. What is
  not accepted is it leaking on the way — the params redact themselves in
  `Debug`, every span is `skip_all`, and the terminal's own buffer is wiped
  when the dialog closes. ADR 0015 says which copies are wiped and which are
  not, rather than implying all of them are. The window does not paint this
  dialog yet (#327).
- **A bar that shows the panels exist** (#324): one row under the menu bar,
  one letter per panel — Places, Tree, Viewer, Jobs, Details, Log. They were
  reachable by shortcut, by menu and by the palette, and all three require
  already knowing the panel is there; nothing on screen said so. That also
  meant a panel contributed by a plugin was invisible to anyone who did not go
  looking, which is most people, and the panel registry is open precisely so
  plugins can contribute one. The bar is therefore derived from that registry
  rather than written out: built-ins keep their positions and anything
  contributed lands at the end, so the row you learn does not move under you.
  Each button says three things a plain launcher would not: whether the panel
  is open, whether it has the keyboard — that is where your keys are about to
  go — and whether it has something to say, like jobs still running or errors
  in the log you have not looked at. Clicking one opens it, through the same
  dispatch as its shortcut. It costs a row; `panel_bar = false` gives it back.
  The letters started out as the shortcut's own letter, read from the live
  keymap, and that was thrown away after seeing it painted: `B Q J M T L` is
  precise about which key to press and silent about what each one opens, and a
  bar whose whole job is to reveal the panels cannot require knowing them.
- **A log panel inside the terminal frontend** (`alt+l` in all seven presets,
  or Panels → Log). The file log has existed since #255 and it answers
  questions *afterwards*; it is no help while something is going wrong in front
  of you, because it lives in another terminal. This is the same lines, in
  memory, next to the reader: `e`/`w`/`i`/`d`/`t` choose how much is shown, `/`
  filters by text and searches the module name too, arrows and pages detach
  from the tail so you can read while lines keep arriving, and `End`
  re-attaches. It is what answers "and *why* did that fail?" — a connection
  that dies leaves a "permission denied" on the status bar that says nothing,
  while the exact reason was already written, somewhere else.
  Two things it refuses to get wrong. Asking for more detail raises the
  recording level for real, not just the filter — filtering to DEBUG what was
  recorded at INFO would show nothing and look broken — and lowering it again
  does NOT stop recording, so going down and back up cannot erase the very
  stretch you were investigating. And the panel says what it is filtering and
  how many old lines it dropped, because a viewer that looks empty has to tell
  "nothing happened" apart from "you are filtering it out", and one that
  discards silently makes a reader hunt for a line that was there a moment ago.
  Making this possible needed the shared logging setup to move from one global
  filter to per-layer filters: under a global INFO filter, DEBUG events are
  never emitted at all, so no panel can show them later. The file and stderr
  layers keep exactly the filter they had, **including the hard `suppaftp=info`
  cap from #43** — that one matters more here than anywhere, because this
  level is raised by a keypress, and `suppaftp` logs `PASS <password>` at
  TRACE.
- **The terminal frontend says when it is waiting** (#323). Opening a remote
  connection froze the screen, and the cause was not a missing spinner: a slow
  navigation ran its own event loop that read keys — so `Esc` always
  worked — and never repainted, so the terminal kept the last frame for several
  seconds, which looks exactly like a hang. Now the pane that is waiting shows
  a spinner and the destination it is going to, the status line says what is
  happening and that `Esc` cancels, and the pane keeps its previous listing
  underneath: if the connection fails you are still where you were. Nothing
  appears below 250 ms, because a flicker on every local `cd` is how an
  indicator stops being read, and nothing pretends to know a percentage it
  cannot have. The same treatment covers the two sibling waits that had the
  identical defect and nobody had noticed — refreshing panes after a task, and
  opening a remote file in the viewer — because the wait itself is now one
  shared piece of code rather than a pattern copied per site; the fourth one
  inherits the spinner for free. The state lives in the shared frontend crate,
  so the window can paint the same thing rather than deciding it again.
- **`norte paths` says where everything lives.** `norte doctor` already knew
  about the four config surfaces, the plugin directory, the secret store and
  the log — it validated them — and printed the path of exactly one. Everything
  else was folklore, and folklore is wrong the moment `NORTE_CONFIG_DIR` or
  `XDG_CONFIG_HOME` is set, which is precisely when someone asks. The new
  command lists the config layers in ascending precedence with their kind, then
  the resolved config dir and each file under it, then state, the effective log
  directory (`[log] dir` honoured, not the default) and the daemon socket —
  each marked present or not, because "not there" is the answer to "why is
  norte ignoring my file". It resolves nothing on its own: every path comes
  from the same function the rest of the binary uses, so the command cannot
  drift from the code it explains. Read-only like `doctor` — asking where the
  config dir is does not create it — and it always exits 0, because a missing
  optional file is an answer and judging config is `doctor`'s job. `--json`
  for scripts.
- **The window can save the workspace as a profile** (#318), which until now
  only the terminal could. The dialog is new; the CONTENT is not decided twice —
  the snapshot builder moved into the shared crate, so both frontends call one
  function and there is nothing left to keep in sync. That is ADR 0077's lesson
  applied rather than policed: two "save as" that produced different profiles
  would make a profile depend on where you saved it from.
- **Marking while you move**, in all seven presets and both frontends. Space
  and Insert marked going DOWN and there was nothing for going up, nothing for
  a range, and nothing for "these and only these" — so a reader who overshot by
  one row had to go back, unmark by hand and come forward again. Now
  `Shift`+Up/Down toggles a row and moves, `Shift`+PageUp/PageDown does the
  same to a whole screenful, and `Shift`+Home/End marks everything to one edge
  **and unmarks the other side**. That last pair is Krusader's, semantics
  included: its documentation says "selects everything above the cursor and
  deselects everything below the cursor, if selected", and that unmarking is
  what makes it a way to say "these and only these" rather than a way to add a
  range — so it is what norte does, and the cursor stays put, because the
  cursor is the edge you just cut at. The row under the cursor decides whether
  a gesture marks or unmarks and that decision applies to the whole span, which
  is what makes each of them reversible and what makes "to unmark, hold the
  modifier and move the other way" true rather than nearly true. None of them
  reaches a row the quick filter is hiding, and none of them can mark the `..`
  row.
- **A real subshell behind the panels** (#142, ADR 0084). `app.toggle-panels`
  used to release the terminal and show whatever the host's scrollback already
  held; the help topic had a section called "What it is not" pointing at this
  issue. Now the key hands the terminal to a shell that STAYS ALIVE behind the
  panels: press it again to come back, a third time to return to the same
  shell, with its history, its variables and the half-typed line still there.
  It is the one of the three shell commands you can leave a `make` running in.
  The panel and the shell follow each other — going in, the shell is sent to
  the active pane's directory; coming back, a `cd` moves the panel. The shell
  says where it is by printing a private OSC marker in its prompt, which norte
  installs by TYPING it into the shell (bash, zsh and fish): no file of yours
  is touched, a `PROMPT_COMMAND` already there is kept — array or string, which
  bash 5.1 made a real distinction — and the lines are hidden from your
  history. The marker carries a per-session secret, so a file whose *contents*
  contain that escape sequence cannot steer your panel by being `cat`-ed. The
  path travels as bytes end to end, so a directory whose name is not UTF-8, or
  contains a BEL, or is called `; rm -rf ~`, is a directory: what norte types
  into the shell is only ever digits and quotes, because a pty is read by a
  line editor, where a byte like `0x15` is not text but "erase this line" — an
  ordinary directory name could otherwise have run a command. The `cd` is typed
  only when the shell is idle at its prompt, so a half-written line, a running
  build or an open editor is left alone rather than having a `cd` appended to
  it. The shell starts on the first press and never before, is replaced rather
  than resurrected once you `exit` it, and dies with norte however norte
  leaves. The key that brings the panels back is the one that gave them away,
  read from the keymap: a preset that binds the command to a key SEQUENCE gets
  a refusal instead, because the first chord of a sequence belongs to the shell
  you are typing in. One change of behaviour: it now declines on a remote pane,
  which the scrollback version did not have to, and on Windows, where there is
  no pty to hand over.
- **Every listing carries a `..` row** (`[ui] parent_entry`, on by default).
  The row an orthodox reader expects: the cursor lands on it and Enter goes up.
  It is never an OPERAND — `selected()` answers `None` over it, so the
  eighty-seven callers that ask "what is selected" in order to copy, move,
  rename or delete get "nothing" rather than the parent directory — and it
  cannot be marked by any of the six paths that mark, because marking it would
  put the PARENT into the list of what gets copied or deleted. It is painted
  `..` and not the parent's name, it never appears in a root, and it stays
  first through sorting, hidden-file filtering and paginated fills.
- **Panel borders can be dragged with the mouse**, in both frontends, and the
  size is remembered — it lives in the arrangement tree, which the session
  already saves per profile. The drag primitive is absolute rather than a step
  (a key wants two cells per press; a pointer says WHERE the border goes), the
  pair's total is conserved so the rest of the row is untouched, and weights
  are renormalised so a drag can land somewhere other than the exact middle. In
  the window the renderer sends the pointer position in layout cells and the
  host decides what that means, which is the same split of responsibility as
  every other pointer gesture.
- **The menu reopens where it was**, in both frontends, instead of always
  starting at the first one.
- **Profiles reach the window** (ADR 0079, bridge 43). `profile.pick`,
  `profile.next` and `profile.prev` used to answer "not here"; now the window
  lists them, cycles through them and switches live. The picker is the shared
  model, so what each row says is not decided twice: a profile that will not
  load is SHOWN with its reason rather than hidden — hiding a directory the
  reader created is worse than showing it broken — and the two warnings the
  spec names are on the row (what else in norte shares that name, and which
  profile cannot remember your panels). Reading `profiles/` and loading the
  configuration both happen off the actor. A switch applies the theme, the
  whole keymap for all three screens, the columns, the favourites and the
  arrangement the profile names; a profile that fails to load changes nothing
  and says why. What cannot be applied without restarting is named, and that
  list is the window's own rather than shared with the terminal: here the theme
  DOES apply and the fonts do not, which is the opposite of the terminal's
  answer. Remembering where you left each panel *inside* each profile is still
  to come — the arrangement comes from the profile's configuration, not from
  its saved state.
- **The window can change theme while running** (bridge 42). It could not, and
  not by oversight: the colours cross to the webview as CSS variables inside
  the catalog, and that catalog was built once at startup — which is why the
  theme screen was read-only and said so. A new native effect tells the hosting
  process which theme is now active; it rebuilds the catalog and the renderer
  re-applies the variables. With that path open, the theme screen CHOOSES:
  presets, the cursor on the one in use, live preview as you move, Enter fixes
  and saves it, Escape goes back to the one you had. The save goes to the
  highest editable layer — the active profile if there is one, the user's
  otherwise — which is the rule ADR 0079 already wrote for the terminal.
- **The window has a menu bar** (bridge 41). It was the most visible gap next
  to the terminal: `app.menu` resolved to "not here", and the parity list said
  so in writing — so the window's commands were reachable only by knowing a
  name in the palette or a chord by heart. The menus and their entries are
  `norte_frontend::menu`, the same model the TUI paints, so what is in each
  menu and in what order is not decided twice; the window adds the projection
  (titles and short labels already translated, each entry's real chord in the
  active preset, and whether this window can run it). An entry the window
  cannot run still appears, dimmed: the menu is where you see what exists, and
  hiding what this frontend does not do turns a limitation into a mystery.
  Keyboard and mouse both: arrows walk it, Enter runs, Esc closes, clicking
  opens/points/runs and clicking outside closes. The bar reserves its row
  rather than floating over it — the host lays out against the height the
  renderer declares, so a floating bar would hide the listing's first row — and
  with `[ui] menu_bar` off it reserves nothing while the key still opens the
  menu.
- **Configuration profiles: the mechanism** (ADR 0079). A fourth configuration
  layer the reader picks by name — `profiles/<name>/`, with the shape of any
  other layer — sitting between the user's own configuration and a trusted
  project's, plus its own live screen state in the UI session. A profile
  carries a theme, a keymap, columns, favourites, arrangements and a starting
  directory per slot; it cannot touch the daemon's transport, the AI, the logs,
  the anti-bomb limits, the policy engine, or run an `init.lua`. That last line
  is the decision the ADR is named after: **a profile declares, it does not
  execute**, because unlike every other layer a profile is chosen from a list
  while the program is running, and a picker that grants in silence is a
  permission escalator. A broken profile answers according to who asked for it
  — `--profile` aborts, a remembered one starts without it and says so, and a
  switch is refused with the current profile untouched. No protocol change: the
  session body is opaque to the core, so the arrangement map ADR 0058 left
  keyed by profile finally has more than one key. **There is no user interface
  for any of this yet** — the layer and the state land first so the surfaces
  are built on something already tested.
- **Profiles in the terminal** (ADR 0079). `profile.pick` opens a picker that
  marks the one you are in, shows each profile's title beside its directory
  name, and says out loud what would otherwise bite later: which one will not
  load and why, which name cannot remember your panels, and which name also
  belongs to a layout or a keymap preset — picking the profile `far` binds not
  one key of the preset `far`. `profile.next`/`profile.prev` cycle without
  opening anything. All of them ship unbound: choosing which key means "profile"
  on top of keys that mean something else in the manager you came from is not a
  decision norte makes for you. `--profile <name>` starts in one for a single
  run and, being known before anything connects, applies even the language;
  otherwise norte returns to the profile you were last in and switches to it
  live, announcing the one thing a live switch cannot do. A switch that fails
  leaves you exactly where you were.

- **A copy can say which directory the human was looking at** (#295, protocol
  **0.54.0**, ADR 0073). `fs.list` now returns a `dir_anchor` — the opaque
  identity of the directory it listed — and `fs.copy`/`fs.move` accept it back
  as `dest_anchor` and refuse to write when the destination is no longer that
  node. It closes what ADR 0072 could not: a symlink *already in place* when
  the core first looks is, from inside the core, indistinguishable from a
  legitimate `~/copias -> /mnt/disco/copias`, and rejecting both would break
  copying to `/tmp` on macOS or `/bin` on a usrmerge Linux. The one thing that
  separates them lives outside the core — the human was not looking at that
  other node. The anchor carries no inode or device number: it is a hash under
  a per-process secret, so equality survives and neither forgery nor deduction
  does. The SDK remembers the anchor of every directory it lists and sends the
  right one automatically, so every frontend gains the check without a line of
  code, and a `norte cp` against a hand-typed path behaves exactly as before.

- **Packaging works, and it was exercised for the first time** (#256).
  `just gui-package` produces a `.deb` and an AppImage, and both carry
  `norte-gui`, `norte` and `ntc` — so on a clean install the window has a
  daemon to start. Verified end to end from inside the AppImage: its own daemon
  binds its socket, its own CLI lists through it, and it shuts down cleanly.
  The build itself was fine; four things around it were not, and every one of
  them is invisible until someone installs the thing. The package description
  read "Spike vertical del renderer de Tauri sobre norte-ui-host" — a
  development note in front of whoever is deciding whether to install this. The
  category was a bare `Utility`, which keeps a file manager out of the list
  where file managers are looked for. There was no `MimeType`, so "open folder
  with…" never offered norte — for a file manager that is the whole desktop
  integration. And `Exec` handed `%U` (a `file://` URL) to a binary that takes
  a path, which would have opened the window on an error instead of on the
  folder the desktop just named. All four are now pinned by tests.

- **The window's dialogs now go through the shared key resolver** (#287). They
  used to answer to fixed keys, so a preset that rebound `dialog.confirm`
  changed the TUI and not the window — exactly the drift the shared catalogue
  exists to prevent. Twenty of the twenty-two `dialog.*` verbs are now honoured:
  the four collision outcomes (overwrite/skip/rename/newer, which previously had
  **no key at all**), list navigation everywhere, and the verbs that name what
  each surface does — `toggle-enabled`, `move-up`/`move-down`, `sort`,
  `cycle-format`, `add`, `pane`, `back`, `filter`. `Home`/`End` stay fixed keys,
  because the catalogue has no verb for "to the top" inside a dialog, and their
  absence from the implemented list is where you can see that. The columns
  picker's footer is now **built from the keymap** rather than being a
  translated string naming keys: that string had already stopped being true.
  Only `dialog.remove` stays deferred — removing a row only means something over
  a list you can edit, and the window's only list (settings) is read-only.

- **`fs.create`: the protocol can create an empty file** (protocol **0.57.0**,
  ADR 0076). It carries an optional `dest_anchor` (#295) that `fs.mkdir` does
  not, because it is the only method on the wire whose success hands a path to a
  program *outside* norte: a frontend creates the file to open it with the
  desktop's editor, so a symlink swapped in between the listing and the
  confirmation costs not an empty file but the whole editing session typed
  afterwards, into a directory the human was never looking at. And `create` is
  its own policy permission — `PolicyOp::Create`, grantable over the wire like
  the other four. It was the gap behind `pane.edit-new`, the last command of #290 the
  window could not do: `fs.mkdir` makes a directory, `fs.copy` writes one that
  already exists somewhere else, and nothing said "an empty file, here, by this
  name". The TUI had been getting away without it — it launched `$EDITOR` and
  let the editor create the file on save, which ADR 0077 then took out — and a
  window has no terminal to hand a process to.
  It is a mutation like any other: journal `Created` with its undo, its own
  policy permission (`PolicyOp::Create`, not folded into `mkdir` — letting
  something create folders is not letting it create files), progress and
  cancellation. It **fails if the destination exists**: there is no reading of
  "create" that means "empty whatever is there", and a method that truncates in
  silence is data loss with an innocent name.

- **The window can create a file and edit it** (`pane.edit-new`). It asks for
  the name, creates the file, and only when the task actually *succeeds* hands
  it to the desktop application. Opening earlier would launch an editor over a
  file that is not there yet, and the empty buffer it shows would look exactly
  like success. On a remote pane it is refused before the name is typed:
  `xdg-open` cannot be given an `sftp://`, and saying so afterwards arrives too
  late. With this, **every command of #290 that needed new surface is built**
  except `layout.preview`, which stays out with its reason.

- **The window can close a connection** (`pane.disconnect`). The panel does not
  stay looking at something it can no longer read: it walks its own back-trail
  and returns to where it was *before* connecting, skipping anything on the
  machine it just left — going back to another path of the same session would
  reopen the connection the gesture asked to close. With nothing else in the
  trail it falls back to `$HOME`. On a local panel there is nothing to close
  and it says so, rather than answering "done" over work it did not do. A close
  that *fails* does not navigate: the panel stays and the session is still
  there, which is what the error says. (The TUI still goes straight home; the
  two will be aligned.)

- **The window has a directory tree** (`pane.tree`, bridge **40**, ADR 0075).
  It occupies a slot like any other kind — it splits, closes and resizes with
  the gestures that already exist — and choosing a branch navigates the
  *listing*, not the tree: the tree stays anchored where it was opened, which
  is what makes having it open worth anything. Two gestures per row, because
  both are needed: the twisty folds, the name navigates. It is lazy and asks
  for one branch per turn, chained — a tree that read itself whole on opening
  would take minutes on a large `$HOME` and hours against a remote. A branch
  that cannot be read is marked read and empty, so it is not re-requested
  forever. The model moved from `norte-tui` to `norte-frontend`, so the TUI and
  the window share it rather than drifting apart.

- **You can drop files from the desktop onto the window** (#283, bridge **39**,
  ADR 0074). Dropping does not copy: it opens the same confirmation a copy
  does, with the destination in its own field and the sources masked line by
  line. That dialog is the only chance the reader gets to see that what arrived
  is not what they dragged — a drop is a gesture without confirmation by
  nature, and the list of URIs is composed by *another* process. It only comes
  in (dragging out would publish the paths of everything marked to any
  application that accepts a drop) and it only copies (moving what another
  application dragged would delete it from wherever that process keeps it, and
  nobody asked that). The destination may be remote: uploading to the server
  what you drag off the desktop is the comfortable case. A path that is not a
  path on this machine is *said*, not swallowed.

- **The window offers a connection picker** (#264). `pane.connect` lists what
  the daemon has configured and navigating to one establishes the session the
  usual way. The URL is masked as an *authority* rather than a path — a host
  can be called `banco.example@malo.example` without carrying a single
  character that gets masked, and "which machine am I connecting to" is the
  only question this picker answers. A URL that does not parse is shown without
  a destination: seeing that it is configured and cannot be opened is more
  honest than hiding it.

- **`connection.list`: the daemon answers which connections are configured**
  (#264, protocol **0.56.0**). It exists so a frontend can offer a connection
  picker *without reading `connections.toml` itself* — reading it would drag
  russh, opendal, suppaftp, age and keyring into a binary that only wants to
  paint a list of names, and the daemon already has all of that because it is
  what opens the sessions.

  It never carries a secret: a connection spec *references* its credentials
  (ADR 0015), and what travels is the URL as written. And it does not connect —
  it answers where one could go; going is navigating to that URL, which already
  establishes the session the usual way, with its TOFU and its policy. A method
  that "connected" would be a second door to what `fs.list` already does.
  Human-only, like `host.volumes`, and the actor gate runs before parsing so an
  agent cannot tell "denied" from "malformed" by the shape of its own request.

- **Desktop notifications, for the three things worth interrupting for**
  (#285): an agent asking for permission, a task finishing, a task failing.
  The first is the one that justifies the mechanism — an approval expires on
  its own if nobody answers, so missing it changes the outcome, while a copy
  is still finished when you come back.

  **Only when the window does not have focus.** With it in front, the status
  bar and the task board already say the same thing, and repeating it outside
  is noise. A renderer that never reports focus behaves as before — notifying
  always — rather than going quiet: losing a notice is worse than repeating it.

  The body carries the file name, and therefore goes through the same masking
  and truncation as a listing row: a notification ends up in the desktop's
  history and may show on the lock screen, so a name with bidi or control
  characters must not be able to pretend there what it cannot pretend here. No
  new dependency: `notify-send` or `kdialog`, through the same native-effects
  channel and the same candidate-list shape as everything else in it.

- **With one panel open, the destination is asked of the desktop** (#284).
  Copying or moving used to be refused outright when there was no second
  listing to take the destination from, which left anyone who had not split the
  window unable to copy at all. The window now opens the desktop's folder
  picker. No new dependency and no new capability: it goes through the same
  native-effects channel as the clipboard and the terminal, invoking `zenity`,
  `kdialog` or `yad` — the same "list of candidates, first one that exists
  wins" shape those already use. Cancelling is an answer and transfers
  nothing.

  The path comes back through the renderer's own door, so it is treated like
  everything from there: validated, and above all **shown in the confirmation
  before a byte moves**. What does *not* travel in that message is what gets
  copied — the operands are still the host's, which is the rule of ADR 0069.
  Only the verb is remembered while the picker is open; the operands are
  recomputed on the way back, because the listing may have changed underneath.

- **The packages carry the daemon** (#256). The `.deb` and the AppImage
  shipped `norte-gui` alone, so on a clean install the window had nothing to
  start — and since #300 starting the daemon is exactly what it does. They now
  carry `norte` and `ntc` as well, landing in `/usr/bin` where the window looks
  for them (next to its own executable, then `PATH`). The `.deb` goes from
  5.6 MB to 43 MB, which is the honest size of a file manager that brings its
  own engine.

- **F4 opens with the desktop's application** (#290). The TUI launches
  `$EDITOR` because it is already inside a terminal; this window is not, and
  opening one on top just to edit a file is more noise than help. What is lost
  is honouring `$EDITOR` — here the desktop decides, and it may open a viewer.
  `pane.edit-new` stayed unimplemented at this point for a reason worth writing
  down: the TUI version launched the editor with *no file* and let it ask for a
  name on save, and `xdg-open` cannot do that — it opens files, not empty
  editors. That is what `fs.create` (ADR 0076) and then ADR 0077 resolved, in
  the other direction: the TUI stopped doing it that way too.

- **The window splits and joins files** (#132, #290). `pane.split-file` asks
  for a piece size and reads it in **binary** — `10M` is 10 MiB, which is what
  it means in a file manager, not ten million — and refuses a zero, because
  pieces of zero bytes never finish. The pieces land in the *target* panel for
  the same reason a copy does: splitting a one-gigabyte file where it already
  sits usually does not fit. `pane.combine-files` only starts from the `.001`:
  beginning at the `.007` would join half a thing, and the core only searches
  forward. Reading a size and finding a piece base moved to the shared crate
  alongside the two rules that moved with packing.

- **The window packs, unpacks and checks containers** (#132, #290).
  `pane.pack` asks for the name and takes the *format* from it — a name whose
  format we cannot write is refused rather than packed into something nobody
  asked for, which is what `.rar` does: read by delegation, never written. The
  base for the stored names is the panel's directory, so whoever unpacks sees
  what was on screen instead of absolute paths. `pane.unpack` needs no method
  of its own: the copy engine accepts an archive's interior as a source, so it
  is the copy the reader could have made by hand — with its journal, its undo
  and its cancellation, and now with the collision dialog of #274 behind it.
  `pane.test-archive` reads the whole container and answers; it writes nothing
  and is not in `MUTAN`.

  Two presentation rules moved to the shared crate on the way (ADR 0066, D14):
  which entries are containers, and which format a name suggests. Both lived in
  the TUI, and two tables of extensions are two places for one to be forgotten
  — after which the same entry navigates on one surface and does not unpack on
  the other.

- **The window can count how much something takes up** (#139, #290).
  `pane.dir-size` counts what is marked — or what sits under the cursor — as a
  single Task for the whole batch, because counting each entry separately would
  make the caller add up the bytes *and* the unreadable ones, and those two do
  not add up the same way. The total is what that Task produces: `fs.dir_size`
  publishes nothing and mutates nothing, its result *is* its terminal progress,
  so the window says it in the status bar when the Task ends. A count with
  unreadable entries inside gets a different sentence — a count is used to
  decide whether something *fits*, so giving a round total without having been
  able to count it whole is a wrong answer, not an incomplete one.

- **The window starts the daemon, and says what killed it when it dies**
  (#300). `norte-gui` used to require a daemon already running: opening a
  window meant opening a terminal first, which is not an architecture decision
  but a chore left to the reader. It now starts one the way the CLI's
  `--daemon` does — looking for `norte` next to its own executable first,
  then on `PATH`, never the working directory.
  When the daemon starts and *dies*, what it said now survives. Its `stderr`
  went to `/dev/null`, so the one sentence explaining why it would never come
  up — "this journal predates `undoes_seq` and has history" — was lost, and the
  caller waited the full ~3.2s backoff to get a `SpawnTimeout` inviting it to
  retry something that could not change. The child is now watched with
  `try_wait` on every turn: if it died, its output comes back in a new
  `ClientError::SpawnFailed` **without exhausting the backoff** (measured: 0.03s
  instead of 3.19s), and the window shows that sentence instead of its own
  "could not connect (retryable: true)". A daemon that *does* start leaves a
  thread draining the pipe, because one nobody reads fills up and blocks the
  daemon on its next write.

- **An RPM comes out of `just gui-package` too** (phase 7.1). The Fedora-family
  baseline the plan asks for, alongside the `.deb` and the AppImage, and
  exercised rather than configured: the built package carries the WebKitGTK and
  GTK runtime requirements (`webkit2gtk4.1`, `gtk3`, plus the soname-level ones
  the bundler derives) and the same desktop entry as the deb — the one with
  `FileManager` and `inode/directory`, without which a file manager is not
  offered under "open folder with…". The explicit `deb` dependency list that
  went in with it came straight back out: the bundler already emits exactly
  those two, so declaring them again produced a `Depends:` field with each name
  twice. Checked by reading the metadata of the artefacts, not by trusting the
  config.

- **Packing says which names mean something else elsewhere** (#250, protocol
  **0.58.0**, ADR 0078). `archive.pack_report` is the fourth of the report
  family and the first whose subject is a Task that *succeeded*: the archive is
  written correctly and entirely, and it still carries entries that land
  somewhere else when extracted on another system — `a\b.txt` becomes a file
  inside a folder `a` in 7-Zip and Explorer, `f:ads` becomes an NTFS alternate
  data stream, `CON` cannot be extracted on Windows at all, and a trailing dot
  or space is silently eaten there. Our own reader round-trips all of them
  exactly, which is why neither the round-trip test nor the `unzip -t`/`tar -tvf`
  interop test can see any of it: the archive is not malformed. Both frontends
  ask for the report when a pack finishes and say so on the status bar.

  **It warns rather than refusing, and its sibling refuses.** Two entries whose
  fold keys match are still rejected outright, because extracted where they
  collide **one of the two files disappears**. This is not that: nothing is
  lost, it is placed differently — and refusing would make norte unable to pack
  an ordinary Unix tree to prevent something that is not a loss. An empty report
  is an assertion, not a silence — `entries` says how many were checked and
  `checked` says *which classes were looked for*, because `<`, `>`, `"`, `|`,
  `?` and `*` are illegal on Windows too and are not among them: a clean report
  without that list would be claiming the archive travels intact anywhere, which
  is more than anyone verified. The compatibility story is the one the handshake
  permits — a 0.57 client against a 0.58 daemon, which packs the same archive
  and simply never asks; the reverse pairing does not exist, because a
  from-the-future client is refused outright at `initialize`.

- **Permissions can be changed down a tree** (protocol 0.62.0, ADR 0083, #315).
  `fs.set_mode` changed exactly the paths it was given: a folder changed its own
  mode and not that of what it contains, which is the hole ADR 0081 left open on
  purpose. The terminal's field now takes `chmod`'s own grammar — `755`,
  `-R 755`, or `-R 644,755` — and the second mode is the one folders get,
  because `chmod -R 644` over a tree makes it unusable: without the execute bit
  you cannot even enter a directory. Absent, everything gets the same mode,
  which is what `chmod -R` does and what breaks trees. The walk stops at a cap
  and SAYS how many nodes it never reached rather than truncating in silence,
  and the entries it writes share a batch id — a hundred thousand journal
  entries nobody can join back together read as a hundred thousand actions where
  the human did one. Undoing that batch is not all-or-nothing, unlike a rename
  batch: modes are independent, so a tree where one file cannot be touched goes
  back except that one. The approval an agent's `set-mode` raises now carries
  the SCOPE as well as the mode: a recursive change over one root arrived as
  "set-mode over 1 path" while what was being approved was every descendant —
  the same hole `ApprovalDetail::mode` closed in 0.61, one size larger. Both
  frontends say it in the loudest form their surface has.
- **The AI rename plan is asked about what you MARKED** (protocol 0.62.0,
  #121). With first-class selection, marking five files and asking for a plan
  sent the directory's thousand names to the provider — more than the human
  pointed at, and the AI gate exists precisely to bound what leaves the machine.
  Marking nothing still means the whole directory. A marked name that is no
  longer in the listing is ignored rather than refusing the plan: between
  marking and asking, a file can be gone.

### Fixed

- **A panel hidden behind a tab is no longer reported as open** (#329, TUI).
  The panel bar asked the layout whether a slot *exists*, and a slot behind a
  tab that is not the active one exists perfectly well while the reader cannot
  see it. So the button lit up as open, its attention mark went quiet — the
  log's warnings stopped marking the bar exactly when nobody had them in
  front of them — and pressing it sent the keyboard to an invisible panel:
  your keys stopped reaching what you were looking at, the bar said "focused",
  and the next press closed a panel you had never seen.
  The bar now derives its state from what is on screen, and the toggles gained
  the answer they were missing. There were two questions — "does it exist?",
  which is what stops a second copy being docked, and "is it visible?" — and
  no way to say **"make it visible"**; finding a hidden panel, a toggle could
  only send it the keyboard. `Node::reveal` activates its tab in every group
  along the path, because activating the inner one while an outer group shows
  a different tab leaves the panel just as hidden and the caller believing it
  did something.
  Revealing and focusing stay ONE press, so the three states hold as they were
  (open and take the keyboard, take the keyboard, close) instead of growing a
  fourth. Closing is now the only one of them gated on the panel being on
  screen: it is the single action a reader cannot undo by looking.

- **Reopening norte no longer leaves a remote panel silently dead** (bridge
  **47**). The daemon shuts down five minutes after its last client and the
  session secret lives only in its memory (ADR 0015), so reopening later means
  the saved panel over `s3://…` comes back asking for a password. Both frontends
  swallowed that: the TUI's session restore logged a warning and left the panel
  **empty and unmarked** — a listing that failed and a bucket with no objects
  looked identical — and the window's startup listing bypassed the dialog path
  entirely, landing in an error that named no connection and offered nothing to
  press. The prompt only ever appeared if you navigated somewhere by hand, which
  meant leaving the place you were trying to get into.
  Startup still does not prompt on its own, and that is deliberate: restoring a
  session is not a request to connect, and a password asked for before the
  screen exists, for something nobody just did, is the shape ADR 0015 calls
  phishing. What changed is that the panel now says **which** connection is
  waiting and can be acted on — the TUI marks it and names it, the window shows
  the name and a retry button — and every gesture prompts: refresh included, not
  just navigation. Backspace, Delete, the arrows,
  Home/End and paste were all cancelled by the window's document-level key
  handler, which called `preventDefault()` on anything that was not a single
  printable character and forwarded it to the host, where nothing happened. A
  name you mistyped in the `mkdir` prompt could only be fixed by retyping it,
  and nothing could be pasted in at all. Barely noticeable there; disqualifying
  in the password field #327 adds, where the text is masked, the value is often
  forty random characters, and the only way out of a typo was Escape — which
  abandons the navigation. Pasting from a password manager is how most people
  answer that dialog. The rule now lives in `keys.ts`, exported and tested;
  inside the handler there was no way to test it, and it was not tested.

- **`JSON_OUTPUT` was declared and never honoured** (ADR 0088). `norte-ai` had
  all three pieces of structured output and none of the wiring: the capability
  flag, a `ChatRequest::json_schema` field documented as "honoured by a
  provider that declares it", and two providers declaring it — Anthropic for
  *whatever model happened to be configured*. Neither provider's request
  builder read the field, and the only caller, the AI rename plan, set it to
  `None` while asking for JSON in prose. Nothing failed, because the local
  validator has always done the real work; that is exactly why it could sit
  like that. A declared capability that nobody honours is worse than an absent
  one — absent, the core knows to be careful.
  The contract now travels, mapped to each provider's native mechanism:
  `output_config.format` for Anthropic, `response_format` with `strict: true`
  for OpenAI-compatible. Ollama declares neither and sends neither — it was
  already honest, and is now the *tested* fallback. The Anthropic capability
  follows the **model** rather than the vendor, by the same predicate that
  gates the request body, so the declaration cannot drift from what is sent.
  Fixtures assert the exact bytes on the socket, not the behaviour.
  The schema deliberately carries no `minLength`, `pattern` or numeric bounds —
  Anthropic rejects a schema containing them — and a test forbids them. That
  limitation makes the security boundary plain: **the rules that matter cannot
  be expressed as a shape.** That every `from` exists, that `to` is a basename
  with no traversal, that no destination is duplicated or collides with a file
  that is not itself being renamed — `{"from":"a","to":"../x"}` satisfies the
  schema perfectly. So the local validation is unchanged and still runs on
  every reply, and a test feeds it hostile envelopes to keep it that way.
  Replies parse in both shapes, so no provider breaks by not supporting the
  contract. And each exchange now logs provider, whether the contract actually
  travelled, entry count, bytes, milliseconds and an outcome category — never
  the instruction, the filenames, the reply, or an error's text.

- **Filenames leaked into the daemon's log on every failed AI plan** (ADR
  0088). Found by the security review of the change above, and older than it:
  `ai_to_proto_error` logged the error's `Display` for unmatched variants, and
  a protocol error from the rename validator embeds the fragment that caused
  the rejection — a name the model wrote, or, in the collision case, **a real
  filename from the user's directory**. It went out at WARN every time a plan
  was rejected, so anyone with the log or a diagnostic bundle had them.
  Protocol errors now log their category and nothing else. HTTP errors still
  log their text: a provider's status line has never seen a filename.

### Added

- **The protocol has a catalogue, and forgetting a surface now turns the gate
  red** (ADR 0089). Adding one of the ~70 methods means touching the constant
  and its types, the daemon's dispatch, the remote client, the notification
  routes, both backends, the schema aggregate, the goldens, MCP or the
  frontends. None of those is redundant and the daemon's flat dispatch is
  deliberate — the problem was never that there are many surfaces, it is that
  **forgetting one did not show**. A method the daemon serves and the SDK
  cannot call is invisible to every window.
  `norte_proto::catalog` now lists every method with its kind, shape and types,
  and six gates check it: every constant is catalogued and every entry still
  exists, the daemon dispatches every request and does *not* dispatch a
  notification, every notification is emitted, the remote client can ask for
  everything (minus exceptions that must carry a written reason), the types are
  in the schema aggregate, and the declared shape matches what the daemon does.
  The wire is untouched: no serde type moved, no constant changed value, no
  golden regenerated, `PROTOCOL_VERSION` stays at `0.63.0`.
  It found a real gap on its first run — `policy.grant_scope` is served by the
  daemon and absent from the SDK. That turned out to be deliberate (a scope is
  granted from the terminal, with the daemon's low-level client), but *nowhere
  did it say so*. Now it does.
  The catalogue also gets a golden, which is the part that protects most:
  nothing guarded the wire's method names before. The published schema carries
  types, not methods, and the only golden holding method names has four of
  them — so deleting `fs.rename_batch`, a textbook wire break, turned nothing
  red beyond its callers failing to compile.

- **The graphical window is a supported frontend** (ADR 0087). `norte-gui` was
  built as a spike with a go/no-go at the end, and had long since acquired
  everything a frontend needs — the semantic host behind it, a versioned
  bridge, a TypeScript renderer with 125 tests, a restrictive CSP, a boundary
  test, and packaging that ships it with the `norte` and `ntc` binaries so a
  clean install has a daemon to talk to. What it did not have was anyone
  admitting it: the application id was `dev.norte.gui.spike`, the README said
  in bold that there is no graphical interface, and CI never ran its gate.
  Running that gate is what settled the question. `just gui-ci` was **red on
  `main`**, in two unrelated ways, and had been for weeks: `boot()` had grown
  past the `too_many_lines` threshold, and a test in the window's crate no
  longer compiled because `UiHostOptions` had gained a field months later. A
  gate that depends on someone remembering is not a gate.
  So it has one now: `.github/workflows/gui.yml` installs WebKitGTK, GTK3 and
  libsoup3 — only in that job — and runs `just gui-ci` on every change to the
  window **or to what goes into it**: `norte-ui-host`, `norte-client`,
  `norte-frontend`, `norte-proto` and the shared build configuration. Those
  four are on the list precisely because of the failure above: a change there
  can break the window while the portable gate stays green.
  The crate still sits outside the portable gate, for the opposite reason than
  before: not "until the spike closes", but because requiring a browser engine
  to test `norte-vfs` would make that gate unrunnable on machines with no
  business having one. The identifier drops `.spike` — done now, in alpha,
  because an id change means an old install sits beside the new one rather
  than upgrading, and after a stable release that is a migration.
  And the half of the packaging promise that could only be checked by
  installing is now a test: that the window **resolves** its sibling `norte`
  binary at startup, falls back to `PATH` without one, and does not try to
  launch a *directory* named `norte` — which would have surfaced as "could not
  connect", saying nothing about the real cause.
  Still open, and named rather than implied: accessibility, IME and fractional
  scaling (#261) need a person in front of a screen, and the clean-machine
  smoke test and old-glibc baseline remain from phase 7.1.

### Changed

- **The window's controller was one 18 725-line file** (ADR 0086), and 16 203
  of those were a single `impl Estado` holding 422 methods: navigation, tabs,
  search, dialogs, the viewer, sync, tasks, plugins, agents, profiles and
  sessions, all in one block. The single-writer design was never the problem —
  one mailbox, one task, one order, exactly as ADR 0066 promises — but one
  writer does not require one file, and the conflation was paid on every
  change: reading any capability meant paging past all of them.
  It is now `controller/mod.rs` (the mailbox, the actor, `Estado` and the
  action dispatch) plus 32 modules, one per capability, the largest 1 484
  lines. Not a method body changed; that is checked and not claimed, method by
  method, character by character, against the file it came from.
  The move opened all 406 relocated methods to `pub(super)` — visible inside
  `controller`, invisible outside it. Nothing else moved: `UiAction`,
  `UiUpdate`, the snapshots, the bridge and its goldens are untouched, and the
  dependency-boundary test still holds.
  Two things fell out of doing it. About **250 methods are called across the
  new boundaries**, which says plainly that these capabilities are not
  independent slices — the dispatcher and the views reach nearly everywhere.
  And the Fluent-key sweep in `catalogo_del_host.rs`, which read five named
  files, now walks `src/` instead: its own header warned that a hand-written
  list separates from the code at the first new surface, and this was it.
  Widening it found a file it had never looked at.

### Fixed

- **The window's tests bet on the clock 111 times** (ADR 0085). Every wait in
  `norte-ui-host`'s controller suite was a `tokio::time::sleep` of 20 to 400 ms
  followed by an assertion — a guess about how long a machine takes, placed
  while nextest runs 423 tests in parallel. Sixty of them had no retry at all,
  so when the guess lost, the failure landed on an assertion twenty lines away
  from the cause. The rest retried on a one-second budget of wall clock that a
  loaded machine can exhaust with nothing broken. Both shapes are how a suite
  teaches people to re-run a red test instead of reading it, and this
  repository's rule is that an intermittently red test is a bug.
  They are gone: 111 → **0**. The test double now announces what it records, so
  a test waits for the event it is about (`hasta`), proves a negative by
  letting the executor drain rather than by sleeping (`asentar`), or reads
  snapshots until the screen says what it should (`foto_hasta`) — and every
  wait names what it was waiting for when it gives up. Real deadlines use
  `start_paused` and skip the wait instead of serving it. The four sleeps left
  in the double are the latency it *simulates*, and it now counts requests and
  answers so a test about a LATE response waits for that response to arrive
  instead of proving nothing by not having waited long enough.
  Measured before and after, because the usual justification for this work does
  not hold here: the suite was never slow on their account. 423 tests took
  15.1 s and all ~376 controller tests together were 3.9 s of CPU; it is now
  13.2 s. What was bought is trust, not seconds. Workspace-wide the count went
  227 → 117; the rest (`norte-core/tests/daemon.rs` has 19) can have the same
  treatment.

- **An empty secret quietly borrowed someone else's credentials** (#320). Set
  `NORTE_SECRET_<CONN>` to the empty string — a mistyped `read`, an unset
  variable exported anyway — and the connection did not fail: opendal discards
  an empty `secret_access_key` without a word, so no static credential was
  registered and S3 authenticated with whatever the ambient chain offered.
  On the machine where this was found that was an expired SSO profile and the
  error blamed the provider; on a machine with a working role it would have
  connected as the wrong identity and said nothing at all. The resolver now
  rejects an empty secret and names both the connection and the step that held
  it (env, keyring or `secrets.age`), and the S3 connector repeats the check
  where the hazard actually is — because the empty-drop is not specific to the
  secret. `access_key_id = ""` gates the same static provider and reproduces
  the bug on its own with a perfectly good secret; `endpoint = ""` and
  `region = ""` are discarded the same way and silently retarget the request at
  AWS, so a user who believes they configured an internal MinIO ships the
  bucket name, the key id and a signature to Amazon. All four are refused now.
  A whitespace-only secret still goes through: that one reaches the server and
  comes back as a credential rejection, which is an answer. `auth = "key"` is
  deliberately exempt — there the secret is a key passphrase, empty means the
  same as absent, and there is no ambient credential behind it to substitute.
  `norte doctor` gained the matching distinction and grades it `Error`, not
  `Warn`: a variable that is set but empty was reported as `Ok`, which is the
  same lie in the one place meant to catch it, and a preflight that exits 0 on
  a connection that cannot work is worth nothing. The same check now covers a
  variable holding bytes that are not UTF-8 — the doctor read those with
  `var_os` and called them present while the resolver, reading with `var`,
  never saw them at all. The neighbouring overclaim is recorded in ADR 0015 and
  tracked as #321: `disable_config_load` is `no_env()` + `no_profile()` in
  opendal 0.58, so SSO, web-identity, process and ECS stay in the chain; what
  keeps explicit credentials deterministic is that the static provider is
  pushed to the front of it. What the failure says still does not reach a
  frontend — every credential failure collapses into `PermissionDenied` and
  nothing fills the RPC `message` — which is #322.
- **The CLI answered in Spanish whatever your language was** (#319), for the
  last twelve messages that still carried their text inside the code. They were
  startup warnings and empty-result notes — each one arrived alone, in a change
  about something else, which is why nobody caught the set. Seven keys cover
  the twelve: three pairs said the same thing in the embedded CLI and in the
  daemon and now share a key.
- **A RAR4 archive with an OEM-code-page name can finally be tested** (#223).
  Names in RAR5 are UTF-8 by format, so the existing forge could not write what
  a decade of downloads actually contains, and nothing here could produce one.
  It can now, and that measured something worth knowing: on such a name `7z`
  hands the raw bytes back untouched, while `unrar` maps them into a
  private-use range — a *different* failure from the truncation already known
  for non-UTF-8 RAR5 names, and one more reason `7z` is the preferred delegate.
  That preference had until now only been measured over RAR5.
- **A git submodule was reported as deleted** (#225). Its index entry is a
  gitlink whose path is the DIRECTORY, so the exact lookup found it and the
  comparison then saw a directory where the index said file: `D`, on a
  perfectly healthy submodule. It now reports nothing at all — knowing whether
  a submodule has changes means opening the repository inside it, which is the
  same boundary that keeps staged status out of v1. Saying nothing is honest; a
  mark would be a claim about something never looked at.
- **Semantic search stopped materialising the whole index to answer** (#122).
  It scored every stored vector into a second full-length list, sorted all of
  it, and threw away everything past the hundred asked for; now a bounded heap
  keeps only those, and the query's own norm is computed once instead of once
  per file. Ties break by path, so the same question gets the same answer twice
  instead of whatever order SQLite happened to return. `index.embed`'s
  pre-check asks "is there anything to embed?" rather than building the entire
  candidate list to look at its length, and a provider that returns a
  zero-dimension vector is refused instead of stored — stored, it marked the
  file as embedded and no later run would retry it. An impossible negative size
  now reads as "unknown" on both read paths rather than "empty file" on one of
  them.
- **A rename that only changes the spelling was refused — and with
  `Overwrite` it destroyed the file** (#274). On a folding volume (APFS, NTFS,
  exFAT, an ext4 `+F`) `Foo.txt` and `foo.txt` are the same node, and norte
  renames WITHOUT replacing, so the destination "already exists": the reader
  got "there is already something there" about the file they were renaming, and
  no way forward — the window has no collision-retry dialog. Worse, the
  `Overwrite` arm then deleted the destination — which IS the file — and
  renamed something that was no longer there. When the collision turns out to be
  the same node under another spelling, norte now performs the rename through an
  intermediate name instead of applying a collision policy. Identity is not
  enough on its own to decide that: a hard link shares an inode, so the leaves
  must also fold to the same key. The window between the two renames is
  deliberately NOT cancellable — the repo reads a cancelled task as "the tree is
  as it was" — and undoing such a rename needed the same detour, because the old
  name "is occupied" by the file itself.
- **Full case folding ignored the default-ignorables** (#214). `FoldMode::Full`
  is meant to be what an ext4/f2fs `+F` directory does, and the kernel builds
  its tables as `nfdicf` — NFD, **i**gnore default ignorables, case fold. norte
  kept them, so `nombre.txt` and `nom<U+00AD>bre.txt` had different keys and
  norte answered "these do not collide" about the one filesystem #145 is about:
  a plan approved with no warning that dies mid-batch. The predicate now shares
  the table the invisible-painting already used — one table, two policies:
  painting exempts ZWJ and the variation selectors for emoji fidelity, folding
  cannot exempt anything because the filesystem does not. **This widens what
  `archive.pack` refuses**: it folds with the widest mode on purpose ("would
  these collide on ANY machine?"), so two entries differing only by a variation
  selector or a ZWJ are now one name and the archive is refused rather than
  built. Simple folding (APFS, NTFS) is unchanged; HFS+, which also drops
  ignorables, remains a documented gap.
- **The editor was launched with an unresolved program name and the browsed
  directory as its `cwd`** (ADR 0082, #302). On unix `current_dir` is applied
  BEFORE the program is resolved, so `EDITOR=vim` with a `.` — or an empty
  component — anywhere in `PATH` executed a file called `vim` out of the
  directory the reader had just walked into: extract a hostile archive, enter
  it, press F4. Every child the TUI launches with a `cwd` now gets the absolute
  path that `openers::resolve_program` found (it skips relative and empty
  `PATH` entries), and a program that cannot be resolved is NOT launched —
  falling back to the bare name would hand the lookup back to `execvp` with the
  directory already changed, which is the hole itself. The `$SHELL` guard is
  deliberately not copied over: a relative `$EDITOR` is normal and a relative
  `$SHELL` is not, so the guard belongs at the launch and not at the variable.
  The same rule now covers the RAR delegate, whose `PATH` walk took relative
  entries and whose working directory was `norte-rar-<pid>` under the system
  temp, created with `create_dir_all` — which succeeds on a directory that
  already exists, whoever owns it. It is a randomly named, exclusively created
  0700 directory now.
- **The name norte creates was announced before the editor opened it** (ADR
  0082, #303). norte announces the name by creating it — nothing to guess — and
  anyone who can write in that directory could unlink it and leave a symlink
  there before the editor started, so the reader typed into a file they were
  never shown; `undo` of the `Created` entry works by path, so undoing would
  send whatever is there NOW to the trash. Both frontends now ask what is at
  that path before launching (`fs.stat`, which is `lstat`: it describes the
  link, never its target) and refuse anything that is not a regular file. The
  question is asked where the LAUNCH happens — the TUI carries the path on the
  pending suspension and the run loop asks immediately before yielding the
  terminal — because asking where the gesture is resolved leaves the panel
  re-listing inside the window it was meant to remove. This NARROWS the window
  rather than closing it: between the `stat` and the `exec` a gap remains.
  Failing to ASK (a relieved daemon, a timeout) says so in its own words rather
  than claiming the file was tampered with.
- **The embedded backend sent no destination anchor** (ADR 0082, #301). The
  anchor of ADR 0073 was only ever filled by the SDK, over the wire — and `ntc`
  runs embedded by default, so every anchored operation from the TUI ran with
  no anchor, including the one ADR 0076 justified the anchor WITH: `fs.create`,
  whose success hands a path to `$EDITOR`. `Backend::Embedded` now remembers
  the anchor of each directory it lists and passes it to `create_file`, `copy`
  and `move`, so a destination replaced between the listing and the write is
  refused with `Conflict{EscapesRoot}` whether norte runs embedded or against a
  daemon. A destination nobody listed still behaves as it did in 0.53. Only a
  PANEL's listings are recorded: the tree sidebar and a script's `fs.list` would
  otherwise re-bless the panel's anchor with whatever they saw, and an anchor
  says who looked. Eviction is LRU in both the embedded cache and the SDK's,
  which had the same bug: with FIFO, the directory you have open was evicted
  once 64 different directories went past — an expanded tree does that alone —
  and the check silently stopped happening.
- **The orphan cap could overflow the session envelope on its own** (#304).
  `prune` trims by COUNTS and the real limit is BYTES: 128 orphan slots with
  full history serialised to ~1.18 MB against the 1 MiB `SESSION_BODY_MAX`, so
  the core refused the `put` and left the stored session as it was — the window
  lost where the reader was without a word (the TUI at least truncates, retries
  and warns). An orphan slot — one no arrangement mentions, which nobody can
  press "back" inside without reopening it first — now keeps 8 steps of history
  per direction instead of 64, and a VISIBLE slot loses nothing. Since no count
  can promise a byte limit — the number of visible slots has no cap and the
  reader picks the paths — `prune` now ends by measuring the serialised body and
  degrading in a declared order until it fits: orphans whole, then visible
  history halved, then the arrangements of non-active profiles. The active
  profile, its arrangement and every visible slot's path and cursor are never
  touched. And the window now degrades and retries the way the TUI already did
  (#316) — the decision of WHAT to drop is one shared function, because a
  decision duplicated between frontends diverges in silence (ADR 0077).
- **The anchor cache is no longer a field the daemon merely promises not to
  touch** (#317). It is installed by `embedded::engine_in`, the constructor for
  a frontend's engine, and the daemon builds its own by another route — so an
  engine that serves many clients cannot pass one client's listing to another
  client's write, whoever mounts a `Backend::Embedded` on it. The rustdoc said
  so before; now the type does, and two tests pin both halves.

- **Five layout commands had no key in any preset.** Closing a slot, growing
  and shrinking it, equalising the row and designating the destination were
  reachable only from the menu or the palette — which is where "the keys in the
  menu are wrong" started, since the menu honestly painted `—` for all five.
  Designating mattered most: with three or more panels a gesture now says
  "designate a destination first", and there was no key to do it with. They are
  now `alt+x`, `alt+[`, `alt+]`, `alt+=` and `alt+g`, the SAME chords in all
  seven presets — a layout key should not move because you changed preset — and
  in `[global]`, which is what lets a side panel accept them too. No original
  manager had these concepts, so the chords are chosen rather than transcribed,
  and `alt+g` does not pretend to a mnemonic it lacks.

- **A panel gesture from the third panel did nothing, silently.** `pane.pull`,
  `pane.mirror` and `pane.mirror-target` worked out "the other panel" as
  `focus ^ 1`, which is a count of TWO — and panels have been splittable for a
  while. With three, from the last one that lands on the panel ITSELF (the
  lookup clamps), so the gesture compared a location with itself and returned
  "nothing to do" without a word: from the outside, pulling only worked left to
  right. They now ask the same thing every other destination asks — the panel
  holding the Target ROLE (ADR 0058 D7) — and when three or more panels are
  open with none designated they SAY so instead of going quiet. The comparison,
  the sync roots and the read-only gate for the destination were computing the
  same wrong "other" and now share that one answer.
- **`orthodox` bound `alt+n` twice**, to `pane.disconnect` and to
  `pane.tab-next`. A chord in one screen names one command: the second silently
  won, so "next tab" lost its key and showed as `—` in the menu, the palette
  and the reference sheet — which is what "the keys in the menu are wrong"
  turned out to be. Disconnect moves to `alt+d` (free here, and what `krusader`
  already uses); next tab keeps `alt+n` and its pair with `alt+p`. A test now
  fails the build if any preset binds one chord to two commands in the same
  section — the effective map cannot see it, because by then one has already
  replaced the other.

- **The settings screen was missing four keys, and one of the ones it had
  could not be turned off.** `[ui] editor` and `editor_detached` are now rows
  of their own (a command line is typed as text and stored as the ARRAY the
  file declares — writing it as a string would make the next load reject it),
  and so are `parent_entry` and `show_hidden`: neither has a command or a key,
  so the file was the only place they could be changed — `pane.toggle-hidden`
  moves the session and persists nothing. And `ui.menu-bar` was in the catalog
  with no arm in `current_value`, so its cell rendered EMPTY: not cosmetic,
  because toggling reads the painted value, so it read "not true" and wrote
  `true` every time — the menu bar could not be switched off from the screen it
  is offered on. The test that should have caught it accepted an empty cell;
  now a toggle row must paint a real boolean.

- **Enter on a file opens it, instead of doing nothing at all.** `nav.enter`
  only ever answered for directories, symlinks and archives — over a plain
  file, binary or not, the key did nothing and said nothing. It now hands the
  file to the program its mimetype names in `openers.toml`, and to the
  desktop's own launcher when no rule matches, which is what every manager in
  this family does; `pane.view` keeps the internal viewer on its own key. On a
  pane that is not on this disk there is no native path to hand over, so it
  falls back to that internal viewer — the only thing that CAN be done there,
  and better than the error it would otherwise be. The window did the same
  nothing, only louder: it answered "opening files is not built yet", a note
  from a task that never landed.

- **Splitting the same way twice divides evenly, and a split that will not fit
  says so.** `alt+v` three times used to leave 1/2, 1/4 and 1/4 rather than
  thirds: each split wrapped the slot in a NEW split instead of joining the one
  already running that way, so every press took half of a half. Press it once
  more and the deepest child fell under its kind's minimum, the layout degraded
  that split to tabs for the frame, and the panel just asked for **vanished
  with no message** — the tree kept it, so what you saw and what existed
  disagreed. From outside, one key that sometimes split, sometimes did nothing,
  and sometimes looked like it undid the last one. Splitting along an axis that
  is already running now joins that split, so N panels are N equal shares
  (a FIXED-size slot still splits inside itself: its size is docked chrome, and
  a new sibling in that row would steal room from what sits beside it). And
  splitting refuses when the focused slot no longer fits two, saying "no room
  for another panel here" — the same arithmetic that decides the collapse,
  asked before the tree is touched, against the rectangle the LAST frame
  painted. Both frontends: the rule and the refusal are shared, so they cannot
  drift apart (ADR 0077).

- **Splitting a panel no longer smuggles the parent directory into the
  listing.** Splitting a panel, and opening a tab, inherit the neighbour's
  listing so the new panel appears filled instead of blinking empty — but what
  they copied was `entries()`, which carries the synthetic `..` row. The new
  panel then added its own, and the inherited one stayed behind as an ordinary
  entry: painted with the PARENT's name, sorted among the directories, and
  markable. Each split added one more. `ctrl+a` in a panel split once therefore
  marked six things where the neighbour marked five, and the sixth was the
  directory above — which F5, F6 and F8 would then act on. The listing survives
  a refresh keyed by path, so the mark came back on the `..` row itself even
  after the ghost was gone. Copying now goes through `real_entries()`, and both
  callers through one `App::fork_pane` rather than three lines repeated in
  each; as a net underneath, the mark set refuses the parent's path at the one
  door every marking path now shares. Checking the path is safe THERE and still
  is not in `is_parent_row`: an entry of this directory is always `dir/name`,
  so only the synthetic row can be exactly the parent, while a link or a mount
  that points at it has a path of its own. The window was never affected — it
  re-lists on split instead of copying.

- **The directory tree answers the mouse.** Its cells belong to no listing, so
  a click on them landed in "outside the panes" and did nothing: a panel that
  was painted and could not be touched — the same hole the places sidebar had
  in #226, and the fix is the same one. A click on a row selects it and brings
  the keyboard; a second click on the same row activates it, which is what
  `Enter` does — expand the branch and send the focused listing there, through
  the ordinary `cd` flow. A click on the MARK (`▾`/`▸`/`·`) folds or unfolds
  that branch in a single press: it is what the arrow already says, and it is
  the one thing the mouse could not otherwise do, since `Enter` expands and
  navigates but never folds. The tree's scroll offset is now computed beside
  the painting instead of left to the widget, so the hit test reads the same
  number the frame drew.
- **The menu bar survives a side panel.** With the keyboard inside the tree,
  the places sidebar or the process panel, `alt+m` did nothing: `app.menu` was
  in none of their allowlists, so the panel ate the key and the screen stayed
  put. The menu bar is application chrome, not the listings', so it is now
  dispatched by one funnel the three panels share rather than by a copy in each
  — three copies are three places to forget the fourth. It is the same lesson
  that already brought `layout.places` and `pane.switch` into those lists.

### Added

- **Checksums over the wire** (#311, protocol **0.59.0**): `fs.checksum`
  computes the sha256 of the CONTENT of a batch of files as a cancellable task,
  and `fs.checksum_report` hands back the digests. Checking a download against
  the sum somebody published is the only way to know it is what was offered, and
  norte had no way to do it; Krusader puts it in its File menu. Two methods and
  not one because N digests fit neither in a task's outcome nor in its progress,
  which only counts — the same split, for the same reason, as
  `fs.rename_batch_report` and `archive.pack_report`. It reads and writes
  nothing: no journal, no undo. A file that cannot be read comes back with its
  REASON instead of killing the batch, a directory is flagged rather than walked
  (hashing a tree is a different question, with its own format), and the report
  keeps the order you asked in — one that reordered itself could not be compared
  against the list you sent, and it names the algorithm it used because a report
  can be fetched without having sent the request. A batch is capped at **4096
  paths** and is REJECTED above it rather than truncated. It goes through the
  NARROW gate — content, not just read (ADR 0080): a digest is a fingerprint of
  the bytes, so this subsumes the `fs.compare` hash oracle and exceeds it, and
  an agent denied that one only had to call here.

- **An approval says WHAT is being asked, not just which op** (#314, protocol
  **0.61.0**). The request a human answers carried the op and the paths, and for
  every op but one that is the whole decision: approving "copy these twelve" is
  approving copying those twelve. Changing permissions is the first where two
  requests with the same op and the same paths mean opposite things — `0600` and
  `4777` — so the human was answering without the half that decides the harm.
  `ApprovalDetail` carries the mode, and both frontends show it: the terminal on
  its own line, the window beside the op.

  Additive: the field is omitted when it says nothing, so the JSON of every
  other op does not change a byte, and it rides in BOTH shapes — the
  notification and the `policy.pending` resync — because a pending rebuilt after
  a reconnect showing less than the notification that announced it is how a
  human ends up deciding with less. Setuid and setgid stay out of an agent's
  reach anyway: the question that would authorise them is only asked when a rule
  says `ask`, and a rule that plainly allows `set-mode` shows nobody anything.

- **Checksums and permissions reach the window** (#311, #314). Both were
  classified as deferred in the parity test with the same reason — the protocol,
  the core and the rules were shared, what was missing was the surface — and now
  neither is. `pane.chmod` opens a dialog with the octal field prefilled from the
  entry under the cursor and the count of what it will change; `pane.checksum`
  and `pane.checksum-verify` launch the task, wait for its REPORT and show a row
  per file, with copying the list as the only thing a list of digests is for.

  The rules stayed where they were: the same `parse_mode`, the same verdicts, and
  the resolution of a sums file's names against its own directory moved into the
  shared crate so the two frontends cannot drift on what `sub/dentro.txt` means.
  A partial report is not compared in the window either — a cancelled batch would
  otherwise accuse files nobody read. And the report is requested both when the
  task ends AND when it is born already finished, which for three small files is
  the common case: without that, the fast path showed nothing.

- **Permissions can be changed** (#314, protocol **0.60.0**, ADR 0081). It was
  the one category where all three reference managers touch and norte only
  looked: the properties dialog showed the POSIX mode and nothing could change
  it, because the protocol had no method. `fs.set_mode` sets the twelve
  `chmod(2)` bits of N paths as a cancellable task, and `pane.chmod` is its
  surface — an octal field prefilled with the mode of the entry under the
  cursor, over the usual operand, with the COUNT in the title, because typing a
  mode believing it applies to one entry and having it apply to fifty is the
  mistake the dialog exists to make hard.

  It is a MUTATION, with everything that drags along: its own policy op (letting
  something create files is not letting it change who can read them), and a
  journal entry whose **reversal is the previous mode**, read immediately before
  the new one is written — so undo works. When that previous mode cannot be read
  the change still happens and the entry says it is irreversible, rather than
  storing a mode nobody had for a later undo to apply. One entry per path, so a
  batch that stops halfway leaves undone exactly what it did.

  Permissions only, and not "attributes": timestamps and ownership are different
  questions with different privileges and different reversals, and each will
  arrive with its own method or not at all. Bits above the permission mask are
  rejected rather than masked — masking would quietly apply a permission nobody
  asked for. Not recursive: it changes the entries you give it. Where there are
  no POSIX permissions — inside a `.zip`, an object bucket, Windows — the new
  `POSIX_MODE` capability is absent and the call answers `Unsupported` instead of
  pretending. Far's `Ctrl+A` now binds what its own source calls it, "Set file
  attributes"; it used to land on properties because looking was all norte could
  do.

  Three things the review pass changed, and they are the interesting ones. A
  **symlink is skipped** rather than changed: `chmod(2)` follows the link while
  the stat that reads the reversal does not, so the "previous mode" would have
  been the link's `0777` and an undo would have left the TARGET world-readable —
  and the target can sit outside the subtree somebody approved. The **undo asks
  for `set-mode`**, not `delete`: the first version let it fall into the delete
  bucket, which meant an actor holding `delete` could undo a chmod the policy
  does not grant it, and one holding `set-mode` could not undo its own. And
  **setuid/setgid are refused to agents**, not because those bits are the danger
  but because the approval request carries the op and the paths and NOT the
  mode, so nobody could see which they were consenting to; a human sets them
  from a dialog that shows them.

  Note for anyone with a `policy.toml`: a rule with no `op` is a wildcard, so an
  existing "allow everything under this prefix" rule now also allows permission
  changes there. Rules that name their `op` are unaffected.

- **Three more ways to mark** (#313), the gaps Total Commander's Gray family
  has and norte did not — and which our own transcription of its keyboard file
  listed among the omissions, for want of a command to bind. `mark.extension-add`
  marks everything sharing the extension of the entry under the cursor, and
  `mark.extension-remove` unmarks it; `mark.files` and `mark.dirs` mark by kind,
  additively, like `mark.pattern-add`; and `mark.restore` brings back the
  selection from BEFORE the last bulk gesture — the net for whoever pressed
  "clear all" by mistake.

  The extension is the tail after the LAST dot, in bytes: `.bashrc` has none —
  it has a name — and two names that would collapse to the same replacement
  character keep different extensions. That is the same rule the template rename
  already uses, deliberately: two definitions of "the extension" would mark one
  set and rename another. Restore keeps ONE snapshot per pane, taken by every
  bulk mutator, and it goes both ways, because whatever rescues a mistaken
  "clear all" has to rescue a mistaken "restore" too; a `cd` drops it, since
  those paths no longer name anything in the listing. All five land in the
  window as well as the terminal — the rule lives in `PaneState`, so neither
  frontend decides anything. `alt+plus`/`alt+-` are the chords Total Commander
  attests for the extension pair, and `/` its "restore selection"; Far's own
  `Ctrl+M` restores there. The three native presets take all five.

- **Compare two FILES** (#312). Comparing two trees has been there since the
  directory-comparison spec; the pair — which Total Commander and Krusader both
  have — was missing. `pane.compare-files` takes two marked in the focused pane,
  or the one under the cursor here and the one under the cursor in the target
  pane, and hands them to the program named in `[ui] diff` (`%F` is both files,
  `%d` the pane's directory). With nothing configured it is `diff -u`, which
  POSIX guarantees, and its output is held on screen until a key is pressed —
  the honest equivalent of what `xdg-open` does for `pane.open`. `[ui]
  diff_detached` says the tool opens a window of its own, and neither key is
  read from the PROJECT layer, for the same reason as `[ui] editor`: they name a
  program to execute.

  It is TWO files or nothing. Three marked, one, the same file on both sides, or
  a folder among them, and the command says so instead of comparing something
  nobody chose — a diff of a file against itself reads as "they are identical"
  when what happened is that only one thing was selected. Both must be on this
  system: an external differ cannot be handed an `sftp://`. The operand rule
  lives in the shared frontend crate, so the window inherits it when it grows
  the surface. `alt+C` in the three native presets; deliberately unbound in the
  four imported ones, whose sources give the feature no chord — the same
  fidelity rule that already leaves `pane.compare-dirs` unbound in `far` and
  `norton`.

- **Checksums in the terminal** (#311). `pane.checksum` sums what is marked —
  or the entry under the cursor, the usual operand — and shows the list;
  confirming copies it in `sha256sum` format, which is what goes into a
  `SHA256SUMS` and what every other tool reads (verified round-tripping through
  GNU `sha256sum -c`). `pane.checksum-verify` walks it back over the sums file
  under the cursor: it parses the lines as BYTES, resolves each name against the
  directory of THAT FILE rather than the panel's — a `SHA256SUMS` speaks about
  what sits beside it — and shows ok / MISMATCH / missing per line, with the
  count in the status bar. The parser lives in the shared frontend crate, so the
  window inherits it when it grows the surface (#311 stays open for that).
  Waiting for the report is spawned rather than awaited in place: a hundred
  large files would otherwise leave the terminal undrawn and uncancellable,
  which is exactly when somebody cancels. A task whose answer IS its report no
  longer gets the tick's generic "done" on top of its verdict, and the modal
  offers the copy key only when there are digests to copy. The keys are chosen
  rather than transcribed — `alt+k` and `alt+K`, free in all seven presets —
  because none of the four imported managers gives checksums a chord.

  Everything a checksum surface can get wrong, it gets wrong QUIETLY, so the
  review pass on this one is worth listing: a **cancelled** batch used to be
  compared anyway and reported "N do not match or are missing" about files
  nobody had read — now a partial report is named as partial and compared
  against nothing. Lines the parser did not understand are **counted**, and any
  number above zero forbids saying "all ok" (37 checked out of 40 is not 40
  ok). Three kinds of line it did not understand it now reads: coreutils' own
  `\`-escaped form for names with a backslash or a newline, the BSD `--tag`
  form, and a leading UTF-8 BOM — and a UTF-16 sums file is named as such. The
  sums file is refused above 1 MiB instead of being read halfway. What goes to
  the clipboard is BYTES with that same escaping, so a non-UTF-8 name is copied
  as itself rather than as replacement characters, two different names cannot
  collapse into one line, and a name with a newline cannot inject a forged
  entry. Names with a directory in them (`sha256sum -r`) resolve segment by
  segment instead of being dropped. "Missing" split into four verdicts, because
  not-there, not-allowed, it-is-a-directory and this-system-cannot-spell-that
  are fixed in four different ways. And the list scrolls, which it did not:
  forty files with the bad one at row twelve showed five "ok" and no way to
  reach it.

- **Batch rename WITHOUT a language model** (#310). The batch machinery has
  been there since ADR 0042 — reviewable plan, `plan_hash`, collisions,
  journal, undo — and the only thing that knew how to produce a plan was
  `ai.rename_plan`, so renaming twenty files needed an LLM. `pane.rename-batch`
  asks for a TEMPLATE instead: `[N]` the name without its extension, `[E]` the
  extension, `[C]` a counter (`[C3]` zero-padded), everything else literal. It
  acts on what is marked, or on the entry under the cursor, and it produces the
  same reviewable plan through the same path — what makes the operation safe is
  not where the names came from. A template that changes nothing says so; one
  that would leave a name empty or with a `/` in it is refused in the dialog,
  with the reader there, rather than three steps later by the daemon. The keys
  are the attested ones where they exist: `ctrl+m` in `total-commander` (its
  Multi-Rename Tool) and `shift+f2` in `krusader` (Krename), both of which had
  been sitting in those files' omission lists for want of a command to bind.

- **The editor is configurable in norte, and it can be a window.** `[ui]
  editor` takes an argv template with the same field codes as `openers.toml`
  (`%f` the file, `%d` the pane's directory) and overrides `$VISUAL`/`$EDITOR`,
  which until now were the only way to choose what `pane.edit` opened. Beside
  it, `[ui] editor_detached` says the program opens a window of its own, so
  norte hands it the file and stays put instead of suspending the terminal
  until the reader closes something on another screen — norte cannot tell a
  terminal editor from a windowed one, so the config says which it is. Opener
  entries take the same `detached` flag, for the same reason: until now every
  declared opener suspended the TUI. Neither key is read from the PROJECT
  layer: they name a program to execute, and a repository you cloned does not
  get to choose what runs when you press a key — the same fail-closed line that
  already keeps `[daemon]` and `openers.toml` out.

- **`pane.mirror-target`: send the folder under the cursor across.** Krusader
  binds `Ctrl+←`/`Ctrl+→` to it — "on a folder: refreshes the [other] panel
  with the contents of the folder; on a file: the [other] panel gets the same
  path" — and the krusader preset had left both keys unbound, because norte had
  the second half only: `pane.mirror` sends this LOCATION and knows nothing
  about the cursor. It is a new verb rather than a smarter `pane.mirror`
  because four presets bind that one as "send this location", and teaching it
  to prefer the cursor would change, in silence, a key those readers already
  use. Which directory it aims at is decided once, in `PaneState::target_dir`,
  so both frontends answer the same (ADR 0077); over the `..` row it sends this
  location, never the parent's, because that row is the operand of nothing.
- **The krusader preset gets its own drive key.** `Ctrl+Shift+←`/`Ctrl+Shift+→`
  are the per-side media list, and they were omitted at transcription time as
  part of MountMan, reasoning that Linux has no drive letters. The media list
  is not MountMan, and norte grew exactly that picker in #131 — the same
  `pane.select-drive-left`/`-right` the other three imported presets bind at
  `Alt+F1`/`Alt+F2`.

### Changed

- **Adding a favourite proposes its name.** The path was already inferred from
  the pane; the name asked you to type by hand what the path already knew. The
  field now opens with the directory's own name — the host at the root of a
  remote, `/` at a local one — sanitised the way every painted name is, because
  a favourite's name is a LABEL and the destination travels separately. It is
  prefilled and editable, the same mould as the destination name of a copy, and
  an emptied field still means cancel. The suggestion also DODGES the names the
  hotlist already holds: `persist_hotlist_add` replaces the entry whose name
  matches, and `src` or `docs` collide constantly, so a prefilled field plus the
  reflex to accept without reading would silently overwrite a favourite that
  pointed somewhere else. It qualifies with the parent directory first
  (`norte/src`, which says more than a number) and only then numbers. A name you
  TYPE that collides still replaces: that is what you asked for. The rule lives
  in `norte-frontend` rather than in the TUI, so the window uses the same one
  the day it grows an add.

- **A columns plugin now survives from one page to the next** (#224). A
  twenty-row page over a two-thousand-entry git index cost **167 ms**, with the
  WASM component instantiated and `.git/index` parsed from scratch every time —
  none of it work that changes between page 1 and page 2 of the same directory.
  Instances are now pooled by `(plugin, wasm, location)`, LRU-capped at eight
  with a one-minute idle TTL. Measured on the same fixture: **297 ms for the
  first page, 1.3 ms for the second**. What the pool buys beyond wall clock is
  the reason it was designed for: freshness is deliberately the guest's problem
  — the host cannot know what a plugin's answer depends on — and a guest that
  does not survive its call cannot cache anything at all, so that caching was
  not unused, it was impossible. The embedded backend also stops building a
  whole WASM engine per call, which was starting and stopping an epoch ticker
  thread for every page painted. Two things the pool deliberately does not
  retain: a **live location token** (the session is minted at the start of each
  call and dropped at the end, so between pages the held instance resolves
  nothing) and an instance whose **capabilities no longer match** what the
  catalogue just resolved — a withdrawn consent takes effect on the next call,
  not when a TTL says so. An instance whose guest trapped is dropped rather
  than reused: its linear memory may be half-written, and serving that half on
  the next page is worse than paying for a fresh instance.

### Removed

- **`norte_frontend::shell::login_shell_editor`** — the editor with no file.
  It is a breaking change to a crate that `just semver` checks, and it is
  deliberate: its only purpose was launching an editor on an empty buffer so
  that the editor would create the file, which is the behaviour ADR 0077 takes
  out. Callers want `editor_argv` on the path the daemon created.

### Fixed

- **Clicking a panel did not give it the keyboard.** In the terminal the click
  moved the listing's cursor and its focus border, while the arrow keys stayed
  wherever they were — with the sidebar open you clicked a file, pressed Down,
  and the sidebar's cursor moved. Pointing at a panel is saying "I work here
  now", and that includes the keys. The slot under the pointer is what decides,
  so it is one rule for every panel rather than one per click path: the panel
  no click path attends — the docked viewer, the tree — can receive the
  keyboard too, and a panel that takes no keys (the details sheet, the task
  strip, the status bar) leaves it where it was. The window had the same gap in
  a smaller form: only a row would focus a panel, so clicking its header or the
  empty space under the last row did nothing.
- **The processes panel swallowed the key that gets out of it.** Its handler
  answered "applied" to ANY effect while the task board was empty — which is
  almost always — because the "no rows" check came before deciding which keys
  are its own. So you tabbed into it and the ring ended there: the very Tab
  that leaves was eaten by the panel, and without a mouse there was no way
  back. The places sidebar had the same hole for the same reason; it never
  showed because it always has headers.
- **The window's keyboard ring stopped where no key gets out.** It walks the
  shared focus order, and that order carries everything FOCUSABLE — which is
  not the same as everything that TAKES KEYS. The details panel is the first
  and not the second (it follows the listing's cursor, and with the keyboard
  inside it would follow nothing, which is half of #243), so tabbing stopped
  there, the arrows stopped moving the listing, and nothing on screen said
  why. The ring now skips what takes no keys, with the same predicate from the
  shared registry that the TUI uses.
- **The window's task board was a history.** A finished task stayed until
  another one pushed it out by the row cap, so what it showed at a glance was
  the session's past. Ten seconds now, the same as the TUI: two frontends that
  expire differently are two answers to "is this still running?". The clock is
  armed on the transition to terminal rather than on every progress, because
  the daemon replays the last one on reconnect and re-arming would pin the row
  there for good.
- **The places sidebar was only ever filled by its own key.** One arm of the
  dispatcher copied the favourites into it, so everything else that touches
  that list — starting up with a layout that already brings the panel (`full`,
  `explorer`, yesterday's session), adding or removing a favourite, reloading
  `norte.toml`, switching profile — left it showing the previous list, or in
  the startup case nothing at all. The same favourite appeared in the `Ctrl+D`
  popup and not in the panel beside it. The list now has one funnel and all
  four paths go through it. An OPEN popup still does not rebuild on a reload,
  and that is deliberate: its items are frozen when it opens because
  `dialog.remove` deletes by the row's name, and a list shifting under the
  cursor because of a file edited elsewhere would delete something else. The
  drive list had the same problem through another door — `host.volumes` is I/O
  and `App` has no backend, so each site asked for it itself and it was missing
  from the ones nobody remembered — and is now a flag the run loop drains, the
  same pattern as a profile switch: one place that serves it instead of five.
- **The side panels could not be reached from the keyboard.** `Tab` swaps the
  two listings and nothing else, each panel opens with its own key, and the
  processes panel had no key in ANY of the seven presets — the one place that
  says what norte is copying was reachable only through the menu.
  `layout.focus-next`/`prev` existed and did half the job: they cycled the
  listings, which is what `Tab` already does. They now walk the whole ring —
  the listings and every panel that can hold the keyboard, in screen order —
  and the details panel stays out of it, because it has no `KeyOwner` and would
  be a stop no key gets out of. `alt+o`/`alt+O` for the ring and `alt+j` for
  processes, in all seven presets; in `krusader` `alt+O` is "Sync panels" and
  is bound to `pane.mirror`, so `focus-prev` is left unbound there rather than
  have one chord mean two things depending on where the keyboard is — the ring
  wraps. Each panel's allowlist now takes the ring and the other panels' keys
  too, by the rule that already put its own key there: opening a side column
  must not kill the key that opens the one next to it.
- **The processes panel said what kind of work and nothing else.** A row read
  `copy #7318349021 45%` — the same line for any copy of anything — and a
  finished task stayed there until another one pushed it out, so what the panel
  showed at a glance was the session's history rather than what is happening.
  Rows now name the entry the task is acting on and drop the task id: eighteen
  digits identify nothing to the reader and eat the width the name needs, and
  both the class and the path were already travelling in `TaskProgress`, so
  nothing on the wire changed. The operand is kept per row rather than read from
  the latest snapshot, because `current` is *the entry in progress* and the
  terminal snapshot of most tasks arrives without one — a row that says
  `copy ✓` without saying what it copied is the complaint, not the fix. A
  terminal row now disappears ten seconds after it finishes, on the clock the
  render already injects; live rows are never dropped, for the same reason the
  row cap never drops them. The class label became one shared function: the
  bottom strip and the panel each had their own `match`, and the same task
  labelled two ways reads as two different things — `fs.dir_size` was falling
  through to the generic label in both.
- **`far` gets a rename key and `norton` a search key, and every other gap in
  those two presets is now a written decision** (#228). Six core commands were
  unbound across the seven bundled presets, and the two that were plain
  transcription misses are fixed: Far's own Shift+F6 ("Rename or move the file
  under the cursor") is `pane.rename` — the same chord `total-commander.toml`
  already gives that command — and it had been omitted by being lumped in with
  Shift+F5, which really is a duplicate of `pane.copy`; and `norton` gets
  Alt+F7 for `pane.search` under the same corroboration rule that file already
  states for F5–F8 and Ctrl+U, since the chord recurs unchanged in both presets
  that *were* transcribed from a source. The rest stay unbound **on purpose**,
  because inventing a chord for a preset whose whole point is fidelity is worse
  than the reference sheet printing `—`: Far and NC have no folder
  synchroniser, NC's rename is F6 ("RenMov") and that key is bound to
  `pane.move` whose dialog carries an editable destination name, and mirror and
  pull are norte's own panel gestures that no source attributes to either
  program. A new test holds the line: a core command in a preset is either
  bound or listed in a table with the reason it is not, so the next preset — or
  the next dropped binding — cannot make the gap an oversight again.

- **The TUI's "edit a new one" no longer creates the file behind norte's back**
  (`pane.edit-new`, ADR 0077). It launched `$EDITOR` with an empty buffer and
  let the editor create the file at save time: a file on disk attributed to
  nobody, with no journal entry and no undo — and one that appeared *even when
  the policy would have refused it*, because norte never asked. It now does what
  the window does since ADR 0076: asks for a name, creates the file through
  `fs.create`, and opens the editor **on the task's successful outcome**. A
  creation that fails opens nothing, which is the whole point — an editor over a
  file that is not there shows an empty buffer and creates it on save, which is
  indistinguishable from success until the reader saves. The intention is
  remembered by task id, so a copy or a delete finishing in between cannot
  redeem it and open the editor on the wrong file. The directory is bound when
  the dialog opens, as the window already did, and the editor's working
  directory is the created file's parent — so a `:w other.txt` lands beside the
  file the gesture just made rather than wherever the focus drifted. **What
  norte governs is the creation**: what the editor writes afterwards runs with
  your permissions, outside the journal, and the help topic says so next to the
  sentence that says what *is* governed. Three things the review found and this
  does not close are filed rather than buried: #301 (the embedded backend sends
  no destination anchor, and `ntc` runs embedded by default), #302 (the editor
  is launched with the panel's cwd and an unresolved program name — `pane.edit`
  has done it since #133) and #303 (the window between creating the name and the
  editor opening it).

- **A `Ctrl+C` during a creation now quits instead of opening an editor**. The
  tick that arms the editor also runs the panel refresh that polls the event
  stream, and the loop drains pending work before it checks `quit` — so quitting
  norte while `pane.edit-new` was in flight opened the editor first and exited
  only when it was closed. `App::take_pending_shell` returns nothing once the
  reader has asked to leave.

- **`pane.disconnect` sends the panel to the same place in both frontends**
  (#140, ADR 0077). The window walked the panel's trail back to the last place
  that was not on the machine it just released; the TUI went home, always. Same
  key, same name, two destinations — and the reason they drifted is that the
  decision was written twice. It is now one function in `norte-frontend` that
  both call. The fallback to home also stopped going through `to_str()`: a
  `$HOME` that is not UTF-8 is a valid home (rule 1), and decoding it lossily
  sent the reader to `/` without saying why.

- **A transfer that collides now has a way forward in the window** (#274). The
  window always sends `CollisionPolicy::Fail` — the safe wire default, because
  overwriting or renaming are the reader's calls — but it had nowhere to make
  them: what was left was a failed task on the board and no path onward, while
  the TUI has offered the four exits all along. A collision now opens a dialog
  with the same four (`overwrite`, `newer`, `rename`, `skip`) plus cancel,
  taken from the shared `dialog.*` catalogue rather than invented here, and the
  retry repeats the **same verb** — an "overwrite" on a copy that turned into a
  move would delete a source nobody asked to touch. Cancelling relaunches
  nothing: not choosing is an answer, and the failed task stays as it was.

- **A lost approval says which of the three things happened** (#279, protocol
  **0.55.0**, bridge **38**). `policy.decide` collapsed "that id never existed",
  "it expired" and "someone already decided it" into one `INVALID_PARAMS` whose
  reason lived in an English `message` string, so a frontend could only ever say
  "the approval did not reach the daemon" — true in one case and false in the
  other two, on a security surface. `Error::ApprovalGone` now carries a closed
  vocabulary (`unknown` / `expired` / `already-decided`) and the window picks
  the matching sentence, naming *which* approval — with two stacked, "the
  approval" did not say which. Telling "never existed" from "already resolved"
  needs both bounds of the id range, not just the upper one: the sequence is
  seeded from the clock so a stale dialog cannot hit by collision, which means
  any small invented id sits below it.

- **The painted deadline counts down** (#279). It was computed when the dialog
  opened and never updated, so a modal four minutes old still read "expires in
  300 s". The host now also sends *when* it expires and the renderer counts —
  substituting the number inside the host's own translated sentence, because
  the phrase belongs to the host and the renderer cannot say "expires in" in
  this window's language. With no known deadline nothing counts: counting down
  from an invented one would be worse than not counting.

- **Compressing an archive no longer holds a runtime thread** (#250). Packing
  ran `deflate` inline on the async worker; at level 9 over a large tree that
  keeps a runtime thread busy in long bursts, and those threads are what serve
  every other client of the daemon. Each chunk is compressed on the blocking
  pool instead.

- **`ai.rename_plan` stops being an oracle for agents** (#122). AI is
  human-only, but the actor check ran *after* parsing the params, checking the
  instruction size and running the read gate — so a denied agent could still
  tell "malformed params" from "instruction too long" from "inside vs outside
  my scope" before being turned away. A method that is closed answered
  differently depending on what the caller sent, which makes it a probe for the
  human's tree. The check now runs first, exactly as `index.embed` already did.
  Wire behaviour changes for an out-of-scope agent: it used to get
  `out-of-scope` and now gets `not-approved`, the same answer every other agent
  gets.

- **Denying a prefix now applies backwards to embeddings already stored**
  (#122). The `denied_prefixes` filter decides what gets *read*, so it only
  ever protected files not yet embedded. A file embedded *before* the user
  denied it kept its vector for good — and a vector inverts to an
  approximation of the text — so the only way to honour a new denial was
  deleting `index.db` outright. `index.embed` now forgets the stored vectors of
  files that fall under a denied prefix before doing anything else, using the
  predicate it already computes. What is purged is decided by `policy::is_under`
  in the core, not by a second path comparison written in SQL. Vectors from
  *older models* are purged too: search ignores them, nothing collects them, and
  a stale vector is still the user's data.

- **A degradation reason nobody knows no longer reads as "plaintext FTP"**
  (#279). `connection.degraded` carries a `reason` from a closed vocabulary
  that the protocol says may *grow*, plus an optional human `detail` — and both
  were ignored, so a newer daemon reporting a brand-new kind of degradation
  produced the exact same sentence as `ftp-plaintext`: a security indicator
  asserting a cause nobody had stated. Known reasons now get their own phrase,
  an unknown one falls back to a generic "degraded session" and leans on
  `detail`, which is what the protocol asks for. That `detail` is wire text, so
  it goes through the same masking and truncation as the host, it says when it
  was altered, and it only travels when the reason is unknown — with a known
  reason it adds nothing and would be free-form text from the other end inside
  a security indicator. Bridge **37**.

- **The degradation ceiling can no longer be bypassed** (#279). The retained
  degradations were a bare `VecDeque` with a free function that ordered it, so
  the cap and the dedupe key only applied if the caller went through that
  function and nothing stopped a direct `push_back` past both. They are now a
  `DegradedSet` that owns its data: there is no way to write to it without
  going through the rule.

- **A resumed copy is published with the same mode as an uninterrupted one**
  (#299). The stable staging is created `0o600` and has to be: its name is
  predictable, so while it exists it must be ours and nobody else's (#297,
  #298). But publishing is a `rename`, which does not touch the mode — so a
  copy that got cut in half landed as `0o600` while the very same copy without
  interruptions landed as `0o644`. Same operation, two outcomes, and since #219
  the resumable route is the default one for a single file. The mode is now
  restored on `commit` to what a `create` would have given (`0o666` minus the
  umask, read from `/proc/self/status` where available and assumed `0o022`
  elsewhere — `umask(2)` only reports by *setting*, which would race every
  other write in flight). It is applied to the **descriptor** after the rename,
  never to the path and never before: relaxing a staging still named
  `.norte-partial.<hash>` would leave the most predictable name in the
  directory readable by anyone, for a file nobody has asked for yet. What this
  does *not* do is preserve the source's mode the way `cp -p` does; norte
  preserves permissions on no copy today, and that is a product decision rather
  than something to slip into a fix.

- **The by-path stable staging is no longer reopened blind** (#298). The name a
  resume reopens is `.norte-partial.` plus the SHA-256-128 of the destination
  name — *calculable by anyone who knows where the copy is going* — and it was
  opened with `append(true).create(true)`: it followed symlinks, never asked
  what it had opened, and created with `0o666`. A regular file planted there
  gets published under the legitimate name with its own content, owner and
  permissions; a hardlink to a victim's file receives our bytes; a FIFO hangs
  the blocking pool forever, where no cancellation token can reach it. The same
  hole was closed for the *confined* route in #297; the by-path route has had it
  since ADR 0012 and with fewer defences. It now opens with `O_NOFOLLOW |
  O_NONBLOCK`, mode `0o600`, and checks the opened *descriptor*: regular file,
  `st_nlink == 1`, ours. `partial_digest` does the same, because verifying the
  prefix of a file that is not the one being continued verifies nothing.
- **Packing refuses two entries that fold to one name** (#250, item 1). Bytes
  being different is not enough: what decides is whether they collide *where the
  archive gets extracted*, and an archive cannot know — it gets sent elsewhere.
  `café.txt` in NFD and NFC are two files on ext4 and one on APFS; `µ` and `μ`
  are two here and one on NTFS; `straße` and `strasse` are two almost everywhere
  and one on an ext4 with `+F`. Extracted there, one of the two disappears
  without a word. The fold uses the widest mode on purpose, so the question is
  "do these collide anywhere?" rather than "do they collide here?".
- **A confined destination resumes again** (#297). Confining a single-file copy
  (#219) silently turned `ResumePolicy::On` into a no-op for exactly the case
  where resume matters most — one large file over a link that drops — because a
  confined staging carried an ephemeral per-sink name that no later
  `open_resumable` could find. `LocalConfinedRoot` now opens the *stable*
  staging name, the one derived from the final name's hash that the by-path
  route has used since ADR 0012, so `keep` keeps and the next attempt continues
  after the bytes already there.
- **The sync executor destroys through its confined root** (#296). It held
  `dest_confined` and still resolved every deletion by path. `ConfinedRoot`
  gains `rmdir` — the twin of `mkdir`, separate from `remove` for the same
  reason `unlinkat` has `AT_REMOVEDIR` — so a `Mirror`'s post-order walk names
  what it is destroying instead of letting the path decide.
- **The SDK remembers which protocol version it is talking to** (#294). The
  `InitializeResult` was discarded, so a client could not tell that the check it
  had just asked for did not happen: it sends `expected_digest` (#282), a 0.52
  daemon ignores it exactly as ADR 0004 requires, grants without verifying, and
  nothing says so. Approving with an anchor against a peer that predates 0.53
  is now refused rather than silently unverified.

- **A single-file copy is confined, and so is the deletion an overwrite
  performs** (#218 closed, #219 narrowed, ADR 0072). `ops::copy_task` used an
  unconfined destination for a lone file or symlink, so `fs.copy` — the most
  common operation in the product — still had #164 open. It now opens its
  destination *directory* as a confined root: that directory is resolved once
  instead of three times plus a retry each, and a substitution after that point
  redirects nothing. `ConfinedRoot` gains `remove`, and the `stat` that decides
  a collision plus the `remove` that executes it both go through the descriptor
  — that was the destructive half of `Overwrite`, and by path it could destroy
  a file outside the approved tree while the write that followed refused.
  What is **not** closed: a symlink already in place when the core first looks.
  From inside the core that is indistinguishable from a legitimate
  `~/copias -> /mnt/disco/copias`, and rejecting both would break copying to
  `/tmp` on macOS and `/bin` on a usrmerge Linux. Closing it needs the identity
  observed at approval time to travel with the request.
- **A transfer batch refuses when two of its marks are one name on the
  destination** (#268). `README.txt` and `readme.txt` are two files on ext4 and
  one on NTFS or APFS; enqueuing both let one win non-deterministically while
  the other failed without explanation. The window now asks each pane's
  location how it folds when the listing lands — not in front of the dialog,
  which would put a round trip on the F5 path.

### Added

- **Protocol 0.53.0** (#251, #265, #282, ADR 0071): three optional fields,
  bundled into one bump because each alone would have cost its own version
  window. `TaskProgress.unreadable` says how many subtrees a task could not
  read, so `fs.dir_size` can answer "at least X" instead of a confident total
  that is short — the dangerous direction of wrong for a method that exists to
  answer "does this fit?". `PluginLoadError.dir_bytes` carries the bytes of a
  broken plugin's directory name, which used to cross the wire already
  converted by an unmarked `to_string_lossy` and therefore painted a name that
  differed from disk while declaring itself faithful.
  `PluginSetApprovalParams.expected_digest` (with `PluginInfo.manifest_digest`
  on the way out) makes what gets granted be what the human read: the daemon
  refuses when the manifest changed between the catalogue and the yes.
  Window shifts to N=0.53.x / N-1=0.52.x — a 0.52 peer loses the warning, the
  mark and the refusal, never correctness.

- **The window has the panel gestures the terminal always had** (#290, phase
  A): sort by name, extension, size or time; the sort menu; refresh; hidden
  entries; name reinterpretation; properties; mirror, pull and swap; the
  history and favourites lists; and the volume picker for a SIDE of the
  screen. Sixteen commands, no bridge change: the model for every one of them
  was already shared, so what was missing was the command — which is what a
  preset binds and what the help documents. Sorting still happens by clicking
  the header (both doors end at `SortSpec::after_click`), the sort menu is the
  columns dialog, and properties is the `metadata` slot: the window answers
  with the surface it already has instead of growing a second one. A side is
  resolved by geometry and never falls back to the focused pane (ADR 0058 D9).
- **`[ui] show_hidden` is honoured by the window** (#107), which had been
  ignoring it: a config that said "do not show dotfiles" opened showing them
  anyway.

### Fixed

- **Swapping two panes no longer blanks both of them.** The paint window
  travelled with the pane, so after a swap each side painted rows from a band
  the reader was not looking at — and nothing corrected it, because the
  renderer owns the scroll and a swap does not move it.
- **A swap during a listing no longer freezes it at a hundred entries.** The
  gesture re-issued only a navigation, and a directory longer than one page
  spends most of its listing time in the *other* in-flight state — the drain
  that carries the rest of the stream. The remainder was then discarded and
  the listing stayed at its first page, in `Ready`, saying nothing; marking
  everything acted on that slice. A listing now says when it has FINISHED
  arriving, which is what its flag always claimed to mean.
- **A redundant mirror or pull no longer clears the other pane's marks.** With
  both panes already in the directory, the gesture re-listed the receiving one
  for nothing, dropping its selection on the way.
- **Hiding the hidden entries says how many marks it took with it.** They were
  pruned in silence, so the next bulk op ran on fewer files than the reader
  had marked.
- **The window paints names with the reinterpretation the panel has set**
  (#57). It was cycling the encoding internally and painting the same thing,
  so `pane.names-encoding` could only be read as broken.
- **The session gives back the sort order and the hidden-entries toggle.**
  Both were being written and read by nobody: the window remembered where you
  were and forgot how you were looking at it, so sorting by size lasted until
  you closed it.

### Added

- **The window shows what plugins say about each row.** Bridge **18**: a row
  can carry the badge a plugin put on it, with the theme role to paint it in,
  and a configured `plugin:` column brings its value. Both are asked for the
  VISIBLE WINDOW only — every call spins up a wasm instance per plugin, and
  asking about a directory nobody is looking at multiplies that cost by the
  directory's size for nothing.
- **A listing says how many entries the provider skipped.** Bridge **19**.
  That count came off the wire and the window used to drop it, so a directory
  whose provider skipped entries — no permission to stat them, over a limit of
  its own — showed fewer rows and said nothing. It is the kind of failure you
  cannot spot by looking: what is missing is not there, so the notice goes in
  the pane's header and is announced, not left for a row nobody will find.
- **A column picker.** Bridge **20**: which columns are painted, in what
  order, with which format, and over which scheme. It applies to THIS window
  and does not write `norte.toml` — this phase does not write configuration,
  and the panel says so rather than leaving the user believing they had just
  configured norte.
- **The viewer shows a plugin's preview, and says whose it is.** Bridge
  **21**. A previewer can show anything — that is its job: a PDF as text, a
  formatted JSON — so whoever is looking is entitled to know they are not
  seeing the file's bytes. A previewer that fails, stalls or does not apply is
  not an error: the viewer falls back to the raw view, because a plugin cannot
  leave a file unopenable.
- **The window asks a model for a rename plan, and shows it before anything
  happens** (bridge **24**). The plan arrives, gets validated whole — one pair
  that is not a legal name kills the batch, because applying "whatever is
  valid" of a tampered plan is the failure this belt exists to prevent — and
  only then does the core get asked whether it is applicable. That verdict is a
  second trip, so the review opens saying it is still checking and fills itself
  in. Approving sends the batch with the hash the core itself returned: what
  runs is exactly what was shown, as one task with one undo. A plan the core
  did not accept cannot be approved and says why.
- **A pair's two names are on separate lines, not joined by an arrow.** Painting
  it found the reason: a file called `cap 2 → final.mkv` made the row read as a
  different pair. The separator is now drawn by the stylesheet and the numbering
  by the list itself — neither is something a filename can write.
- **A review that opened by itself does not answer with the next keystroke.**
  The plan lands tens of seconds after the gesture that asked for it and takes
  the keyboard; the first key only acknowledges that. Without it, the `y` of
  someone typing `yes.txt` into the quick filter approved renaming the whole
  directory. `Enter` stopped approving entirely — that is a deliberate break
  with the TUI, where the plan is opened by the reader's own keystroke and the
  next key is an answer; here `Enter` is the key you were navigating with.
  Chords with a modifier are refused, and there are now buttons: a click is a
  gesture aimed at this screen and cannot be a key meant for somewhere else.
- **A plan cannot be approved before it has been read through.** Five pairs of
  up to two hundred and fifty-six were visible, and pair two hundred executed
  without ever having been painted. Approving now needs the core's verdict AND
  the reader having reached the end, and the two refusals say which is missing.
  A name painted differently from what it is OUTSIDE the visible window is
  announced too — the mark of a line only ever existed for that line.

- **A plan that arrives late does not reopen what its owner closed**, and it
  opens over the directory it was PLANNED for, not the one on screen. The model
  takes real time and browsing while it thinks is normal; what must not happen
  is a plan appearing half a minute later, taking the keyboard, and promising to
  rename what is visible now. `Escape` at the listing abandons a plan still in
  flight and says so.

- **The window renames** (`shift+F6`). The field is seeded with what the row
  paints, and the rule underneath it is the one the TUI already had: leave it
  alone and the ORIGINAL BYTES are what get reconstructed — which means the
  destination is the source, so nothing is renamed. That is the protection, not
  a gap: the seed is a screen projection and for a name that is not UTF-8 it is
  not reversible, so it must never become the operand. Touch it and the text
  travels — unless it still contains the replacement character, which is
  refused, because confirming it would write the mojibake the screen invented.
  The consequence, said out loud because it is not obvious: a name that is not
  valid UTF-8 cannot be renamed from this window at all. A name too long to
  fit on screen cannot either, and says so — the clamp appends an ellipsis, and
  `…` is a perfectly legal filename character that nothing masks and nothing
  flags. With more than one entry
  marked this window declines and explains, which is what its own availability
  facts already claimed and what the help page was already dimming.

- **The window copies, moves, creates and deletes** (ADR **0070**). Bridge
  **23**. The renderer names neither operand: it sends "copy", and Rust derives
  the sources from the marks of the focused pane and the destination from the
  pane holding the target role. The final path is the source's last segment
  pushed onto the destination directory — bytes, never through a display
  function. Everything goes through the daemon, so the journal, the policy gate
  and the undo path are the ones the TUI already uses. The collision policy is
  the wire's safe default: a destination that exists makes the task fail and
  the board says so, because overwrite and rename are the reader's decisions
  and this window has nowhere yet to take them.
- **The confirmation labels out of band.** The destination has its own field
  rather than being the first body line behind an arrow, and the reason is a
  fixture the corpus has carried since before anything needed it: `→` (U+2192)
  is legitimate in a filename and is not a terminal hazard, so it is neither
  masked nor flagged. A directory named `docs → /home/you/DELETE` would produce
  a line that reads as two paths, and a reader who parses "arrow, then path"
  would confirm a move believing their files go to the second one. The same
  argument gave the truncation notice its own field: a confirmation that shows
  sixteen names out of two hundred and says nothing describes a smaller
  operation than the one about to run, on the last screen where anyone can
  still say no.
- **Every line of a dialog says whether it is painted differently from what it
  is**, and so does the file a task has in flight. Both surfaces masked and
  threw the flag away, and a dialog body is the one place where a name from
  whoever wrote in that directory gets APPROVED. This also fixed an older
  hole found on the way: the dialog where a human approves an agent's mutation
  cited a Fluent key defined in neither locale, so a truncated list of paths
  painted the raw identifier.

- **Image preview** (ADR **0069**). Bridge **22**. The bytes cross as a
  `blob:` built from a read that goes through the daemon like every other
  read, so the policy engine sees it. Three caps, all refusals rather than
  truncations: a byte budget, the dimensions the header DECLARES checked
  against a pixel budget before anything decodes — a 64 KB PNG can claim
  60000×60000 and cost the decoder gigabytes — and a closed format list
  decided by magic bytes, never by the filename extension. A header that
  cannot be understood is refused too: treating "I don't know" as "go ahead"
  is the door the budget exists to close. A refusal is said, not silently
  swapped for the hex view.
- **A running task can be stopped from the window** (`task.cancel`, `Ctrl+K` in
  the orthodox preset). Which task it is depends on where the focus is: with
  the process panel in front it is the one under its cursor, because a board
  that paints a cursor and cancels something else is painting a selection that
  does not command; anywhere else it is the most recent live one, which is what
  the TUI does with the same key. Nothing running says so, and a task that
  already finished says THAT — collapsing the two would answer "no tasks" while
  the board shows four.
- **A rename batch reports what it left behind.** The report is the only signal
  that a batch left a directory half renamed, and it is asked for even when the
  task says it completed: the task's outcome talks about the batch, the report
  talks about what is on disk. The board row keeps the summary, and a batch that
  left something stuck opens a surface naming what the file is CALLED NOW —
  which is the only actionable thing in the whole report — plus whether the
  journal knows about it, because that decides whether an undo can finish the
  job or only a person can. A daemon that cannot report says so: "the outcome is
  unverified" and "the batch went fine" are different facts.
- **A dialog that opened BY ITSELF does not answer with the next keystroke.**
  The same rule the AI plan review already had, now for the approval dialog and
  the batch report: they appear when the daemon replies, on top of whatever the
  reader was doing, and they take the keyboard. The first key only acknowledges;
  `Escape` is the exception, because getting rid of something you did not ask
  for has to work first time.
- **Persistent notices in the status bar.** Three facts that outlive a keystroke
  and used to be painted by nobody: a provider session travelling UNENCRYPTED
  (the phrase and its cap are now shared with the TUI rather than written
  twice — a security indicator implemented in two places is two places to
  forget the masking), a daemon that announced it is going away, and a mutation
  the daemon REFUSED because it could not open its journal. The daemon notice
  distinguishes a handover from a shutdown: once the connection drops the two
  look identical, and painting "reconnecting…" over a daemon that is not coming
  back is a false wait. It clears when the daemon returns.
- **An approval says how long it has left, and closes itself when it runs
  out.** The daemon stops accepting the id when the TTL expires; a dialog still
  sitting there invites approving into the void — and whoever did would walk
  away believing they had authorized something that was in fact denied by
  silence. The same approval arriving twice no longer opens two dialogs either:
  the SDK resyncs `policy.pending` on every reconnect, so anything still alive
  comes back through the channel, and two dialogs are two answers.
- **The graphical window writes.** The effects switch was `SoloLectura` until
  the mutation security review of phase 5 task 5.4; what supports the change is
  written in the constant's own rustdoc, where whoever changes it will read it,
  and a test still pins it — now in the other direction, so going back is also
  a decision rather than a merge. What the review itself changed: an approval
  that never reaches the daemon now says so (`policy.decide` is sent and
  forgotten, so a daemon that died between the question and the yes left the
  window believing it had authorized what stayed denied by silence), the
  journal notice clears when a mutation is accepted again (an indicator that
  cannot say "it's fine now" lies about the only thing it describes for the
  whole session), and a reconnect no longer wipes a batch report off the board.
- **An undo reports what did NOT come back.** Same shape as the batch report and
  for the same reason: the task's outcome says the undo ran, while an entry that
  was irreversible, a LIFO that stopped halfway, a creation left in place
  because the destination has no trash, or a unit the policy denied are only
  ever counted by the report. A clean undo says so on the board and interrupts
  nobody.

### Added

- **The window asks for a synchronisation PLAN** (`pane.sync-dirs`) and shows
  it: every step with what it does, what it acts on, and whether undo gives it
  back — which never comes from the step's `reversal` alone, because that is
  the half that lies when the destination has no trash. The mode is painted
  first and in its own element: a mirror DELETES at the destination and an
  update does not, and whoever approves has to see that before anything else.
  What blocks the plan is an alert above the steps, because a plan that cannot
  run has to say why before it shows what it would do. Nothing here writes:
  applying is the next pass, and the panel's hint says so rather than offering
  a key that does nothing — and it says exactly that, because the shared
  model's footer offers "a to approve" the moment a plan becomes approvable and
  this pass has no such key. Before the steps it shows the plan's SUMMARY (how
  many cannot be undone, how many bytes, what could not be read, whether the
  list is hiding steps) and what blocks it WITH the path — "the destination is
  read-only" without saying which one sends you looking blind — plus the real
  blocker count, because the wire truncates that list to 256 and a reader needs
  to know there are forty thousand.
- **And the window applies it.** `a` approves; a plan that deletes trees or
  leaves something without a way back asks a SECOND question that only `y`
  answers — and a plan that undoes completely does not ask it at all, because
  asking every time is what teaches people to answer without reading. What
  travels is the hash the core returned, through the one door that checks
  approvability and latches the in-flight apply in the same gesture. While the
  daemon writes, `Escape` asks to cancel and does not close: closing would lose
  the report — the counts, the failures, the undo handle — over a destination
  that was rewritten halfway. When it ends, the report is fetched and shown,
  failures one by one with the path and which root it hangs from, because "3
  failed" without saying which cannot be acted on. Bridge **30**. Two things
  that review changed and that generalise: "it failed" is not "it did not
  write" — the daemon answering *no* and the connection dropping *after* the
  request are different facts, and only the first lets the panel offer to
  apply again, because the second may already be writing; and the writing
  panel now has a way out, the second `Escape`, which says the destination may
  be halfway rather than leaving the only screen in norte you cannot leave.
  A step's and a failure's anchor is SAID, not left in an attribute: staying
  quiet about "on either side" where an unqualified path means "on the source"
  asserts the source.
- **The window has tabs** (#288, bridge **35**): open, close, cycle, move, and
  go to the Nth. The strip crosses the bridge on its own, separate from the
  placements, because an inactive tab is NOT placed — its content is not
  painted — and a window that shows only the front one without saying there
  are two more behind it hides open work. A tab's label is the name of its
  listing's directory, masked like any other name: a hostile directory inside
  a tab is as hostile as inside a listing. The tab that comes to the front
  takes the FOCUS, because working in one you cannot see is what that avoids,
  and a click picks a tab by SLOT rather than by position — against a tree
  that already changed the host refuses instead of guessing right by accident.
  Asking for the 7th of three is refused too: guessing there would be changing
  tab on its own.
- **The window edits the layout tree, not just paints it** (#291): split
  horizontally or vertically, close a slot, and toggle the three auxiliary
  slots it can actually paint — places, the task board, the attribute sheet.
  The new listing starts in the directory of the one it split from and takes
  the focus, because asking for room to work is not going somewhere else, and
  it all lands in ONE snapshot: in two steps the first would show the focus
  where it no longer is. Closing the last listing is refused out loud — a
  screen with no usable listing is not a screen. `layout.preview` stays out on
  purpose: this window cannot paint a preview slot, and one that would only
  paint grey is not open.
- **The window's dialogs go through the shared keymap** (closes #287), and it
  gained mark-by-pattern (#289) and a walkable task board (#292). The dialogs
  were answered with FIXED keys, so rebinding `dialog.confirm` changed the TUI
  and not the window — the drift the shared catalogue exists to prevent. There
  are two regimes, the same pair the TUI has: with a field open the keys are
  letters, because there is no `dialog.*` verb for "type a letter" and
  resolving there would turn typing a filename into answering the question;
  with no field, the keymap resolves. The verb picks among the answers THAT
  dialog offers — one it does not offer is not interpreted, which is why
  `dialog.confirm` does not approve an approval. A running task cannot be
  dismissed from the board: stopping it is another key, and taking something
  that is still writing out of view is losing sight of exactly what to watch.
- **The window copies paths, opens with the desktop, and drops a terminal
  where you are** (`pane.copy-path`, `pane.open`, `app.terminal`). Native
  effects leave the host on their own channel, never through the webview:
  they carry paths and programs, and the webview neither needs to see them
  nor has permission to run anything. None of the three is a shell — each
  builds a closed `argv` from a list and nothing goes through an interpreter,
  so a filename with a `;` stays a filename. The clipboard travels as BYTES
  over the helper's stdin: a lossy decode would paste a path that opens
  something else, and in an `argv` a path starting with `-` is a flag. What
  is not on this disk is refused out loud — `xdg-open` cannot take an
  `sftp://` — instead of quietly opening somewhere else. And a frontend with
  no desktop behind it refuses the gesture rather than claiming it copied.
- **The window can undo everything an agent session did** (bridge **33**,
  closes #276). `app.agents` lists the agent sessions THIS window has seen ask
  for permission — the only thing in the whole protocol that names one is the
  approval request an agent triggers, so that is what the host records, panel
  open or not, and whether or not the approval was granted. The screen says
  that is what the list is: an empty one without that sentence reads as "no
  agent has touched anything", which is a claim this window cannot make. `u`
  asks first, naming the scope — it reverts ALL of that session's operations,
  and whatever cannot be reverted is named in the report that already existed.
  A session id is an OPAQUE daemon key: it paints masked and flagged, and what
  travels back is the raw one. The row separates "asked N times" from
  "approved M from here", which are not the same number once another window
  answered, or the human denied, or it expired. What this buys over typing the
  id — which is what phase 5 refused to build — is that the operand is CHOSEN:
  a typed session id can be the wrong one, and undoing the wrong session undoes
  somebody else's work. The list is one of the few in norte that changes with
  no gesture — a permission request reorders it — so it carries a generation, a
  click is refused rather than clamped when it names the previous one, and the
  selection follows its session by id and not by position. It also says how
  many sessions it forgot to its cap, because a session id is chosen by the
  agent: flooding the list to push a particular one out is within reach, and a
  truncated list presented as complete is what would turn that into "that
  session does not exist". Bridge **34**.
- **The window governs extensions** (bridge **31**). Approve and revoke a
  plugin's capabilities, enable and disable it, edit its `[config]` keys, and
  run the commands it contributes from the palette. Three things are not
  decoration: approving ASKS, and the question lists the capabilities one per
  line, each masked on its own and carrying its own flag — the line that
  paints differently from what it says is exactly the one a hostile manifest
  writes to slip in among the real ones; none of it exists in read-only mode,
  because the switch that decides whether this window deletes is the switch
  that decides whether it grants permissions; and after a change the CATALOGUE
  is fetched again rather than flipping a local boolean, with the cursor
  re-placed by id, because the core orders by category and id and approving
  can move the row. The config editor is the shared model, so a `bool`/`enum`
  cycles, a `string`/`int` is typed and validated against the schema's bounds,
  and a `kind` this build does not know is read-only — and the screen now says
  so, because offering `Enter` on something that will not change reads as a
  failed write. While a value is being typed, letters are letters: resolving
  `a` as "approve" there turns typing "casa" into two grants. A command's
  output is third-party text: masked, capped, and the fact it was cut is said,
  because the reader cannot deduce it from text that arrives already short —
  and it travels LINE BY LINE with a flag per string, because a newline is a
  C0 control, so masking the whole output marked every multi-line run as
  hostile while a hostile plugin name with ASCII output went unbadged. The
  panel also names the extension by its reverse-DNS id: a name does not
  identify, and two manifests can claim the same one. Bridge **32**.
- **The window compares two directories** (`pane.compare-dirs`): a diff panel
  with the rows as they stream in, a filter per category with its count, side
  switching, and `Enter` to go where a row points. The model is the one the TUI
  already uses — nothing here re-pairs rows or decides a verdict — and two
  pieces of it are worth naming because reimplementing them is what has bitten
  other surfaces: the status line distinguishes "finished" from "finished but
  batches were lost" and from "the channel closed and nobody saw the outcome",
  which in a comparison IS the answer; and where a row opens comes from the
  shared rule, so an orphan seen from the side that does not have it opens
  nothing rather than falling back to the other side. What crosses the bridge
  is a WINDOW of rows: the engine emits one row per paired name over the whole
  tree and capping that would turn "are these the same?" into half an answer.
  Cancelling is the only brake, and the second `Escape` closes the panel
  whatever the daemon is doing. Bridge **28**.
- **The window searches by MEANING** (`pane.semantic-search`, from the command
  palette — no preset binds it, in either frontend). The shared catalogue has
  had the command since the keymap work and the host answered "not here"; now
  it asks the index and shows what came back, best first, with the similarity
  in its own cell — without it a 0.91 and a 0.42 read as equally good and the
  order looks arbitrary. Results extend the existing search view rather than
  opening a second list: two lists drift, and the one you are looking at stops
  being the one you navigate. A hit carries no KIND, because the index answers
  with paths and scores and claiming "file" because it usually is would be
  inventing the answer — so activating one opens its directory with the cursor
  on it. Nothing indexed yet is its own answer ("run `norte index build`"), not
  an empty result set. The command is treated as read-only-removable for the
  same reason as the AI rename: it writes nothing, but the query leaves the
  process. Bridge **27**.

### Changed

- **Bridge 26.** Three shapes moved, all for the same reason: what is masked
  has to say so, and what comes from outside does not go inside a sentence.
  The status bar's persistent notices are no longer bare strings — the
  cleartext-session one carries the connection in its own field, with its flag,
  because `{scheme}://{host}` inside the phrase turns a host called
  `bank.example@evil.example` — which contains nothing that gets masked — into
  something that reads as userinfo of a legitimate host. An approval carries
  what is being asked, WHO is asking (the agent session, which was being thrown
  away, so the reader could not tell which agent), and the deadline, each in its
  own field; the body is now only the paths, numbered by position, and a
  deadline that is not known says so instead of staying silent. And the four
  surfaces of issue #266 that masked and dropped the flag — a layout parser
  diagnostic quoting the user's file, the name of a slot kind this renderer does
  not paint, a theme's unsupported effect keys, and a key's label from a keymap
  — now carry it. The issue asked for exactly this: not to spend a bridge
  version on four flags alone.

### Fixed

- **`task.cancel` could stop a task that was not on screen.** The board is
  capped at 256 rows and the cursor is an index; the cap lived in one place and
  the cursor counted over the whole map, so with more than 256 tasks — marking
  three thousand files and pressing F5, one task per entry, and eviction only
  takes the FINISHED ones — the highlighted row and the cancelled task were two
  different tasks. There is now one definition of "the visible ones" and every
  index means the same thing.
- **A rename batch that was already finished when it arrived never asked for its
  report.** The daemon can complete it before the call returns, and then the
  progress channel never fires: the request hung off progress alone, so the only
  signal that a directory was left half-renamed went missing precisely on the
  fast batches, where the outcome most looks like everything went fine.
- **A dialog that opened by itself could be answered by a CLICK.** The
  acknowledge rule was keyboard-only, and the pointer is the primary input of
  this surface: dialogs paint in the same place with the same first button, so a
  click already in flight over "Confirm" landed on the "Approve" of an agent
  approval that had just arrived. The rule now lives where both inputs pass.
- **Clicking the processes panel or the places sidebar did not focus them.**
  Focus by click demanded a listing; only the Tab cycle could reach the others.
  Both now use the same shared focus order.
- **An approval path redacted by the daemon was shown as faithful.** The flag was
  computed by masking the text again, but the daemon had already replaced the
  dangerous bytes with U+FFFD, so it never fired for the most dangerous class —
  while a zero-width space, which that pass does not touch, did fire. The
  replacement character is itself the signal, and it is now read as one.
- **A clean but over-long path was shown truncated and declared faithful.** The
  bridge's clamp appends `…` AFTER the verdict, and `…` is a legal filename
  character, so the reader could not tell "it is called that" from "this was
  cut" — in a batch report that name is the only actionable thing there is.
- **Task ids restart at 1 in every daemon, and the board did not know.** After a
  handover — which this window now hears about — the new daemon hands out the
  same numbers, and a new task inherited from the old one that its report had
  already been asked for (so it never was), along with its affected directories
  and even its row detail. Rows now carry the connection epoch, and a report in
  flight from the previous one is dropped rather than hung off whatever carries
  that number today.
- **The cleartext-session notice deduplicated by scheme alone**, so a second FTP
  host EVICTED the first and the "+N more" counter went to zero. The one that
  disappeared was the host the reader was not looking at, which is the only
  question the indicator exists to answer. A session is `(scheme, host)`.
- **The dialog stack had no ceiling** now that the wire feeds it: another client
  running two hundred stuck batches piled up two hundred modals, each cloned
  into every dialog patch. Eight at a time, a report is sacrificed before a
  decision, and the drop is said out loud.
- **A report whose row had already been evicted was dropped in silence** —
  losing exactly the "left half-done" that never gets folded into "it went
  fine". Without a row it skips the detail and still says what it has to say.
- **Cancelling from a window mounted without effects could abort ANOTHER
  client's transfer**, leaving them a `.norte-partial`: cancelling a copy does
  touch the disk. Own tasks stay cancellable — launching them already needed the
  switch.
- **Whether a task was still alive was read from a stale projection**, so
  "cancelling…" was said about something already finished, and "the most recent
  live one" could skip the one actually running. It now asks the live progress,
  which is what the TUI does and for the same reason.
- **A task with no byte totals showed no progress at all in the window.** The
  percentage only looked at bytes, so a delete — which counts entries, not
  bytes — crossed the bridge with nothing to paint from start to finish. The
  arithmetic now lives in `norte-frontend` alongside the TUI's, which already
  fell back to entries: this is exactly the divergence a second copy of a
  presentation rule produces.

- **The destination pane was guessed, and the guess claimed to be a choice.**
  With two panes the destination is "the other one" and nobody notices the
  concept exists; the window reassigned it on every focus change to the
  lowest-numbered other pane, and recorded that guess as an explicit human
  designation. With three panes — a layout a user writes — designating one by
  hand and then pressing Tab silently moved the destination somewhere else.
  The rule now comes from the shared layer, which is where it was already
  written (ADR 0058 D7): a chosen destination survives, and with several
  candidates and none chosen the role is left unset and the transfer asks you
  to pick rather than breaking the tie for you.
- **A pane refreshed after an operation moved the reader's cursor onto a
  different file.** The cursor was restored by INDEX, and a refresh is exactly
  the case where the index stops naming the same file: the operation removed or
  added an entry. Nothing on screen explained it — the pane had not moved under
  a keystroke, it moved under a task completing — and the next key could be
  F8. The cursor is now pinned by path, and the marks come back by path too
  instead of being dropped: a listing that reloads on its own was taking a
  selection someone had made by hand.
- **A refresh could throw away a navigation.** A pane's directory only changes
  when its listing lands, so a refresh triggered while the reader was walking
  into a directory re-requested the OLD one, and the navigation's answer
  arrived with a stale token and was discarded. The pane sat in the directory
  the reader had just left, with the trail already recorded, in silence. A
  refresh now skips a pane that has a request in flight, and recognises a pane
  by where it is heading rather than by what it is still showing.
- **A hidden pane on a changed directory stayed wrong forever.** A tab behind
  another one is not re-listed — what is not seen is not fetched — but nothing
  recorded that it had gone stale, so bringing it back showed a listing from
  before the operation. It is now marked loading, which the existing wake-up
  path already picks up.
- **The marks consumed by a transfer were whichever pane had the focus.** A
  click on the other panel is not blocked while a dialog is open — only keys
  are — so clicking away between the question and the answer cleared the wrong
  pane's selection and left the sent one fully marked, inviting a second press
  of F5 over the same files.
- **A batch opened one connection per marked file.** Marking a few thousand
  files and pressing F5 is the ordinary way to use an orthodox file manager,
  and it fired that many simultaneous requests at the daemon; over SFTP that is
  not a copy. The batch is now enqueued in order through one sender. The task
  board also honours its own documented cap again, and prefers to drop a task
  that finished cleanly over one that failed — a failure leaves no journal
  entry, so its row is the only surface that says what did not happen.
- **The command palette offered what the window refuses to run.** It was the
  one door that did not go through the effective keymap, so a read-only window
  listed copy, move and delete and then declined them.

 Bridge version
  **17**, and it breaks on purpose: volumes arrive from a background task and
  are inserted in the MIDDLE of the sidebar — drives sort before favourites —
  so between the frame the user clicked and the host handling the action, that
  row could be a different place. Worse, the cursor was CLAMPED rather than
  refused, so the failure mode was navigating to the last entry in the list
  with an `Applied` acknowledgement. The sidebar and the volume picker now
  carry a generation and refuse a click that does not match it; the other six
  index-named actions do not, and the code says why — their row set cannot
  change without a gesture from the user. What they all do now is REFUSE an
  out-of-range index instead of clamping it: choosing a layout applied the
  LAST one in the list, and going to a search hit went to the last hit.
- **Choosing a layout on a narrow window killed it.** The slots were seeded
  from the resolved PLACEMENTS, and placements and hidden partition the tree,
  so a layout whose listing the resolver does not place — a tab group whose
  active child is another kind, a fully weighted split that does not fit —
  emptied the map, and the next keypress died in an `expect` inside the
  actor's task: no log, no visible crash, a window answering `Down` forever.
  Seeded from the tree now, which is what `validate` actually guarantees.
- **A hidden slot that came back was never listed.** It stayed `Loading`
  forever with zero rows, and after a layout change it painted as
  `Unsupported { kind_name: "browser" }`.
- **A search with no hits never ended and never cancelled.** The search was
  named by its first batch, and the core does not send empty batches, so on a
  tree with no matches the Task id never arrived: the view said "searching…"
  forever and `Esc` had nothing to cancel while the daemon walked the whole
  subtree for a surface that was already closed. A late batch from a previous
  search could also be adopted by a new one, filling a list labelled with one
  query using the results of another.
- **A modal dialog did not own the keyboard.** With a name prompt open,
  `Backspace` navigated the pane to its parent and `Enter` entered the
  directory under the cursor instead of confirming. `Enter` now picks the
  first NON-destructive answer, so in the dialog where a human approves an
  agent's mutation it picks "deny".
- **The dialog's text field was rebuilt EMPTY on every keystroke**, and every
  keystroke causes a patch: what reached `fs.mkdir` was the last character
  typed. It is the one surface where a user approves the bytes that become a
  filename.
- **Twenty-two Fluent keys chosen in Rust did not exist**, so they painted as
  their own identifiers — including both buttons of the agent-approval dialog,
  which read `dialog-approve` and `dialog-deny`, and the title of the delete
  confirmation. The task strip had the same problem from the other side: the
  renderer asked for `task-kind-…` and the catalogue spells them
  `gui-task-kind-…`, so every task read as its key.
- **The settings view blocked the whole window.** Projecting "where each thing
  lives" called `exists()` — a synchronous `metadata` — inside the single
  writer's loop, so a config layer on a hung NFS mount froze keys, listings
  and task progress until the mount timed out.
- **Plugin-authored `[config]` text reached the DOM unmasked.** A plugin's
  effective value, its schema default and every value of an `enum` are free
  text that the manifest bounds only in LENGTH — no charset check — while
  three doc comments claimed the opposite.
- **A column id is an identity and was being masked**, so two configured
  columns differing only in an invisible character collapsed into one and
  clicking the second sorted by the first.
- **The graphical window overwrote the terminal's layout.** It wrote its live
  tree into the session's `layouts["default"]`, which `norte-tui` adopts at
  startup, so a minute spent browsing the layout picker changed what the TUI
  opened with. It also started from an empty body, discarding another
  frontend's slots, and stamped live slots with a zero clock that the next
  writer would read as thirty days old.

### Added

- **The window can search a whole subtree.** Bridge version **16**: a prompt
  takes a name glob, `fs.search` runs as a cancellable Task, and the results
  arrive in BATCHES — the list can be walked and used before the search
  finishes, which is half the value of searching a big tree. It says which of
  the three states it is in, because a short list that stopped growing, one
  still growing, and one that stopped at the limit read the same otherwise.
  Going to a result navigates the pane to its directory and leaves the cursor
  on it, byte-exact: the renderer sends an INDEX and never a path, and the
  path the daemon sent is handed straight to the pane — a painted name never
  becomes a path again, because that is how you end up opening another file.
  Closing the search cancels the task: walking a tree for nobody spends the
  daemon on a result that has nowhere to appear.

- **The window can be reshaped, and pick another shape.** Bridge version
  **15**: `layout.grow`/`shrink` resize the slot that has the FOCUS — which is
  the only way to widen the sidebar, and the reason it is the focus and not
  the active listing — and `layout.equalize` puts its weighted siblings back
  on equal terms. `layout.pick` opens the picker: the five factory layouts
  plus whatever is in `layouts/*.toml`, each with the *shape* it would produce
  drawn by the same engine that lays out the real screen, so the preview
  cannot lie about what comes next. A row whose name also names a keyboard
  preset says so — choosing it changes no key, and without the line the
  coincidence is a trap rather than a convenience — and one whose file does
  not parse is offered with its reason instead of being dropped, but choosing
  it changes nothing: swapping the screen for a broken file is worse than
  doing nothing. The choice applies to this window and is not written to the
  configuration; writing is mutating, and that is Phase 5.

- **The places sidebar is a sidebar.** Bridge version **14**: the host's
  volumes with their space, and the user's favourites, each section foldable.
  Volumes are asked for at start-up and again when the drives section is
  unfolded — and nowhere else: a sidebar with a clock would break ADR 0058's
  suspension rule from the first frame, and `host.volumes` is not free (it
  mounts and queries space on every filesystem). A favourite whose path does
  not parse is *painted* with its reason: one that vanishes quietly is a
  configuration fault nobody can see. Enter — or a click, because a sidebar
  exists to go places — navigates the *focused listing*, down the same road as
  any other `cd`, which is what makes having it open not change where
  operations go.

- **The details sheet and the process panel are panels, not grey rectangles.**
  Bridge version **13**: a `metadata` slot shows what the listing already
  knows about the entry under the cursor of the pane it *follows* — resolved
  with the shared engine, so a slot following a role that lost its pane
  degrades to the active one instead of staring at nothing — and it reads
  nothing: the `Entry` is already there, and a panel that followed the cursor
  by asking per row would turn walking a directory into a storm of requests.
  A `processes` slot shows the same tasks the strip does, with its own cursor;
  it keeps no second copy, because two lists of tasks drift and the one you
  see stops being the one you cancel.

- **The window shows the theme from the inside, and its volumes.** Bridge
  version **12**: `F9` lists every semantic role with the colour it resolves
  to — as a swatch, because `#2d4f8a` tells nobody anything until it is next
  to the square it paints — and *names* the `effects` the theme declares that
  this renderer cannot paint. Naming them is the point: a retro theme that
  looks identical to every other reads as broken, and the user goes hunting a
  bug that is not there. The volume picker asks the host for its mount table,
  says so while it waits (which is not the same as "none"), and choosing one
  navigates the pane to it — navigation is reading, so it is allowed. A size
  the system did not answer is *said*, never painted as `0`, which reads as
  "full" — the opposite of "unknown".

- **The window shows what is installed and what it asked for.** Bridge version
  **11**: `F12` lists the extensions with their state told as the TWO
  independent facts it is — approved, and switched on — because one approved
  and later switched off is not the same as one nobody has looked at yet, and
  neither is "still asking the daemon" the same as "none". The capabilities a
  plugin requests are on the ROW, not behind a second gesture: they are the
  decision a human approves, and hiding them turns "this can read your files"
  into something you have to go looking for. `Enter` opens its `[config]`
  schema with the *effective* value of each key beside the schema default, so
  what has been changed is visible. Read-only throughout, and structurally so:
  the window's backend has no `set_approval` and no `set_enabled` — what is
  not there cannot be called by accident. A directory that failed to load is
  shown rather than dropped, because an extension that vanishes quietly is one
  the user believes they have.

- **The window shows its settings, and where they come from.** Bridge version
  **10**: `F11` opens the shared registry — the same catalogue the terminal
  shows, with the same stable ids — each entry with its *effective* value, plus
  a section the terminal does not have: where each thing lives (the config
  layers in precedence order, the state directory, the logs, the daemon
  socket). A layer nobody has created says so instead of painting a path that
  looks like it is there, and a directory whose name carries hostile bytes
  arrives masked and flagged, down the same road a filename takes. Locations
  only — never a value, so nothing secret. Read-only, and it *says* so rather
  than offering an `Enter` that would refuse: this window does not write
  settings until Phase 5 supplies the safe path, and every entry needs a
  restart because the window resolves theme, fonts and keymaps once at
  start-up. It is told once per section instead of on every row.

- **The window has help, and the help teaches the reader's own keys.** Bridge
  version **9**: `F1` opens the shared corpus over the page for *where the
  reader is standing* — a dialog, the viewer, the listing — and never on an
  index they never asked for. The prose crosses as a CLOSED vocabulary of
  blocks, with the corpus' two live marks already resolved: a `{{cmd:…}}`
  becomes the key this user's preset binds (or the command's name, never an
  invented key), and a `[[topic]]` becomes that page's title. The renderer
  builds a DOM node per block and never parses markup, which is the whole
  reason a third party's `help.md` can be painted at all. The keyboard sheet is
  a page of that help, generated from the effective keymap — a rebind changes
  it — with every unavailable key dimmed *and explained*, because dimming alone
  leaves a reader guessing whether the app is broken. A row this window cannot
  run is offered switched off rather than promising an `Enter` that would
  answer "not here", and running one goes through the same path a keystroke
  takes. The resolver behind all of it moved out of `norte-tui` into
  `norte-frontend`, so the two frontends cannot teach different keys for the
  same command.


- **The window has a command palette.** Bridge version **8**: `Ctrl+P` opens
  every command this frontend implements, each with what it does in the
  reader's language and the shortcut the *user's own preset* binds to it —
  never a hand-written list. Typing narrows over what is painted (name and
  description, the way the terminal's palette folds it), the arrows move,
  `Enter` runs the selection through the same path a keystroke takes, and
  `Esc` closes without running anything. The model — filtering, cursor, what is
  selected — moved out of `norte-tui` into `norte-frontend`, because two
  frontends with two copies are two palettes that drift without anyone
  noticing.

- **A half-typed prefix shows what continues it.** Bridge version **7**: the
  window paints a which-key panel while a key sequence is pending — which key
  follows, what each one does in the reader's language, which ones open another
  sequence (marked, rather than named after a command they do not run) and
  which ones this frontend cannot do, with the reason already translated. It is
  the shared `norte-frontend::whichkey` model, built on the keystroke that
  opens or deepens the sequence and dropped the moment it closes, because its
  own contract says a panel kept past that point describes keys that are no
  longer live.

- **Column headers and the viewer travel as patches.** Bridge version **6**.
  Sorting used to move the rows and leave the `▲` describing the previous
  order — the screen contradicted itself, and `aria-sort` said the wrong thing
  out loud — because no patch could carry headers. And every keystroke in the
  viewer shipped a whole snapshot, which meant the visible rows of both
  listings underneath, per line of scroll. The renderer also declares how many
  lines fit in the viewer instead of the host guessing it from layout cells
  minus an assumed chrome: the guess sent more lines than were shown (clipped
  in silence) and paged by a different number, so every page-down skipped what
  had been clipped.

- **A row is named by key and generation.** Bridge version **5**, ADR 0068.
  Every action that names a row now also names the screen it was named on, and
  the host refuses it when the listing has moved on since. The contract had
  promised this from the start — "a late double click does not act on the file
  that took that row afterwards" — and nothing implemented it: the key was the
  index and the guard was a bounds check. A range whose endpoint is outside the
  window is now refused rather than clamped, because clamping widened a mark —
  and what is marked is what gets deleted — to rows the renderer was never
  shown.

- **The graphical frontend can look at a file.** Bridge version **4**: F3
  opens a viewer over the listing, reading a bounded 256 KiB head — the rest of
  the file is not read, the same budget the terminal uses — decoding it through
  the shared detection, and travelling as lines that are already sanitised. A
  binary goes to a hex view because of its *content*, not its extension. While
  the viewer is open the keys are its own: it is the `viewer` screen of the
  same keymap preset, so `esc` closes, `e` reloads with the next encoding and
  `F8` does **not** reach the listing underneath. Encoding, line ending,
  forced-encoding, decoding-errors and truncation all travel as facts the host
  resolved; the renderer paints them and computes none of them.

- **The graphical listing has column headers, and they sort.** Bridge
  version **3**: every listing carries its headers with the label already
  translated in Rust, which column is sorting and in which direction. A click
  sorts by that column's id — never by its position or its label — and what a
  second click on the same column means (invert, rather than start again) is
  `SortSpec::after_click`, the rule the terminal frontend already used.
  Directories keep their own group: inverting the order does not touch it.

- **Two panes behave like two panes.** The wheel over the pane that does not
  have the focus now moves *that* pane without stealing focus — saying which
  rows are visible is not an action on the listing, it is where the reader is
  looking. The background fill of a listing announces itself in its own pane
  instead of always in the active one, and a listing answer lands in the pane
  that asked for it, not in whichever pane happens to be focused when it
  arrives. `pane.switch`, `layout.focus-next`, `layout.focus-prev` and
  `layout.set-target` reach the host through the shared focus order, so the
  keyboard can change panes and the focus never lands on the status bar.

- **norte has a graphical window again, and it is a spike, not a product.**
  Phase 3 of the multi-frontend plan: `norte-gui-tauri` is a Tauri 2
  application over `norte-ui-host`, with a plain-TypeScript webview that
  paints and does nothing else (ADR 0067). It lists, navigates, moves the
  cursor, marks (including a range in one gesture), quick-searches, shows the
  task strip and the status bar, and asks for confirmation before a delete —
  all of it against a real daemon, with two panes laid out by Rust. The
  webview has no filesystem, no shell, no HTTP, no `rpc(method, params)` and
  no `window.__TAURI__`: four commands are the whole surface, and a test
  fails if a fifth appears. It does not write: the host takes a read-only mode
  at startup and the window uses it, so the mutating commands are absent from
  its keymap, refused at execution, and the policy-approval channel is never
  taken. Phase 5 lifts that. It exists to answer a question with measurements;
  the go/no-go is in `docs/spike-tauri-2026-08-20.md`.

- **The bridge projects the screen's layout, and its snapshots are complete.**
  Bridge version **2**. `ViewSnapshot` now carries a `LayoutView` — where each
  slot goes, in layout cells, with its role — so the renderer no longer has to
  invent where two panes live or which one is the target; that rule stays in
  `norte-frontend`, shared with the terminal. Changing focus travels as a
  patch rather than a whole screen. And a snapshot now includes the open
  dialogs and the live task board: it did not, so a renderer that resynced
  while a delete confirmation was up would have painted the question away
  while the operation waited for an answer.

- **A range of rows is marked in one action.** `UiAction::MarkRange` — what
  belongs to a range (and what does not, like `..`) is a selection rule, and
  those live in `norte-frontend`, not in whoever paints. Shift-click in the
  new window goes through it.

- **The graphical frontend can paste.** It could not — at all: no input
  handler, no clipboard read for text, and `Cmd+V` filtered out before it
  reached any field. So a path, a rename, a search term or a filter had to be
  retyped. Pasting now works in the rename prompt, the AI-rename and semantic
  prompts, the command palette, the live filter and the settings editor, and
  what arrives is filtered exactly like what is typed: terminal hazards are
  dropped, and a newline **cuts** instead of confirming — pasting two lines
  into a one-line field cannot mean "accept the first and carry on with the
  second", because nobody has read the second. Decision dialogs still take no
  paste: a yes/no has no field, and giving it one is how something gets
  approved by accident (#200).

- **The GUI's diff pane can mirror.** `s` plans an update and `m` plans a
  mirror, the same two letters the terminal uses. `SyncMode::Mirror` was
  unreachable from this frontend — the dispatch knew one entry point and it
  was `Update` — so the whole destructive half of the sync spec was written
  for a mode the GUI could not ask for. The panel stays open when you press
  them, and that is not cosmetic: the plan takes its source from the pane's
  ACTIVE side, and that branch was previously reachable only through the help
  overlay, because the diff pane owns the keyboard and `pane.sync-dirs` never
  arrived while it was up (#188).

- **Archives can be written.** The last five commands the presets bound and
  norte did not have (#132). {{pack}} builds a new archive from what you
  marked — the name you type decides the format, and the dialog says which one
  it is going to write before you press Enter: `.zip`, `.tar`, `.tar.gz` or
  `.tgz`. `.rar` is refused rather than quietly written as something else,
  because norte reads rar by delegating to another program and that program is
  not asked to write. Unpacking needs no dialog and no new machinery: it is a
  copy out of the archive into the other panel, with the collision questions,
  the journal entry and the undo that copying already had. Testing reads every
  entry to the end and says **what it checked** — a zip has a CRC per entry, a
  `.tar.gz` one for the whole stream, and a plain tar none at all, so
  "passed" means three different things and the report distinguishes them.
  Splitting cuts a file into `name.001`, `name.002`… in the other panel, and
  joining puts them back from the `.001`; a gap in the numbering or a short
  piece in the middle stops the join instead of producing a corrupt file that
  looks fine. On the wire that is protocol **0.50.0** and **ADR 0060**.

  What did *not* change is that an archive is read-only from the inside: none
  of this writes into a container, and copying into one is still refused. A
  cancelled pack leaves no file — an archive written halfway still looks like
  an archive.

  With those five, **the shared catalogue has no `Planned` commands left**:
  every command a preset names is one norte has.

  Four reviews of this branch found things worth naming here, because two of
  them were silent. **An agent could have used packing as a laundry**: the read
  gate looks at the root of a request and nothing else, so a legitimate scope
  over a large tree packed the daemon's state directory with it — `journal.db`,
  `secrets.age`, `connections.toml` — and the archive was then readable entry
  by entry through a file inside that same scope. The walk now consults the
  same exclusion list `fs.search` and `fs.compare` use. **Two size fields could
  be written wrong**: a tar entry of 8 GiB or more recorded a size of zero (the
  octal field kept the low digits) and a zip past 4 GiB flagged one field for
  zip64 while writing three, which our own reader — and every conformant one —
  reads as garbage. Both now refuse or write correctly, and both have tests
  that need neither 8 GiB nor 4 GiB of disk. **Joining across a gap** built a
  short file and called it done, which is the exact failure this command exists
  to prevent; the test that should have caught it asserted the bug. And a split
  that is cancelled now takes its pieces with it, because half a set of pieces
  is indistinguishable from a whole one.

- **A directory tree panel.** `pane.tree` opens a column on the left with the
  tree hanging from the directory you are looking at; `⏎` on a branch expands it
  and sends the listing there. It is read branch by branch — opening one lists
  that directory and nothing else — because a tree that read itself whole would
  take minutes on a big folder and far longer on a remote one. Only directories
  show: a tree with files in it is a worse copy of the listing next to it. Three
  presses like the places panel: open and take the keyboard, take it back, close
  (#136).

- **Opening and closing a connection from the keyboard.** `pane.connect`
  (Ctrl+N in the Total Commander and Krusader presets) lists what is in your
  `connections.toml` — name and address, never a password — and takes the panel
  to the one you pick. `pane.disconnect` (Ctrl+Shift+D) does both things its
  name promises: it releases the session, so the socket closes now instead of
  when it eventually times out, and sends the panel home. On a local panel it
  says there is nothing to close rather than answering "done" (#140).
  On the wire that is protocol **0.49.0**, together with `fs.dir_size` below:
  `connection.close` closes by PATH, not by a session key the frontend has no
  reason to know.

- **F4 edits.** It opened the file with the system handler, which is a
  different thing and is what #133 was about. `pane.edit` now hands the file to
  your editor — `$VISUAL`, then `$EDITOR`, then `vi` — and steps aside while it
  runs, exactly as it does for a shell; leaving the editor brings the panels
  back and reloads the listing. `pane.edit-new` opens an empty buffer in the
  directory you are looking at, and lets the editor ask for the name when you
  save. The path travels as its own argument rather than inside a command line,
  so a filename with a quote or a newline in it reaches the editor unchanged
  instead of breaking the line. It refuses a folder and a remote panel, and says
  which (#133).

- **Properties, and how much a folder actually takes.** `pane.properties`
  (Alt+Enter in the Total Commander and Krusader presets, Ctrl+A in Far) opens
  what norte knows about the entry under the cursor: kind, size, date, path and
  whatever attributes the backend reported. All of that is already in the
  listing, so opening it asks for nothing — except the one thing a listing
  cannot know, which is how much a folder takes. That is counted, and the dialog
  says so while it counts. `pane.dir-size` (Ctrl+L in TC, Alt+Shift+S in
  Krusader) counts without opening anything, over what you marked. Counting is a
  cancellable task like any other, and an unreadable folder in the middle of a
  big tree costs its own subtree rather than the whole count (#139). On the
  wire that is protocol **0.49.0**: `fs.dir_size` delivers its answer through
  the PROGRESS of a task instead of a result type of its own — the last
  snapshot is the result.

- **Sorting has keys now.** Sort the focused panel by name, extension, size or
  date without opening anything; pressing the one already in use reverses it,
  exactly like clicking a header twice. The presets that bind these keys — Far's
  Ctrl+F3..F6 and its Ctrl+F12 sort menu, Total Commander's, Krusader's — stop
  saying "not built". Sorting by extension is new as an order: `.TXT` and `.txt`
  land together, `.bashrc` counts as a name rather than an extension, and
  anything without one sorts last in both directions. It can also be your
  default, with `sort.column = "extension"` under `[ui.columns]` (#138).

- **The default preset binds all of it.** F4 edits and Alt+F4 keeps the old
  "open with the system handler"; Ctrl+F3..F6 sort; Alt+Enter shows properties
  and Ctrl+L counts a folder; Alt+T opens the tree; Ctrl+N opens a connection
  and Alt+N closes it. The four transcribed presets already bound these names
  and were waiting for the commands to exist.

- **The help serves an extension's own page.** `plugin.list` and `plugin.help`
  reach the graphical host, so an extension with a `help.md` gets a row in the
  sidebar and its page on demand — with the provenance line a third party's
  page always carries, including while it is still being fetched. The
  catalogue is asked for and *not* waited on: documentation is cosmetic, and a
  window blank until the daemon answers is worse than a sidebar that gains
  rows half a second later. An id that is not valid reverse-DNS is DROPPED at
  the entry point rather than masked — masking is not injective, so it would
  quietly map two extensions onto one row — and the alphabet that decides that
  now lives beside the wire type that carries it, so the manifest parser and
  every receiver ask one question with one answer.

### Changed

- **A cut plugin description now says it was cut.** It was truncated flush at
  280 characters; it ends in `…` like every neighbouring cut in the codebase.
  Cutting flush presents a truncated description as though it were complete —
  the same class of lie `plugin_label` has been avoiding since H3e, and the
  two are the same kind of text.

### Fixed

- **Four Fluent keys the graphical window asked for did not exist**, so it
  painted the key itself at the reader: every altered-name badge said
  `hostile-name`, an empty listing said `listing-empty`, a palette with no
  matches said `palette-empty`, and the viewer's lossy-decode marker asked for
  a key whose real name is `viewer-lossy`. No test caught it because the
  renderer's test catalogue is a fixture that invented the keys. There is now
  a test that reads the renderer's own source for every key it asks for and
  fails if the catalogue does not have it — in both languages.

- **Tab could not reach the sidebar or the process panel.** The shared focus
  order includes every focusable slot, and the window moved the focus there —
  and then snapped it straight back to a listing while reconciling roles, so
  the key looked like a toggle between two panes. The focus now moves only
  when the slot that had it stopped being valid (hidden, gone, not focusable).
  With that fixed, a second half surfaced: movement with the focus on the
  process panel still moved the *listing* beside it. Which surface a movement
  belongs to is decided by the shared kind registry's `takes_keys` — the
  details sheet is focusable and deliberately does not take keys, since it
  follows the listing's cursor and would stop following anything with the
  keyboard inside it.

- **`F1` with the viewer open opened a help nobody could see or close.** The
  viewer took keys before the help and painted over it opaquely, so the
  overlay was built, shipped, and then received not one keystroke — including
  the one that closes it. The help is opened last, so it goes last: first in
  the key routing, last in the document.

- **`F1` over a text prompt stole the keystrokes.** Opening the help over a
  dialog being typed into turned the `⌫` that fixes a typo into the help's
  "go back". It is refused now, as the terminal has refused it since H3c.

- **A dimmed help row ran anyway when activated with the keyboard.** The check
  lived only in the renderer, which does not attach a listener to a disabled
  row — but the keyboard does not go through the renderer. It lives in the
  host now, for both doors, and a refusal keeps the page open, because the
  page is where the explanation is.

- **Rows belonging to another screen were offered wrong in both directions.**
  Asking one flat command list meant a dialog's page came out entirely dimmed
  as "this window does not do it" while a working dialog was on screen, and
  the viewer's rows came out lit with no viewer open, only to refuse when
  pressed. Each row is now judged against the screen it belongs to.

- **F3 was dimmed on a symlink the viewer opens happily.** The fact the help
  dims by said "file", the code it describes refuses only directories.

- **Running a command from the palette did not close the palette.** It closed
  in the host's state and in the next full snapshot, but no patch said so: a
  renderer that applies patches — which is what the reference renderer does,
  and what the sequence exists for — kept the palette painted over the listing
  until something else, for some other reason, forced a snapshot.

- **Per-scheme column configuration was dead in the graphical frontend.**
  Columns were resolved once at start-up from the `file` scheme, so
  `[ui.columns.schemes.sftp]` never painted and — worse — its `attr:` ids were
  never requested, because the attribute list that travels with every listing
  had been frozen too. The host takes the whole `ColumnsSettings` now and
  resolves per pane, per scheme. A column id that does not parse is logged at
  start-up instead of vanishing.

- **Closing mid-copy reported that nothing was left undone.** The shutdown
  report only looked at whether the session had been written; a queued or
  running task now counts, which is what its own documentation always claimed.

- **Closing the window could hang it.** The shutdown ran on the event-loop
  thread and waited on a daemon round-trip with no deadline, so a stalled
  socket meant a window that stopped repainting and never closed — and killing
  the process is the one path that guarantees losing the session. Two seconds,
  then it closes and says the session may not have been written.

- **A layout with no listing killed the window on the first keystroke.** Not
  at start-up: the panic happened inside the actor's task, with no log and no
  visible crash, and every later action answered "the host is gone". It is
  refused at construction now, with a typed error (#242 on a new surface).

- **A slow viewer read no longer opens a viewer nobody asked for.** F3 on a
  file over a slow mount, then Esc, and seconds later the viewer appeared —
  and since keys route on "is the viewer open", the next keystroke was
  interpreted by a different keymap. The read carries a token, any listing key
  cancels it, and it has a deadline.

- **The start directory is no longer stat'ed on the runtime thread**, where a
  dead NFS mount blocked a worker for the mount's full timeout before the
  window existed (hard rule 2).

- **`--layout` and `--preset` keep their bytes**, so two different invalid
  names can no longer collapse to the same one (#246), and a value that names
  nothing is refused instead of silently falling back — the same file already
  refused a misspelled *flag* loudly.

- **Actions reach the host in the order they were made.** The renderer fired
  each `invoke` independently, so the `focus_slot` and `select_row` of one
  click could arrive inverted and the click would select nothing, now and
  then. They queue in a single chain.

- **An incompatible renderer stops sending.** It painted an incompatibility
  screen and kept dispatching keystrokes; and the outcome of the very first
  snapshot — the one message a mismatched renderer is guaranteed to see — was
  discarded, so the screen never appeared at all. A patch kind the renderer
  does not know now forces a resync instead of being dropped while the
  sequence advances.

- **The name you type is the name that gets created.** The `mkdir` field's
  text travelled through the *display* truncator, which cuts at 4 KiB and
  appends `…` — and `Segment::new` accepts an ellipsis, so a directory could be
  created with a name nobody typed. The host now keeps the typed bytes as the
  operand and projects a separate masked, bounded copy for painting; the
  renderer no longer writes that projection back into the field on every
  repaint. A name whose painted form differs from what will be created says so
  in the dialog — it is the one surface where a name is approved, and it was
  being shown raw.

- **The bridge's truncator no longer cuts inside a grapheme.** It cut on a
  character boundary and appended `…`, so an accent could migrate from its
  letter to the ellipsis and a ZWJ family emoji could be cut into unrelated
  people. It now uses the shared truncator in `norte-frontend`, which had
  carried the tested fix for this class since the H3b audit, and the test
  sweeps the hostile corpus instead of only asserting valid UTF-8.

- **Column ids, unsupported slot names and peer error text are masked.** All
  three reached the DOM raw: a column id comes from configuration (a project
  layer can name a `plugin:` column), a slot kind comes from a layout file and
  `KindId` validates nothing, and an error string from a newer peer is
  documented as "show it as-is". The status bar also painted a raw Fluent key
  (`err-not-found`) in a field whose contract says the host already translated
  it.

- **Text keys reach a text field again.** The renderer measured "one
  character" in UTF-16 code units, so an emoji or a decomposed `é` fell
  through, `preventDefault` ate it, and it could not be typed into a name.

- **A right-to-left filename no longer drags the hostile badge to the wrong
  side.** Hebrew and Arabic names are legitimate text and carry no control
  characters, so nothing flags them — and without bidi isolation they reorder
  the line box around the badge and the directory marker that describe them.

- **The size and date columns stopped filling after the first two hundred
  rows.** Each probing round is capped, and nothing asked for the next one: a
  tall window got 200 sizes and the rest stayed blank until the user scrolled.
  Worse, the round claimed its candidates as "already probed" *before*
  deciding whether to run, so an overlapping round marked rows nobody ever
  stat'ed and they were never asked for again. Rounds now re-arm until the
  window is done.

- **Probes are answered by the path that was asked for.** A provider may echo
  a different spelling of the same name — NFD on HFS+, another case on SMB, a
  symlink's target — and the reply then matched nothing while the requested
  path was already marked as probed, so that row's size stayed blank for good.

- **Probing a directory no longer costs one round trip at a time.** Up to
  eight run at once with a five-second deadline each, one round per pane, and
  a relisting cancels what is in flight — a hung provider used to stall the
  other 199 stats behind it and lose the whole batch.

- **Entering a large directory showed only its first hundred entries.** The
  listing's first page cleared the request token, and the background drain kept
  sending its batches with that same token, so every one of them was dropped:
  `/usr/lib` showed 100 rows and a resync did not help, because the entries had
  never been merged. Start-up worked only because it restored the token by
  hand. The drain has its own token now, and the hand-restore is gone.

- **Coming back to a directory left its size and date columns blank.** The set
  of already-probed paths was never cleared, so a re-listing — whose entries
  are lazy again — filtered every candidate out, permanently, for the rest of
  the session. It also grew by one path per file ever seen.

- **Two kinds of bridge patch could never be serialized.** `ViewChange::Tasks`
  and `ViewChange::Dialogs` wrapped a sequence in a newtype variant of an
  internally tagged enum, which serde refuses at run time: every task-board
  and dialog update would have failed on the wire the moment a non-Rust
  renderer existed. They are struct variants now, and the golden corpus covers
  every `ViewChange` one by one instead of two by example.


- **A comparison asks each directory, not just the two roots.** Under one
  `file://` there are mounts — an exFAT stick, an ext4 subtree in `+F`, a
  read-only bind — and the walk applied the roots' answer to the whole tree, so
  case collisions inside a mounted subdirectory were lost in silence. It asks
  per directory now, which costs nothing for backends whose locations are all
  alike (that is the trait default, no I/O) and is cached by directory identity
  in the one that really probes. The same question inside a copy —"is this
  move a rename onto itself?"— had the same defect and now asks about the
  destination LOCATION, folding with the shared key instead of a `to_lowercase`
  that is no filesystem's rule (#215).

- **A tree deletion checks how much is inside it, not just the folder.** A
  directory's `stat` only moves when its DIRECT children change, so a subtree
  that gained a hundred files two levels down between approving a plan and
  applying it revalidated clean and was deleted whole — the step with the
  widest blast radius had the weakest check. The witness now carries the count
  of its first level and the executor counts again before destroying. It still
  cannot see a change in a grandchild; that is written where the check is
  rather than assumed away (#176).

- **A name the destination cannot have blocks the plan instead of surfacing
  mid-copy.** Nothing checked that a name legal under the source root was legal
  under the destination's, so `CON`, `f:ads` or a trailing dot — all legal on
  ext4 — were discovered while writing. `f:ads` is the worst of them: on NTFS
  it SUCCEEDS, writing an alternate data stream, so the copy reports fine and
  the file is not there. The destination's provider decides, because it is the
  one that knows its rules, and the answer arrives as a plan blocker where a
  human can act on it. On the wire that is protocol **0.52.0** and **ADR
  0064**, which records the three decisions together (#163).

- **An undo that cannot happen now says which file is where.** An `Overwrite`
  writes two journal rows, and a `created` that failed after a `trashed` had
  succeeded left a batch whose undo blocks — it would have to delete a file it
  has no row for before restoring the buried one. Compensating means two more
  mutations down the path where the journal has already proven unreliable, so
  it is not compensated; it is reported, with the buried path and its place in
  the trash inside the error, which is the difference between "it failed" and
  "your file is here" (#206).

- **A link planted after indexing cannot feed a denied file to the embedder.**
  `index.embed` classified a candidate from the row `index.build` left behind
  and read it later, so anyone who could write in the indexed tree could
  replace a `.txt` with a link to a file under `denied_prefixes` and its first
  32 KiB went to the embedding provider — the one thing that module promises
  does not happen. What is read is checked again right before reading it, and
  anything that is no longer a regular file is skipped. A hard link still
  defeats the path filter without racing at all, and that is now written down
  where the check is rather than assumed away (#122).

### Added

- **What norte writes is now checked by tools that are not norte.** The archive
  round-trip read what this crate wrote with this crate's own reader — a fine
  encoder/decoder consistency check, and blind to every place where we and the
  rest of the world disagree, which is exactly where both format blockers of
  the archive branch lived. `unzip -t` verifies our zips and extracts them,
  GNU `tar` lists and extracts our tars and tar.gz, including a name past the
  100-byte ustar boundary. Missing tools skip with a message rather than
  passing quietly. The corpus grew the two names that straddle that boundary,
  which nothing had (#250).

- **A terminal that copied one file no longer holds the journal all day.** The
  embedded session took the lock on its first mutation and kept it until it
  exited, so a copy at 09:00 left `norte daemon run` and `norte audit` unable
  to open `journal.db` until the window was closed. It is released after thirty
  seconds without use and reopened by the next mutation — the reopen re-reads
  the chain from the file, which is what makes letting go safe (#179).

- **The graphical diff pane fills in an orphan's size.** A local listing
  arrives lazily and the comparison engine does not stat per entry, so the size
  cell of a file that exists on one side only — the row that decides whether it
  gets copied — stayed blank forever. The selected row is probed, exactly as
  the terminal does, and a probe belonging to a previous comparison is dropped
  by generation rather than landing in the current one's tables (#199).

- **A plugin that ran out of time no longer reports as a crashed plugin.** The
  guest's budget is measured in wall clock — a ticker advances the epochs — so
  a loaded machine spends it on a plugin that is merely slow, and that arrived
  as "internal error, the plugin panicked": the one reading that is certainly
  wrong. An expired budget is its own error now, and outside the host it
  becomes "unavailable, try again", which is what actually happened (#211).

- **A capability answer is cached per directory, not per connection.** Since
  the daemon began answering per location, the terminal was still keying that
  cache by scheme and authority — so the answer for `/home` was served for the
  exFAT stick, the `+F` subtree and the read-only bind mounted under the same
  `file://`. Nothing had broken yet because the read-only veto is the only
  reader today, which is exactly the kind of latency that makes the next flag
  the one that finds out. The cache is bounded now, because a key per directory
  is not bounded by the seven schemes that exist (#215).

- **A plugin's approval now covers its binary, not only its manifest.** Approving
  a plugin anchored what it asked for and when it fires — so editing
  `plugin.toml` after approval correctly forced a fresh consent — and said
  nothing about the code. Swapping `plugin.wasm` and leaving the manifest alone
  kept the approval, which is the same confused-deputy the anchor exists to
  close, entering by the other door of the bundle. The anchor is now the pair,
  so a changed binary asks again. **Every existing approval is reset by this**,
  deliberately: the question a human answered did not include "and this
  binary", so their answer does not cover what is being asked now (#241).

- **A `.git` in your home no longer hands a plugin your home.** The climb that
  finds a project root stopped at 64 levels and nothing else, so one stray
  marker at `$HOME` — a badly extracted archive, a careless installer — turned
  every folder of yours that is not a repository into a root covering the lot.
  It stops at your home now; a marker AT home still counts, since the ceiling
  is "no further", not "ignore what is there". The badge also names the marker
  (`location-root:.git`) instead of just saying `location`: what is granted is
  the nearest ancestor containing it, which in a repository is the whole
  project rather than the folder you have open (#241).

- **The root a plugin gets is the one that was checked.** Between deciding a
  path was the project root and opening it, the path was resolved again from
  `/`, following symlinks and unconfined — renaming a component in between
  swapped the root for whatever whoever could rename it wanted. The open now
  requires the same `(dev, ino)` the climb saw, and refuses otherwise (#241).

- **The session's schema version has one home now.** Two numbers described the
  same document and the documented one was read by nobody: the protocol says
  `Session.version` is the body's schema, and norte's own frontends instead
  wrote — and read — an undocumented copy inside the body. A client following
  the written contract (`version: 2` in the envelope, a v2 body) reached a
  reader that saw no copy, took it for version 0, dropped every field it did
  not understand and wrote the remains back. And the core accepted a `put`
  whose version it could not read, which wrote a file it would refuse from the
  next start onwards — a newer terminal against an older daemon killed
  persistence permanently, until the file was deleted by hand. The reader now
  takes the envelope's version (the greater of the two, so bodies already on
  disk still read), and the core refuses a schema it cannot read with "your
  daemon is older" instead of writing it. On the wire that is protocol
  **0.51.0** and **ADR 0062**; the two decisions protocol 0.49.0 made without
  one are recorded in **ADR 0063** (#247).

- **Counting a folder no longer counts anything twice.** `fs.dir_size` took a
  list of roots and summed them with no overlap check, so `/a` and `/a/b`
  together reported more than the space they occupy — the opposite of the
  question the method exists to answer. Overlapping roots are refused now, the
  same as `fs.compare` and `sync.plan` do, and the method's own documentation
  stops claiming a number it does not compute: it sums apparent size, does not
  deduplicate hard links, and has no way yet to say that an unreadable subtree
  made the total a floor (#247).

- **An abandoned listing no longer wedges the connection.** A daemon serves one
  request at a time per connection, and a read that the client stopped waiting
  for — the five-second budget the session restore now uses, or any dropped
  future — kept running against the stuck provider with everything behind it
  queued, each request dying at its own thirty-second timeout. The terminal
  came up, drew itself and did nothing, without saying why. Reads now send
  `rpc.cancel` when abandoned, like mutations always have, and the daemon acts
  on it for `fs.list`, `fs.stat`, `fs.read` and `fs.capabilities` — dropping a
  read leaves nothing half-done, which is why they can be cut at all (#248).

### Added

- **The places sidebar answers the mouse.** It shipped keyboard-only: clicking
  a drive or a favourite did nothing, because its cells belong to no listing
  and the hit test landed outside every pane. A click now selects the row and
  brings the keyboard over, clicking the selected row activates it — the same
  thing `Enter` does — and clicking a section header folds or unfolds it, which
  is what the arrow it already draws promises. Unfolding the drives asks for
  them again through the same path the key uses, not a fourth refresh trigger.
  The clickable rows are measured by the function that paints them, scroll
  offset included, so a click cannot activate the row next to the one under the
  pointer (#226).

- **The GUI's mirror can be narrowed to a selection.** The diff pane had no
  mark gesture, so `include` went out as "the whole tree" every time: under
  `Update` that was inert, but `m` turns every destination orphan into a
  `DeleteTree`, and the reader had no way to reduce it — the confirmation's
  count was the only thing standing between them and it. `ins` now marks a row
  there, the same key as the terminal, and what is marked is what the plan
  covers. A marked row says so to a screen reader too, not only with the
  asterisk (#249).

- **Holding the sync key no longer queues whole-tree walks.** Every press of
  `s`/`m` in the GUI's diff pane bumped a generation and started a fresh
  `sync.plan`, and a superseded plan is only cancelled when its own start
  event lands — a full round trip later. Holding `m` for two seconds against
  an SFTP pair started dozens of concurrent recursive two-tree walks before
  the first cancellation arrived. A request in flight now blocks the next one
  until its answer arrives, and on Wayland the auto-repeat is dropped before a
  request is even built. It could not be only the second: GPUI's X11 backend
  never reports a key as held (#249).

- **A layout with no file listing no longer panics the TUI.** A layout file
  that gave the listing's slot to another kind — `places`, `status`, anything
  — passed validation, and the frontend then seeded that slot with the kind
  the tree asked for, leaving the screen without a single listing. The first
  access by side panicked, in raw mode, on the alternate screen. Persisted
  into your UI session it panicked on **every** start until the file was
  deleted by hand. A tree with no `browser` slot is now a load error, in every
  door it can come through — the layout file, a preset, and the saved session
  — and the side list is rescued independently, so it can never point at a
  slot that stopped holding a listing (#242).

- **The processes panel has the keys it claimed.** It took the focus border
  and consumed nothing: the arrows moved the *file list* behind it, `F8`
  opened the delete dialog for that list's selection, and the `▶` sat on row 0
  forever — while the changelog and both help topics promised "cancels the one
  under the cursor". The panel now dispatches its own vocabulary: up, down,
  Enter to cancel the task under the cursor, Escape to hand the keyboard back
  without closing, and its own key to close from inside. The attributes sheet
  went the other way: it never wanted the keyboard — it follows the cursor —
  so its half-implemented focus state is gone and it opens and closes in two
  presses instead of three (#243).

- **A layout name is a filename, and is now treated as one.** Two bugs with
  one cause. `--layout` went through `to_string_lossy`, so `$'\xff'` opened
  `layouts/\u{FFFD}.toml`: the real file was unreachable and two different
  invalid bytes landed on the same one, silently. And a name was recomposed
  into a path and left to the OS to resolve, so on macOS or Windows a saved
  `Orthodox.toml` was what the row labelled *factory* loaded — the preview
  showed the preset and the user's tree was applied. Names now travel as
  `OsString` from the command line to the filesystem, the file is resolved
  byte-exactly against the directory listing, and the guard on what may be a
  layout name rejects what the old single-component check admitted: `C:` (one
  `Path` component on Windows, and `join` with it replaces the whole base),
  NTFS alternate streams, Win32 device names like `CON` and `NUL`, and
  trailing dots and spaces. Non-UTF-8 layout files also stop disappearing from
  the picker, and `MIO.TOML` stops being invisible there while `--layout MIO`
  loaded it (#245, #246).

- **Opening a layout no longer reads disk from the event loop.** Enter on a
  picker row called the loader from inside the loop, so with the config
  directory on a mount that had gone away, input, redraw, task progress and
  `Ctrl+C` all hung for the mount's timeout. The rows now arrive already read,
  which is also what gives the user's own layouts the preview the help had
  promised them — and a file that does not parse says why, in the place its
  screen would have been, instead of showing a blank half that cannot be told
  from an empty layout (#244).

- **The places sidebar can be resized again.** Its width was fixed at whatever
  it opened with: `layout.grow`/`layout.shrink` always resized the *focused
  listing*, so the branch that resizes a fixed-width panel was unreachable
  from any production path. With the keyboard inside a chrome panel — places,
  the directory tree, processes — those two commands now resize that panel
  (#244).

- **The transfer destination prompt stops corrupting what you type.** It is a
  wire address, where one invalid byte costs three characters, and it was
  capped at 256 of them — a cap chosen for a mark pattern — so a deep or
  hostile directory opened the prompt already over budget and every keystroke
  was a silent no-op. Backspace popped one character of the *text*, so
  retreating over `%C3%A9` left `%C3%A`, which no longer parses: one press
  did not delete one letter, it broke an escape. And confirming without
  editing submitted a copy of every mark onto itself, which surfaced as N
  failed tasks instead of one line in the dialog (#246, #244).

- **A detached window says so for as long as it lasts.** A window that does
  not own the session — a second window, a core without the lock, one that
  found a screen written by a newer binary — said it once at startup, and the
  next message erased it. From then on it silently stopped saving your screen.
  It is now a permanent indicator in the status bar, alongside the journal and
  degraded-connection ones (#232).

- **A dead panel no longer hangs the startup.** Restoring your session listed
  every panel in turn with no deadline, on a path that runs before the event
  loop exists — so a panel left on an unreachable SFTP or NFS mount hung norte
  before `Ctrl+C` was even wired, and the only way out was another terminal.
  The restore now runs the listings together under one five-second budget, so
  a dead remote costs the wait and not the healthy panels beside it, and a
  panel that did not list says `[not listed]` in its own frame until something
  lists it — an empty listing you cannot tell from an empty directory is a
  screen that lies (#235).

- **`fs.compare` gets its tracing span back.** Adding `connection.close` and
  `fs.dir_size` inserted the two handlers between an attribute and the
  function it decorated, so `fs.compare`'s `#[instrument]` and its whole doc
  block landed on `connection.close`: comparing silently stopped emitting its
  span, and `connection.close` published rustdoc describing gates it does not
  have. Both are back where they belong, and `fs.dir_size` has one now too
  (found by `protocol-guardian`).

- **A plugin with `location = "read"` can no longer read through a protected
  root.** Confinement bounds a plugin from above and said nothing about what
  lies below, so a perfectly ordinary root contained everything that matters:
  with a panel open on your config directory, an approved columns plugin could
  read `norte/secrets.age`, `norte/journal.db` and `norte/connections.toml`,
  and with a panel on `/` it could read the disk as you. The protected roots
  now travel into the confinement itself and are enforced by `(dev, ino)` on
  every directory of the path — a symlink pointing at one resolves to the same
  inode and is refused the same way (found by `security-reviewer`, #238).

- **An agent no longer escapes its scope by one directory through a columns
  plugin.** The location handed to the plugin is the parent of the page, and
  it was never gated — the comment claimed the paths' gate covered it, which
  it does not, because a scope's own root is inside its scope. An agent scoped
  to `~/work` could ask for columns over that root and hand the plugin a
  confined root over `~`. The parent now passes the read gate on its own; when
  it does not, the plugin runs without a location and its column comes back
  blank, rather than the call failing and turning the column into an oracle
  for what exists outside the sandbox (#239).

- **A FIFO can no longer wedge the daemon's blocking pool.** The location read
  checked the node type *after* opening, and `open(O_RDONLY)` on a FIFO with
  no writer never returns. A hostile archive carrying `.git/index` as a FIFO
  cost one blocking thread per repaint, and wasmtime's epoch deadline cannot
  interrupt that. Opens are `O_NONBLOCK | O_NOCTTY` now. The same read also
  bounded itself by `st_size`, which a file being appended to — or any FUSE
  mount the user controls — can lie about; it is capped for real, and a file
  that outgrows the cap while being read is refused rather than truncated
  (#240).

- **A symlinked `.git` no longer widens a plugin's root.** The project-root
  marker was accepted on `symlink_metadata().is_ok()`, so a dangling
  `ln -s /nada /tmp/.git` — and creating a name in `/tmp` is available to
  anyone — made every panel under `/tmp` hand the plugin all of `/tmp`. A
  symlink is refused; a real `.git`, directory or worktree `gitdir:` file, is
  not (#241).

- **A detached core no longer accepts a screen it cannot save.** `session.put`
  against a daemon that is not the writer answered `Ok` and kept the body in
  memory, where the embedded half had always refused it outright. That stopped
  being harmless the moment the daemon's writer could take the lock late: a
  body accepted while detached outran the revision on disk, survived the
  adoption, and was published over the other window's saved screen in the same
  tick — undetectably, because the revision only ever rises. It is
  `PermissionDenied` now, on both halves (found by `security-reviewer`).

- **A daemon that starts while another holds the session retries.** It computed
  "can I persist the session?" once, at bind, and answered from that for the
  rest of its life — so a daemon that started during a handover never wrote
  your screen again, even hours after the other process was gone and the file
  had been free the whole time. Its session writer now retries the lock every
  second, and on taking it adopts the document from disk, which is what the
  embedded half already did (#237).

- **The session write policy is shared, not the TUI's.** What to send, what not
  to resend, when to ask for ownership again and what to trim before writing
  lived inside the terminal's event loop; the GUI would have reimplemented all
  of it, trimming bugs included. It is now `norte-frontend`'s, with the field
  that always travels empty documented as such (#236).

- **An agent cannot reach your session file.** The policy engine protects the
  directory holding `journal.db` and the sync spools, but the screen norte saves
  lives in a different one — `~/.local/state/norte` — which was outside it. An
  agent granted a scope over `$HOME` could therefore read `session.json`, which
  is the list of every directory you have visited, or delete the lock beside it
  and leave two norte windows both believing they were the only writer. Both
  directories are protected now.

- **Navigating no longer stutters once a second.** Saving the session ended in
  an `fsync`, or in a round trip to the daemon, and the terminal was not reading
  your keys while that was in flight — once a second, and precisely while you
  were moving around, which is when there is something to save. The write now
  happens in a task of its own (#230).

- **A second window is no longer stuck as a copy forever.** Open two norte
  windows and the second one runs detached: it says so and does not overwrite
  the first one's screen. If the first one then closes, the second now takes
  over saving within half a minute, instead of spending the rest of its life
  unable to save and losing its screen on exit (#234).

- **Quitting saves where you actually were.** The session was written once a
  second and not on the way out, so closing norte right after a `cd` — or from
  the quit dialog — stored the directory you had left. It now takes one last
  snapshot before the process goes.

- **Two windows no longer cost each other their history.** When two windows
  raced to save, the one that lost re-read the session and rewrote its own over
  it, dropping the panels the other one had been keeping. What only the other
  window had is now kept (#231).

- **A write that arrives while the daemon is shutting down says so.** It used to
  be accepted and answered with a new revision, for a file nobody was going to
  write any more (#233).

- **A small terminal no longer leaves you with a screen full of panels and no
  files.** The `explorer` and `full` screens are built from docked panels with
  fixed sizes — a sidebar, a viewer column, a processes panel — and in a 40x10
  terminal those sizes added up to more than the screen, so the file listings
  were squeezed to nothing and what was left was three panel headers. A frame
  that cannot show a listing now sets aside the biggest docked panel on the axis
  that is short, and only that one: at 40x10 `full` gives you the sidebar, two
  listings and the status bar. Nothing is saved or forgotten — grow the terminal
  and the panels come straight back (#229).

- **A sidebar you can widen.** Grow and shrink did nothing to a panel with a
  fixed width, which is every sidebar, so the places panel was stuck at the
  width it opened with. It now moves two columns at a time (#227).

- **A plugin-backed connection no longer dies after ten seconds.** The time
  budget a plugin gets was being handed out once, when the connection opened,
  instead of once per operation — so an FTP session stopped answering ten
  seconds after you opened it, and said the plugin had crashed, which it had
  not. Each operation now gets its own budget.

- **Two names that a Linux volume calls one file are recognised as such far
  more widely.** On ext4 or f2fs with case folding turned on, `ﬁle.txt` and
  `file.txt` are the same file, and so are dozens of Greek and Armenian pairs.
  norte only knew about the German `ß`/`ss` case and a handful of Latin
  ligatures, so a copy planned against such a volume could be approved with no
  warning and collide on arrival. It now knows every expansion the standard
  defines.

- **A dead network mount no longer wedges comparing, synchronising or asking
  what a folder supports.** Those three questions each ask the filesystem what
  it can do, and on a hung NFS or SMB mount that question never comes back —
  the whole request waited forever with nothing to cancel. It now gives up
  after a fifth of a second and answers with what it already knew.

### Added

- **The screen you left is the screen you get back.** norte now remembers the
  arrangement, the directories, the cursor, the history, the sort and whether
  hidden files are shown, for every panel, and gives them back when you start
  it again — across a daemon that was replaced under you. A second window opens on the same
  screen and then goes its own way: it says so when it opens, and it never
  writes over the first one's state. Neither does an older norte started on a
  session a newer one wrote — it starts from your configuration and leaves the
  file alone. On the wire that is protocol **0.48.0** (ADR 0059): `session.get`
  and `session.put`, with a body the core stores, versions and hands back
  without reading.

- **Five screens to choose from, instead of one.** `orthodox` is what norte has
  always looked like and still the default. `simple` is one panel, for a narrow
  terminal or a shared screen. `krusader` adds the places sidebar to the two.
  `explorer` is one panel with the sidebar, the docked viewer and a processes
  panel. `full` turns everything on. Pick one from the layout list, start in one
  with `ntc --layout <name>`, or set `[ui] layout` and always get it. Each row
  of the list draws the screen it would give you, from the layout itself rather
  than from a picture saved beside it, so it cannot go stale. A layout named
  after a file manager does not touch your keys: the layout and the keymap
  preset are separate settings, and the list says so where the names meet.

- **A processes panel, and an attribute sheet.** The processes panel gives every
  running task a row with its progress and cancels the one under the cursor —
  the strip at the foot of the screen stays exactly as it was, and the panel is
  what you open when you want to act on a task rather than watch it. The
  attribute sheet shows what is known about the entry under the cursor and
  follows it as you move, reading nothing to do it: everything it shows was
  already in the listing.

- **A copy with nowhere obvious to go now asks instead of failing.** In a layout
  with a single listing there is no other panel to copy into, and with three
  there is no obvious one either. Copying now opens a prompt for the destination
  address, prefilled with the panel's own, in the same form the hotlist takes;
  edit its tail and press ⏎ and it is the ordinary confirmation from there. A
  destination is never guessed, because copying into a panel you did not have in
  mind is silent data loss.

- **A sidebar with your drives and your favourites, and a viewer that follows
  the cursor.** `Alt+b` opens a panel down the left with every mount and how
  much room is left on it, plus the favourites you have saved; `Enter` on a row
  sends the focused listing there. It is a control, not a third pane: the
  listings do not move, the destination of a copy does not change, and a second
  press moves the keyboard into it so you can pick with the arrows.
  A favourite whose path no longer parses is marked and dimmed instead of
  quietly disappearing, because a favourite that hides itself is a
  configuration mistake you cannot see; press it and the status bar says what
  is wrong with it. Drives are asked for when the panel opens and when you
  unfold their section — never on a timer, because asking every filesystem how
  much room it has left, every few seconds, is felt on a network mount.

  `Alt+q` opens a viewer on the right that shows whatever the cursor is on,
  and keeps up as you move. It is the same viewer `F3` opens — same keys, same
  encodings, same hex, same plugin previews — sitting in a slot instead of over
  the whole screen, so pressing `Alt+q` again moves the keyboard into it and a
  third press closes it. It never interrupts: a directory is not read, a file
  it may not read paints the reason where the text would go instead of raising
  a dialog for every key you press going down a listing, and one you cannot see
  — behind a tab, or squeezed out of a small terminal — reads nothing at all.

- **A git status column, and plugins that can finally say something about a
  file.** Turn it on in the extension manager and the panel marks what changed
  since your last commit: `M` for modified, `D` for deleted, `?` for untracked,
  `!` for ignored, nothing at all for clean, and a folder shows the strongest
  thing inside it. It reads `.gitignore` too, otherwise `target/` alone would
  drown the column.
  Until now a column plugin only ever saw the *names* on screen, which is why
  the only one that existed counted characters. It can now be handed a way to
  read where you are — and only that: a handle to the folder, never a path, and
  the operating system itself refuses anything that tries to climb out of it.
  A plugin has to ask for the permission in its manifest, you approve it by
  name in the extension manager, and a plugin that adds the permission later
  has to be approved again.
  The git column is installed exactly the way somebody else's plugin would be,
  because that is the path worth proving. It reads only what it needs: the
  index git already keeps, and the file itself only when timestamps cannot
  settle the question.

- **`.rar` archives open as folders.** Press Enter on one and it browses like
  any other archive, with the files readable inside it. norte does not contain
  a RAR decompressor — that code is not free, and no amount of wanting changes
  it — so it asks a program you already have: `7z` (from p7zip) or `unrar`,
  whichever is installed, `7z` first because `unrar` cannot print a filename
  that is not valid text and loses everything after the first odd byte. If
  neither is installed, opening a `.rar` says so and names what to install,
  instead of showing you an empty folder.
  Reading only: writing a RAR needs the proprietary half. Encrypted entries are
  listed but will not open, because the password prompt is deliberately
  unreachable — the helper program runs with no keyboard attached to it, no
  environment, and a working directory somewhere empty that is not your files.
  A `.rar` sitting on a remote server or inside another archive is refused
  rather than quietly downloaded whole. If you want to choose the program
  yourself, `[archive] rar_delegate` in your `norte.toml` does it — and it is
  ignored from a repository's own config file, because a folder you happen to
  `cd` into does not get to pick which binaries run. On the wire that is
  protocol **0.47.0** (ADR 0056): `rar` joins the composed-scheme whitelist,
  which is what a client may OFFER — it does not stop an older one from
  parsing such a path it is handed.

- **Upgrading norte no longer drops what you had open.** Replacing a running
  daemon used to look identical, from a window's point of view, to somebody
  stopping it: the connection closed and that was all anyone knew. So the
  windows sat there reconnecting to nothing, because reconnecting on your behalf
  after *you* stopped the daemon would be worse — it would restart the thing you
  just asked to stop. The daemon now says which of the two is happening before
  it goes, and the windows come back on their own when a replacement is
  expected, with their running tasks picked back up. Stopping it still means
  stopped.
  **A replacement waits for your files.** Asking for one while a copy or a
  synchronisation is running is refused, and says how many are still going, so
  nothing gets killed halfway through a folder. Wait for them, or stop the
  daemon the ordinary way, which cancels them deliberately.
  What does not survive: a synchronisation you approved but had not applied has
  to be planned again, because a plan belongs to the connection that approved it
  — deliberately, so nobody else can redeem it. `norte daemon stop --handover`
  is the switch, for whoever is doing the replacing. On the wire that is
  protocol **0.46.0** (ADR 0055): `daemon.going_away` says which of the two is
  happening before the socket closes.

- **The graphical interface notices changes it did not make.** Something else
  writes a file into a folder you are looking at — another program, a download,
  a `git checkout` — and the pane now updates on its own, as the terminal
  interface has done for a while. Same limits, and they are worth knowing: only
  local folders are watched (a remote or archive pane still refreshes on
  demand), and where the system runs out of watches norte says so in the status
  bar and falls back to checking every couple of seconds, which notices files
  appearing, disappearing and being renamed but not a file being edited in
  place (#106).

- **Copying a file full of holes no longer fills them in.** Disk images, virtual
  machine disks, database files and anything else stored sparsely used to arrive
  at the destination with every empty byte written out — a 64 GB image that
  occupied 2 GB became a 64 GB image occupying 64 GB. The copy now leaves the
  holes as holes. It reads back byte for byte identical either way; what changes
  is what the disk keeps.
  Two honest limits: the source is still read in full, so this saves space and
  not time, and the unit is the block norte copies in — a hole smaller than one,
  or straddling two, is still written out.

- **There is a log, and `norte doctor` tells you where it is.** norte kept
  diagnostics about what it was doing and had nowhere to put them: the terminal
  interface could not print them without corrupting its own screen, so it
  discarded them, and the graphical one did the same. Both now write to a
  rotating file under your state directory, a week of them kept, and neither
  writes a byte to the screen it is drawing on. `[log]` in `norte.toml` moves
  the directory or changes how many are kept.
  **Nothing leaves the machine.** There is no upload, no telemetry and no
  network in any of this — the point is that a bug report from someone who is
  not us can now include what actually happened. FTP passwords are capped out of
  the log at the filter, which is now tested against the file and not only
  against the terminal, because a password in a file that persists is worse than
  one that scrolled past.

- **A recursive copy cannot be redirected out of the folder you pointed it at.**
  Approve a synchronisation or a copy of a folder, and between saying yes and
  the bytes landing there was a window: anyone who could drop a symbolic link inside the
  destination — a shared directory, a network mount, a machine with other
  people on it — could make a subfolder of it point somewhere else entirely, and
  norte would follow it and write outside, with the daemon's permissions. It
  affected copying and creating folders; deleting and renaming already dodged it
  for reasons of their own.
  norte now opens the destination **once** and works underneath it by name from
  there on, so there is no path left to re-resolve between the check and the
  write — which is the only way to close this rather than narrow it, since any
  "look before you leap" check has that window by construction. A component that
  tries to leave the destination fails and is reported as a conflict; the file
  is not written. Symbolic links **inside** the destination are still followed,
  because forbidding them would break ordinary trees and buy nothing. This is
  #164, closed on Linux and macOS for the two operations it was reachable
  through: copying into a folder, and creating one.
  **Two edges of it are still open, and are named rather than glossed over.**
  Copying a *single* file still resolves its destination by path — the honest
  anchor there is the permission scope rather than a folder, which is a larger
  change (#219). And when a copy is set to overwrite, the deletion it performs
  first still goes by path, so under the same attack it can destroy a file
  outside the folder even though nothing is written there (#218). Neither is new;
  both used to be the whole picture.
  **Where it cannot be done, norte says so instead of pretending.** Windows has
  no equivalent call yet (#217), and a remote destination — SFTP, an object
  store — resolves names on the far side where norte has no say. Those copy the
  way they always did, and the confirmation dialog tells you, on one line, above
  the keys, that this destination cannot confine its writes. It never refuses:
  refusing would strand every destination that cannot offer the defence, which
  costs far more than the race it avoids. On the wire the destination's answer is
  the `confined_writes` capability, per location, which is what ADR 0054 is for.

- **A collision is judged against the volume it will land on, not against
  whatever the program happened to ask first.** norte asks a filesystem about
  *the directory in question* rather than about itself, so comparing your home
  directory against a USB stick no longer answers the home directory's rules
  for both. Two files called `README` and `readme` on a stick that cannot tell
  them apart are now reported as the collision they are, and on Linux the
  directories that fold names the *expanding* way (ext4 and f2fs with
  case-insensitivity switched on) are recognised too: there `straße.txt` and
  `strasse.txt` are one file, and the batch rename planner now says so before
  you approve a plan that would die halfway through (#153, #145, ADR 0054).
  **Finding this out never writes anything.** norte asks the kernel, and where
  the kernel has no answer — tmpfs, btrfs, XFS, network shares — it says so and
  keeps the filesystem's declared behaviour rather than creating a probe file in
  a directory you only asked it to read about. A read-only mount and someone
  else's directory get an answer instead of a shrug.
  On the wire that is protocol 0.45.0: two capability flags (`FULL_FOLD`,
  `CONFINED_WRITES`), one conflict subtype (`escapes_root`) and one pairing
  transformation (`full_fold`) — all additive, all of them read by a 0.44 client
  as "something I do not know" rather than as something wrong. A pairing that
  only holds because one side expands is reported as its own kind precisely so
  nothing downstream reads it as "these two names are the same text": on the
  other volume they are two files, and a synchronisation must not overwrite one
  with the other.

- **Synchronise two directories, one way, and be told what you cannot take
  back before you say yes:** `Ctrl+y` over the two panes plans a
  synchronisation, and inside the **terminal interface's** diff pane `s`
  chooses *update* (copy what is missing, overwrite what differs) and `m`
  chooses *mirror* (that, and delete what the source does not have). Either way
  you are shown every step it intends to take before anything moves. Mark rows
  in that pane first (`Ins`) and the plan covers only those, subtrees included.
  There is now a second diff pane, in the graphical interface (below), where
  `Ctrl+y` plans an *update* and applies it — the two keys that choose the mode
  from inside that pane are still terminal-only (#188).
  What makes this different from a scripted copy is that **the plan you
  approved is the plan that runs.** norte keeps it, and approving sends back
  nothing but a fingerprint of it, so there is no path by which a different
  intention arrives between the screen and the disk — not from a bug, not from
  an agent, not from a client that decided to be clever. The plan is retained
  for ten minutes, can only be applied once, and is discarded when it is
  applied, when it expires, when you disconnect, when the daemon restarts, or
  when too many pile up. Since the world can move inside those ten minutes,
  norte re-checks each file it is about to overwrite or delete against what it
  looked like when you approved, and reports a conflict instead of destroying
  something that changed underneath you.
  **The headline is what the destination's trash can give back, not a count of
  irreversible steps**, and that distinction is the whole reason this took the
  shape it did. A plan of nothing but copies onto a disk with no trash looks
  identical, step for step, to the same plan onto one with a trash — and one of
  them undoes completely while the other undoes nothing at all. So norte says
  which of the three cases you are in: everything comes back, or what was
  replaced is in the system trash where you can fish it out by hand (macOS and
  Windows), or it is gone. The confirmation question changes with the answer
  rather than reading the same over all three.
  It is journalled, so `undo` reverses the whole batch — copies, overwrites and
  deletions together, in the right order — and where it cannot, it says which
  files it left alone rather than reporting success over a half-undo. On Linux
  and BSD this now works at all, which it previously did not: norte implements
  the freedesktop trash itself instead of delegating, so it knows exactly where
  it put each file and can put it back. Previously an undo could restore the
  *new* file over itself and leave your original in the trash, and call that
  success.
  Every step carries the criterion that decided it and how much that criterion
  proves, so a mirror that deleted something can tell you it did so because two
  dates differed and not because anyone verified the contents. Sizes are an
  honest lower bound: a plan reads "1.2 GB, plus 340 files whose size the
  provider would not give", never a confident total built out of zeroes. It is
  a normal task — progress, and `Ctrl+K` cancels — and cancelling leaves a
  clean destination and a closed, undoable batch, never a half-written file.
  Overlapping folders are refused before a single byte moves: the same folder
  twice, one inside the other, the same folder under two spellings on a
  case-insensitive disk, and a symlink pointing at the other side.
  On the wire that is protocol **0.40.0**: `sync.plan` and `sync.apply` as
  cancellable tasks, `sync.steps` and `sync.plan_done` streaming to the
  connection that asked and to no other, `sync.report` for what happened
  (ADR 0049).
  **What this is not, on purpose.** It is **one-way**. There is no two-way
  synchronisation, because "both sides changed" needs a rule for choosing and
  there is nothing here honest enough to make that choice for you. There is
  **no resume**: a cancelled synchronisation is undoable and re-plannable, not
  continuable. There are **no conflict rules** beyond the one switch — what to
  do when the comparison could not verify its own answer. And it is the
  terminal interface only, over a daemon: no command line, no agent surface, no
  graphical one (#161, #162), and **not in the standalone TUI**, which has no
  journal — the key is shown greyed with that reason rather than hidden,
  because synchronising without an undo is not a convenience worth having
  (#167). Two limits worth saying out loud: a name that is legal at the source
  and not at the destination is reported when it fails rather than when you
  approve (#163), and the re-check before deleting a folder looks at the folder
  and not at every file inside it, so something added deep within it after you
  approved goes with it.
- **Compare two directories, and be told how much the answer is worth:**
  `Shift+F2` compares the two panes and opens a diff pane over both of them —
  one row per pair, and each row says not only *what* was concluded but *which
  criterion* concluded it and *how much that criterion proves*. This is the
  point of the feature. Two different sizes prove two different files. Two
  dates nine seconds apart suggest it and prove nothing: a restored backup, a
  `touch` and a real edit look identical at that rung. An entry inside a zip
  has no size and no date you should trust, and the honest answer there is
  "the provider cannot say" — which norte shows as an answer, with its own
  glyph, rather than as an error or as a comforting "same". So a comparison
  against an archive or an object store completes normally instead of filling
  the screen with failures that are not failures. Both columns are ASCII
  glyphs, not colour, so "same, verified" and "same, probably" stay
  distinguishable to a reader who cannot see colour.
  The comparison is cheap first and expensive only if you ask: presence, kind,
  symlink target, size, then date; a full sha256 of both sides runs **only**
  when you request it, and an agent needs the content permission to ask for it
  at all. It is a normal task, so it shows progress and `Ctrl+K` cancels it,
  and it walks with bounded memory rather than holding two whole listings — a
  directory that cannot be read becomes one row and the walk carries on
  instead of dying at leaf 40 000.
  In the pane you filter by category, jump into either side, and choose which
  side an action means — the active side is one you pick, never one norte
  infers from the row, because guessing on a destructive operation is not a
  feature. If the answer came back incomplete, it says so instead of painting
  "done" over a partial one.
  On the wire that is protocol **0.39.0**: `fs.compare` returns a task and
  `compare.rows` streams the rows, in batches, to the connection that asked and
  to no other (ADR 0048).
  **What this is not, on purpose:** it does not change anything. There is no
  synchronisation plan, no journal entry and no undo, because nothing is
  written — that is the next piece of work. Symlinks are never followed
  (norte refuses the request rather than quietly ignoring it, and compares link
  targets as bytes). And it is the terminal interface only: no command line and
  no agent surface. (The graphical one arrived after this note was written —
  see below, #158.) One known limitation worth
  saying out loud: inside tmux, `Shift+F2` does not reach norte at all — nor do
  the long-standing `Shift+F6` and `Alt+F7` — while plain function keys work
  (#159). Until that is fixed, reach it from the command palette.
- **Synchronising from the graphical interface:** `Ctrl+y` over the two panes
  plans the same one-way synchronisation the terminal does, shows every step
  with the same three glyphs — what it is, how sure the comparison was, and
  what undoing it would give you back — and asks before it writes. The question
  it asks is the shared one, so it counts the deletions and says whether they
  go to a trash you can restore from; a plan it will not let you approve gets
  an explanation instead of a shorter prompt. Pressing `Esc` after you approve
  and before the daemon answers now cancels rather than closing, and a report
  that arrives with the panel already gone is still shown — because it is the
  only record of what was written and of the undo that exists (#161).

  This lands *update* only: choosing *mirror*, the mode that deletes, still
  needs the gesture in the diff pane, and that is #188 rather than a key away.

- **The diff pane, in the graphical interface too:** `Shift+F2` — `Alt+D`
  under the `far` and `norton` presets, which never had a comparison key to
  transcribe — now compares the two panes in the GUI and opens the same diff
  pane the terminal interface has had: the same rows, the same two ASCII
  glyphs for verdict and confidence, the same five category filters with their
  counts, the same choice of which side an action means, and the same footer
  that says whether the answer is complete rather than painting "done" over a
  partial one. Both interfaces now take that last decision from one shared
  rule instead of each deciding for itself — the command line and the agent
  surface each re-derived it and each got it wrong, reporting a complete
  answer for a run that had lost batches, and this was the last surface left
  that could have made the same mistake a third time.
  Roots and filenames are shown the way the file list shows them: sanitised,
  and marked when sanitising changed them. The pane's header keeps each root
  in its own element with the `↔` in a third, so a directory named with an
  embedded `↔` cannot read as a different pair of roots and a long left root
  cannot push the right one off the screen unmarked. The terminal's header is
  still built as one string and does not have that protection (#185).
  Synchronising from this pane is the next piece (#161): `s`, `m` and `Ins` do
  nothing here yet, and neither does the mouse — selecting and navigating are
  keyboard-only in this pane for now. Closes #158.
- **A number before a key repeats it, on the presets whose originals do that:**
  typing `5` then `j` under the `vim` preset moves down five rows, `12` then a
  page key turns twelve pages. The number is visible at the status bar while
  you type it, `Esc` cancels it, and a key that misses clears it — a count can
  never end up glued to the keystroke after it. A count over a command that
  does not take one runs the command once and says so rather than swallowing
  the number: `3` then a quit key quits, and tells you the 3 was ignored.
  It is opt-in per preset, so `orthodox` and `cua` are unchanged and their
  digit keys still mean what they meant. **Two things a `vim` user will
  notice.** Digits are now counts there, so on the GUI a bare digit starts a
  count instead of opening the type-to-filter quick search. And `12gg` does
  **not** mean "go to line 12": norte repeats the command twelve times, and
  twelve "go to the top" is still the top, so norte reports the count as
  ignored instead of pretending. Absolute positioning needs a command that
  takes a line number, which does not exist yet (ADR 0044).
- **A keymap that would break pane switching, or that fights its own counts,
  now fails to load with a diagnostic** instead of doing something unexpected
  under your fingers. `Tab` is reserved for switching panes on the file
  screen — a preset or a layer that binds it to something else, or that merely
  starts a two-key sequence with it, is rejected at load, because a `Tab` that
  sits waiting for a second key has lost you pane switching just as
  completely. And with counts enabled, binding a bare `1`-`9` is rejected too,
  since the key cannot be a digit of a count and a command at the same time.
  `0` stays bindable: a count never starts with zero. Both are reported by
  `norte doctor` as well.
- **A batch of renames is one transaction, so a permutation finally works:** AI
  rename used to apply its plan one `fs.move` at a time. Ask it to number a
  season of episodes correctly and the very first move refuses, because the name
  it wants is the name the next file still has — a permutation could never
  succeed, and a permutation is the ordinary case. There was no preview of the
  whole plan either, so the first collision was discovered halfway through, and
  undoing what had already landed meant walking back a list of individual moves
  by hand.
  The core now decides the whole thing before anything moves. It reads the
  directory, works out an order in which each name is free when it is needed,
  inserts a temporary of its own where a cycle has to be opened, and reports
  every collision it found against the plan as a whole — a name two rules both
  want, a name already taken by a file nobody is moving, a source that is no
  longer there. Then it runs the plan as ONE task and records it as ONE undoable
  unit: undo is a single step, and a batch that fails partway renames back
  everything it had already done. Cancelling one does the same, so the directory
  is never left half-renamed.
  Names are compared the way the destination directory compares them, asked of
  that directory and never guessed from the operating system: two spellings of
  the same accented name are the same name where the filesystem says so, and a
  name that is not valid text is compared byte for byte and never normalised.
  Both frontends now apply the AI plan through this path.
  On the wire that is protocol **0.36.0**: `fs.rename_batch_plan` returns the
  reviewable plan, `fs.rename_batch` executes the plan a human approved,
  `fs.rename_batch_report` says what a failed batch managed to put back, and a
  filename now crosses on its own as a percent-encoded segment rather than as a
  string. New task kind `RenameBatch`, new errors `PlanStale` (the directory
  changed between the preview and the confirmation — re-plan and look again) and
  `PlanNotExecutable` (the plan had collisions, so nothing was attempted).
  `policy.undo_report` gained `batch_stuck` and `compensations_lost` for an undo
  that could not finish putting a batch back, and the approval prompt gained
  `paths_total` so a request over many paths can say how many. See ADR 0042.

- **The journal groups a batch under one id:** it gained a `batch_id` column, so
  the entries of one batch are recognised as one unit by undo instead of as a
  pile of unrelated renames — walking those back one at a time is exactly the
  thing that cannot work. The migration runs on open and leaves an existing
  journal's entries hashing precisely as they did, so the tamper-evident chain
  still verifies across it. One caveat worth knowing: a journal that has been
  migrated is read as `Broken` by a norte binary older than this release, which
  is [#127](https://github.com/compilando/norte/issues/127).

- **The graphical help answers where you ARE, from every screen:** `F1` opened
  the index no matter what was on screen. It now opens the page about the
  screen you are on — the viewer, the collision dialog, the rename prompt, the
  AI plan — through a table that a new modal cannot compile without extending,
  so nobody can add a screen without deciding which page explains it.
  Two places the key could not even reach: with a dialog open the modal ate it,
  and with the viewer open its own resolver did. Both now reach the help, and a
  help opened over a dialog takes the keyboard while it is up — the dialog
  stays painted underneath, so the question is never hidden, only unanswerable
  until you close the page about it. Nothing gets confirmed through a page
  covering the question.
  From the command palette, `F1` on a row opens that command's page — the other
  direction of the bridge that already carried your filter INTO the palette. A
  row nothing documents says so instead of dropping you in the index, where you
  could not tell whether your command was in there somewhere or simply
  undocumented.
  The help's scrollbar can be dragged now, and clicking the track jumps there.
  The sidebar's stays an indicator on purpose: its window is derived from the
  cursor, and moving the cursor OPENS the page it lands on — a scrollbar that
  changes what you are reading is not a scrollbar.

- **The graphical file listing lines its columns up again:** size and modified
  sat wherever the name happened to end, so every row put them in a different
  place while the header — painted outside the list, and therefore full width —
  lined up with nothing. The list virtualiser hands each row a definite space
  but leaves the row itself auto-width, so the name column had nothing to
  absorb; rows now take the width they were given.

- **The graphical help is readable, navigable with the mouse, and sized to your
  window:** what a screenshot showed and no test could. A paragraph did not
  wrap — it ran off the panel and was cut mid-word — and rows painted over each
  other, so a long page appeared duplicated and overlapping. Text is now laid
  out as text: one run per line that breaks at a word, with table rows kept in
  columns instead of run together.
  The overlay was a fixed 720×520 box, a stamp in the middle of a large window;
  it now takes 86% of the window, up to a width where a line of prose is still
  a line of prose. How many rows fit is derived from that size rather than
  guessed at, which is what used to let the sidebar cursor walk off the bottom
  edge and vanish while the keyboard said it had moved.
  The mouse works: click a topic to open it, click a runnable row or a link to
  activate it through the same path `Enter` uses — so a row the keyboard
  refuses is a row the mouse refuses — and the wheel scrolls the page. Both
  panels show a scroll indicator when there is more than fits. Dragging that
  indicator is not wired yet.
  Two things that were simply hard to read: links and command names took the
  colour of the quick-search highlight, which a theme guarantees against ITS
  background and not against this panel, and the sidebar drew group headers
  almost exactly like the pages under them. Secondary text is now derived from
  the prose colour, links are underlined as well as coloured, and a group looks
  like a group. The page title moved out of the scrolling body into a header of
  its own, so a long page stays labelled.
  A command that no key runs now paints its NAME and never its dispatch key:
  the dialog verbs live in a different catalogue, and the page about answering
  a dialog used to read `dialog.confirm accepts what the dialog is showing`.

- **The key that opens the GUI's help now closes it — all of them:** the close
  was spelled `f1` inside the overlay, so the two halves of one switch could
  disagree. No rebinding was needed to see it: the **vim** preset binds
  `app.help` to both `f1` and `?`, so `?` opened a page that only `f1` and
  `Esc` could close. The close is now asked of the keymap, and every chord
  bound to `app.help` answers it — including one behind `ctrl`, which the
  overlay's modifier gate used to swallow. `Esc` still closes in every
  configuration, which is what the footer promises.

- **A keymap change now reaches a help page that is already open:** the page's
  chords and its generated keyboard sheet were frozen when the overlay opened,
  so they could go on naming keys that no longer did anything. They are rebuilt
  where the effective keymaps are; the availability FACTS are deliberately not,
  since freezing those is what keeps a row from changing verdict under the
  reader's cursor. The window is narrow and worth naming: this frontend watches
  no config files, and the settings screen cannot be open at the same time as
  the help — but the write is asynchronous, so changing the preset, closing
  settings and opening the help lands the swap with the page in front of you.

- **A wide publisher no longer pushes a warning off a plugin's help page:** the
  provenance line under a plugin page title is `from an extension · published
  by X`, followed by the host's own flags — cut short, some bytes did not
  decode. The publisher comes from the plugin and is capped at 280 characters,
  which in CJK is 560 columns; the graphical frontend paints that line
  unwrapped in a fixed-width panel, so those flags — the only part of the line
  a reader acts on — were pushed off the right edge by text the plugin chose.
  The publisher now gets what a 96-cell budget has left once the host's
  segments are reserved. The clamp is on the publisher and never on the
  finished line, because truncating that would eat the flags.

- **The help now covers everything, and the gate has nothing left to forgive
  (H3h):** every command norte can dispatch and every screen it can open is
  explained by a page, in English and in Spanish. Sixteen topics per locale,
  up from eight: the viewer, finding things, what the listing shows, answering
  a dialog, settings and themes, AI rename and semantic search, what an agent
  may do here, and extensions — plus the sections the existing pages were
  missing, from the cursor keys to renaming, creating a directory and the four
  answers to a collision.
  The part that outlives the prose is the gate underneath it. Since the corpus
  existed, a command with no page was carried by an explicit allowlist that
  could only shrink, with a compile-time ceiling so it could not quietly grow.
  H3h emptied it, and the list and the ceiling are deleted: a new command or a
  new screen that arrives without a page now fails the suite with nowhere to
  write it down. That is permanent friction on adding a command, and it is the
  point.
  `F1` over any dialog opens prose about THAT dialog — the fallback that
  refused to cover a live question with the index is still there, but it is now
  a guard against someone deleting a page rather than a state the app ships in.
  Three hostile cases the audit asked for are covered from the shared corpus
  rather than from literals: a plugin id carrying a bidi override (a new
  fixture, since an id is a lookup key that crosses the wire), a command title
  that is blank after masking, and a double-width publisher.

- **`norte help` reads the same corpus from the command line (H3g):** the index,
  one page by id, `--list`, `--search`, and `keys` — the keyboard sheet
  generated from *your* effective keymap, not a written list. Embedded: no
  daemon, no network, no engine. `--json` dumps the whole thing (or one page)
  for agents and for a golden that pins the shape, with a `version` field so a
  consumer has a discriminator before it needs one.
  It behaves like a command-line tool rather than a window: exit 1 with one line
  on stderr for an unknown page (naming `--list`) or a search that matched
  nothing, following `grep`; a closed pipe is a quiet exit rather than the Rust
  panic `println!` produces, so `norte help | head` prints what you asked for
  and stops; and the arguments it echoes back in an error are masked and capped,
  since an error line is exactly where a pasted string carrying an escape ends
  up. Callouts are ASCII words where the terminal app paints glyphs — the stream
  may not be a terminal.
  `norte help <command>` is no longer clap's way to reprint a subcommand's
  `--help`; that lives where it always did, at `norte <command> --help`.
  Plugin pages are deliberately not reachable from here: they need the plugin
  registry, and `norte doctor` already reports what is wrong with one.
- **The GUI has the help, and `F1` finally does something (H3f):** the same
  corpus, the same model and the same executable rows the terminal app got in
  H3b–H3e, painted in the active theme — sidebar of topics on the left, page on
  the right, `/` to filter, `⇥` to cross between the two, `⏎` to run the row you
  are reading, `Ctrl+P` to hand your filter to the command palette. Extensions
  get a page each, fetched the moment you open their row and never before, and
  every plugin page says on its face that a plugin wrote it.
  The bug underneath it is worth naming, because nothing reported it: all three
  shipped keymaps have bound `F1` to the help since the corpus existed, and the
  GUI filtered that binding out because its command table did not list the
  command. The key was not broken, it was dropped — silently, which is the part
  that took a test to make impossible again.
  A row the app would refuse is dimmed here too, judged against the facts of the
  moment the overlay opened rather than a fresher set: a page whose verdicts
  shift while you read it disagrees with itself, and `⏎` on a dimmed row now
  says why instead of quietly doing nothing.
  What the reviews caught, since it is the part worth recording: the sidebar
  shipped a literal `help-group-keys` to every reader who pressed `F1` — the
  model always emits that group and neither catalogue names it, on purpose, so
  the painter had to drop it rather than translate it; a plugin whose name is
  blank *after* masking bought itself a nameless row in the Extensions group,
  which is the H3e fix this frontend was missing; plugin ids arrived unvalidated
  where the terminal app drops them; a blank command title sent the row back to
  wearing its raw dispatch key; the reason a dimmed row gave on `⏎` was painted
  underneath the overlay's own scrim; the search box could be entered and never
  left, so letters meant for the page silently edited it; and the body laid out
  every line of a document, not the ones on screen.
  Everything this entry once listed as deliberately not done is done; see
  below.
- **Plugins bring their own help page (H3e):** a plugin can ship a `help.md`
  next to its `plugin.toml`, and it becomes one more page in the help overlay,
  under an *Extensions* group, with the plugin's own commands as rows you can
  run from there. The page arrives on demand — 64 KiB per plugin has no
  business riding every listing — and it is fetched once per time you open the
  help, never while a frame is being painted. A row for a plugin that is not
  approved and enabled is dimmed and says so, which is the answer you came for
  if you are reading that plugin's documentation to decide whether to turn it
  on. The rows wear the name the plugin gave the command, not the internal key
  norte dispatches it with.
  The text is third-party and is treated as such throughout: the host bounds it
  at the same cap the hostile-input parser uses, decodes it, and refuses to
  serve a `help.md` that symlinks out of the plugin's own directory; the page
  is masked when it is parsed, so a bidi override in a title cannot rearrange
  what you read; a `{{cmd:}}` naming a command the plugin does not own stays
  literal text instead of becoming a row you could press; and every plugin page
  says on its face that it is a plugin page, because a page that could pass for
  norte's own prose is a page that can tell you approving is safe.
  `norte doctor` now names six ways a `help.md` can silently do nothing: over
  the size cap, undecodable bytes, announcing a page that serves nothing, a
  `+++` header that does not parse, commands the plugin does not own, and an id
  that collides with a built-in page.
  Two things it deliberately does not do. `F1` pressed over a dialog or from
  the command palette opens without the extensions group: those paths are
  synchronous, and a daemon round trip inside key handling is not worth a
  sidebar node. And the command palette still hides an inactive plugin's
  commands rather than dimming them — the palette is the fast gesture, and the
  place to explain a plugin is its own page.
- **The help stops offering what the app would refuse (H3d):** a row for a
  command that cannot run where you are — writing into an archive, renaming
  something the backend will not let you rename — is dimmed and says why,
  instead of promising a key that is about to fail. The verdict comes from the
  same table the GUI's context menu uses, so the two cannot disagree about
  whether "copy" is available; where the frontends genuinely differ they say
  so as facts rather than forking the table (a `.zip` is a thing you enter in
  the terminal and a file you open in the GUI). Capability flags now come from
  the answer norte was already fetching for the columns and throwing half
  away, so knowing this costs no extra round trip, and a degraded connection
  is finally kept as data instead of a sentence — two degraded connections no
  longer overwrite each other, and the status bar names the most recent one
  and says how many more there are, masked like every other name that arrives
  from somewhere else.
  Three things it deliberately does not claim: policy denial, because in the
  embedded app you are the human, whom the policy engine never denies;
  the difference between a plugin that was never approved and one whose
  approval expired, because the protocol does not carry it; and anything at
  all from a degraded connection — that flag means the session is unencrypted,
  not that it cannot act, and dimming on it would lie to every FTP user.
- **Help about where you actually are (H3c):** `F1` no longer always opens the
  index. From a pane it opens the page about the panes, from a collision dialog
  the copying page, from the host-key prompt the remote page — the mapping
  lives in the corpus, so moving an explanation from one page to another is an
  edit to prose. Pressed over a dialog, the help reads about *that* dialog and
  `Esc` puts you back in front of the question without answering it. While the
  page covers a dialog the dialog's own keys are inert, and its footer says so
  instead of advertising keys that do nothing. A dialog that arrives *while*
  you are reading closes the help rather than hiding behind it: an approval
  you cannot see is an approval you cannot answer — and if you leave the page
  open over one, its timeout denies the agent, which is the safe direction.
  `F1` on a command-palette row opens the page that documents that command,
  the other half of the `Ctrl+P` handoff the help already had.
  Five places have their own page so far; everywhere else `F1` still opens the
  index, and the ones still missing are listed, one per line with its reason, in
  the gate that will not let them be forgotten. Over a *dialog* with no page yet
  nothing opens at all: the index would cover a live question with prose about
  something else, so norte says so and leaves the prompt answerable.
- **Passing a location between the panes:** `Alt+i` sends this pane's location
  to the other one and leaves the focus where it is — the fastest way to line
  up a copy, because the destination is whatever the other pane holds. `Alt+u`
  is the same gesture the other way round, and `Ctrl+U` swaps the two, which is
  how you reverse the direction of a copy without navigating anything: nothing
  is re-read, and the marks, the filter, the sort and the pane's own trail all
  travel with their pane, so the focus stays on the side of the screen you were
  already looking at. Mirroring onto a host you have not visited connects and
  asks about its key exactly as walking there would, and the question belongs to
  the pane that is travelling, not to the one you are looking at. If the
  destination cannot be reached the pane stays where it was and says why. From a
  live-search results pane there is nothing to send: a list of hits is not a
  location, and norte says so rather than guessing which directory you meant.
- **A real back and forward:** `Alt+←` returns the focused pane to where it was
  and `Alt+→` undoes that. It is a trail, not a list of favourites: from one
  directory to a second and then a third, back twice reaches the first — walking
  the "places this pane has been" list instead would bounce between the two most
  recent forever. Navigating somewhere new from the middle of the trail forgets
  the branch you stepped off, as a browser does. If a step back lands on a
  directory that has since disappeared, the step is rewound and the dead
  directory leaves the trail, the popup and the forward branch at once, so the
  key cannot trap you on it; any other failure keeps it, because a host that is
  down is still a place. When the trail runs out the key says so — a key that
  goes quiet is indistinguishable from a broken one.
- **Help you can navigate, and that knows your keys (H3b):** `F1` opens a page,
  not a key dump. The topics of the corpus on the left, grouped by tag; the page
  you are on to the right; `/` to filter the list by page title, page id or the
  commands a page documents; `Tab` to move the cursor into the page; `Enter` to
  run the command a row describes — through the same dispatch its own key uses,
  with the same confirmation, the same policy gate and the same journal entry,
  because a second, quieter way to run a mutation is exactly what this project
  does not want; and `Backspace` to go back where you came from, or out of the
  help when there is nowhere left to go back to. Those are the orthodox
  preset's keys: the overlay resolves every one of them through the `dialog`
  context, so a rebind moves them and the footer it generates says where they
  went.
  Enter on a link follows it and the help stays open — reading is not leaving.
  Enter on a runnable row closes the overlay first, deliberately: the command
  acts on the panes underneath, and help left on screen would cover the
  confirmation it opens. Arrowing the list of topics PREVIEWS them instead of
  navigating, so a scan down the index does not cost one `Backspace` per row
  before the overlay can close. `Ctrl+P` hands whatever you have typed to the
  command palette rather than making you retype it — the same model at a
  different speed — with the built-in commands only, since that handoff cannot
  wait on the backend for the plugin rows.
  No key in any page is written into the text. Each one is a `{{cmd:…}}` mark
  looked up in your effective keymap as the page is drawn, so a rebind changes
  the prose (a hot reload rebuilds that resolver next to the cheatsheet, or
  every page would keep teaching the old key), and a command you have unbound
  is named in the prose instead of claiming a chord you do not have. The corpus
  gains a page about this help itself, in English and Spanish like the rest.
  The old flat cheatsheet is still there, as the last entry in the list,
  generated from the same effective keymap — and still the one place the
  `dialog.*` verbs are all visible, which overlay footers leave out for want of
  width.
  What this phase does **not** do, each with a later phase of its own: the page
  still claims every command it documents is runnable right now, so nothing is
  dimmed and no reason is given for what the app would refuse (H3d); plugins
  cannot ship help pages yet (H3e); and the GUI has no help view at all (H3f),
  although the navigation model lives in `norte-frontend` rather than in the
  TUI precisely so that view is a second painter and not a second corpus.

- **The mouse, in both frontends:** left click focuses a pane and moves its
  cursor, double click does exactly what `nav.enter` does, and the wheel
  scrolls the listing **under the pointer** rather than the focused one.
  Ctrl+click toggles one mark, shift+click marks the range from the cursor
  (additively), and a drag marks what it sweeps — retreating gives those rows
  back instead of leaving everything the pointer ever touched marked. A plain
  click still never marks: browsing a listing cannot change what the next
  command acts on.
  Dragging onto the other pane **copies; shift+drag moves**, and the modifier
  is read at RELEASE, so someone who starts a drag and changes their mind does
  not move what they meant to copy. The drop opens the same confirmation the
  copy and move keys open and goes through the same collision dialog, policy
  gate, journal entry and undo — there is no quieter second mutation path,
  in either frontend.
  The rule that lets one gesture do two jobs is the row you press: a drag from
  a **marked** row carries the marks, a drag from an **unmarked** row carries
  that one row — but only once the pointer crosses into the other pane
  (*promotion*, without which the commonest drag in any file manager would
  transfer nothing). Promotion changes what the gesture does, never what is
  selected: the pressed row is not marked, and rows the sweep marked on the
  way out are given back, so a cancelled drag leaves the selection exactly as
  it was. A sweep armed with shift is deliberately **not** promotable —
  shift means "extend the range", and reading a range that ends past the pane
  boundary as a drop would turn a marking gesture into a move of the whole
  selection. Because the gesture means one thing at home and another across
  the way, it says which before the button comes up: how many items, to which
  directory, copy or move — the GUI as a drag label, the TUI in the status
  bar, both read from the same state the release reads.
  Right click (GUI) opens a menu of operations that already have keys — open,
  view, copy, move, rename, delete, copy path — each dispatching the SAME
  command the keyboard does, with entries that cannot run right now dimmed
  and carrying their reason. Two of them became real commands rather than
  menu-only actions: `pane.rename` (shift+F6) and `pane.copy-path` (alt+y).
  The menu acts on the marks when the clicked row is marked and on that row
  alone when it is not — which means **right-clicking an unmarked row drops
  that pane's marks**, irrecoverably, Esc included. That is deliberate: every
  command prefers the marks when there are any, so leaving them would let the
  menu say "1" while the copy took eleven.
  In the terminal the mouse is captured by default, which means your emulator
  stops seeing the buttons it uses for its own text selection. `[ui] mouse =
  false` (hot-reloaded, and discoverable in the settings overlay) gives it
  back, and Shift+drag selects natively in almost every emulator. Capture is
  released on exit, on panic, and whenever norte hands the terminal to an
  external program, so nothing ever inherits a terminal in mouse mode.
  The semantics live once, in `norte-frontend::mouse`, as a pure state
  machine over pane indices: each frontend hit-tests, feeds it, and applies
  the effects, so the two cannot drift into two different file managers.

- **Help corpus (H3a, ADR 0040):** new crate `norte-help`, the foundation of
  the help-system redesign. Topics are markdown-lite files with TOML front
  matter between `+++` fences, embedded through an explicit `include_str!`
  table (the `norte-theme` preset pattern) with a test that cross-checks the
  table against the directory, so a topic file cannot be silently left out.
  The markdown accepted is a **closed vocabulary** — headings, paragraphs,
  bullets, fenced code, tables, callouts, inline strong/emph/code — plus two
  **live marks** the parser deliberately leaves unresolved: `{{cmd:id}}` and
  `[[topic]]`. A frontend resolves them at draw time through the new
  `ChordResolver` seam, so the prose shows the key the user actually has bound
  and can never claim a chord that a rebind has moved. Ships six seed topics
  in English and Spanish (index, panes, selection, copying, remote, archives),
  every factual claim in them verified against the code rather than against
  the design docs.
  A second parse mode reads plugin-supplied `help.md` as hostile input: it
  never fails and never panics, decodes through `norte-encoding` (so a
  BOM'd file keeps its header instead of losing it), bounds source bytes,
  line length, block count and total table cells, masks terminal hazards
  where the data is built, and reports `truncated`/`lossy` for the UI badge.
  A plugin's topic id is host-assigned, its `{{cmd:…}}` marks must be
  namespaced to itself, and its `[[…]]` links are inert — so plugin help
  cannot shadow a built-in topic, forge a reference to a host command, or
  link into the host corpus.
  Integrity checks (`check_corpus`, `check_commands`, `check_contexts`)
  report findings as data for both the test suite and the future `norte
  doctor`, covering locale parity, duplicate ids, dangling links,
  unknown/undocumented commands, unknown/duplicate contexts, stale allowlist
  entries, and marks typed where the parser cannot make them live. A gate in
  `norte-tui` fails the build when a command in the vocabulary appears in no
  topic, with a hand-written allowlist that phase H3h drains to zero.
  The canonical `norte-testkit` corpus grows a fixture for the live mark
  itself (`cmd_mark_bidi_payload`, hostile names 31 → 32): a bidi override
  inside a `{{cmd:…}}` payload, which the parser must carry byte-for-byte so
  the gate's byte-exact cross-check refuses to ship it.
  No frontend renders any of this yet — `norte-tui` takes the crate as a
  **dev-dependency only**, for the gate; the F1 overlay is phase H3b/H3c.

- **Semantic index (M4-IA-2, ADR 0031 A3, proto 0.33.0):** two new
  methods over the wire. `index.embed` is a cancellable Task
  (`TaskKind::Embed`) that embeds the files a previous `index.build`
  already indexed: it filters by `denied_prefixes`, an
  extension-based text heuristic and a size cap **before reading a
  single byte**, then reads bounded 32 KiB prefixes through the
  providers, skips anything whose `(sha256, model)` is unchanged, and
  batches 16 texts per provider call with a bounded, cancel-aware
  retry on rate limits. `index.search_semantic` is a direct response,
  cancellable via `rpc.cancel`: one embedding call for the query plus
  a brute-force cosine scan over the stored vectors (`k` clamped to
  100, query capped at 4 KiB, scores guaranteed finite). Vectors live
  in the existing index database as an additive `embeddings` table
  (f32 little-endian, cascading with the file row); a vector from
  another model counts as absent and is regenerated. Both endpoints
  are human-only — content prefixes and the query leave the process,
  so agent connections are denied fail-closed — and both pass the
  full AI gate (`enabled`, `local_only`, `denied_prefixes`).
  Configuration is `[ai] embed_provider`; local Ollama is the
  expected default. New CLI verbs `norte index embed` and `norte index
  semantic`, plus a semantic search flow in both frontends
  (`pane.semantic-search`): query prompt → cancellable search (TUI) →
  hostile-safe hit list (masked paths, badges, scores that a crafted
  path cannot push out of view) → Enter navigates to the file. A
  hostile or broken daemon cannot flood either frontend: the shared
  `validate_semantic_hits` belt in `norte-frontend` rejects any
  response over the wire ceiling or carrying a non-finite score,
  whole, never truncated.

- **AI rename over the wire (M4-IA, ADR 0031, proto 0.32.0):** new
  `ai.rename_plan` method — a direct response, cancellable via
  `rpc.cancel`, that returns the reviewable plan and never mutates.
  The daemon builds the configured AI provider at startup (opt-in,
  degrading — a broken `[ai]` never aborts `norte daemon run`), denies
  agents fail-closed, and caps the instruction at 4 KiB. TUI and GUI
  gain the full flow (`pane.ai-rename`): instruction prompt → reviewable
  plan modal (target dir shown, numbered pairs, hostile names masked and
  badged, scrollable window, plans over 256 entries rejected en bloc) →
  N journaled `fs.move` tasks with undo. A malformed pair from a
  hostile or broken daemon aborts the whole apply before any move is
  submitted (shared `validate_ai_plan` belt in `norte-frontend`).

- **The plugin system has an installable plugin, and a way to install one.**
  It shipped with neither: discovery reads `~/.config/norte/plugins/<id>/` and
  nothing ever put anything there, while a finished syntax-highlighting
  previewer existed only as a test fixture. `norte plugin install <dir>` brings
  one in, `just plugin-syntect` builds and installs that one, and its manifest
  is now a real file the test reads rather than a literal the test invented.
  Installing is not consenting: the plugin arrives discovered and unapproved,
  because turning "I brought this file" into "it has its capabilities" is the
  whole decision. Replacing an installed plugin requires `--force` and
  **withdraws its consent** — the approval digest covers the manifest and not
  the `.wasm`, so without that, a new binary would run under a permission a
  human granted to a different one, with the manifest identical so nothing
  noticed.

- **A plugin's WIT no longer breaks every other plugin.** The package version
  travels inside each interface name, so one shared package meant any change to
  `provider` renamed `previewer` and every previously compiled plugin stopped
  loading — verified twice and never fixed, invisible here because the example
  guests are rebuilt every time, fatal for anybody else. Three packages now, so
  the interface that is going to keep moving moves alone.

- **A plugin that declares a hook is refused instead of installed.** There is no
  hook interface, no world, and no call site: the manifest accepted one, the
  manager listed it, and nothing would ever have run it.

### Fixed

- **A directory still loading no longer pours its entries into the wrong
  panel.** Swap the panels while a large directory is still filling — or, now,
  close one — and the batches still arriving were applied to whichever panel
  happened to be in that position. You watched a listing grow with another
  directory's contents, and nothing said so. The work in flight now belongs to
  the listing that asked for it, and a batch for a panel that no longer exists
  is dropped.

- **A finished synchronisation now says, by itself, whether it can be given
  back.** The report you can ask for after a synchronisation told you what was
  copied, overwritten, deleted and what failed — and not whether any of it
  comes back. That answer lives in the destination's trash, and it only ever
  travelled once, on the notification that closes the plan, which is fine for
  whoever approved the plan seconds earlier and useless for anyone who
  reconnected, was not the one who planned it, or simply did not keep the
  notification. Two synchronisations of nothing but copies produce byte-for-byte
  the same report, and one of them undoes completely while the other undoes
  nothing at all. The report now carries the same trash answer the approval
  screen was given, from the same plan, so it cannot say something different
  from what you agreed to. On the wire that is protocol **0.42.0** (#170).

- **A step that failed now says what it was, so a panel can tell you which of
  the two folders to go and look at.** A failure row carries a path, and that
  path is measured against the source for everything that writes and against
  the DESTINATION for a deletion — but the report is read without the plan in
  front of it, and the row did not say which kind of step it had been. The most
  ordinary hostile row of a mirror is exactly the ambiguous one: a deletion
  refused on permissions, whose path is in the destination and which carries
  nothing else to prove it. Until now that was inferred from whether the row
  happened to carry a second, destination spelling — sound only for as long as a
  deletion never carries one, which nothing on the wire said out loud. The class
  now travels with the failure (protocol **0.42.0**), so the rule is read rather
  than deduced. The panels still infer, and switching them over is deliberately
  a separate change, tracked in #208: it changes what two interfaces paint
  (#195).

- **Two unrelated files could be compared as if they were one, with nothing
  saying so.** Names are paired by their NFC form so that the same file copied
  between macOS and Linux lines up, and NFC is not injective: U+212A KELVIN SIGN
  becomes `K`, U+2126 OHM SIGN becomes the Greek omega. Two files that sit side
  by side on ext4 without any case-insensitivity involved therefore paired, and
  the resulting row — an ordinary "same" or "different" — had no way to say that
  its two halves are not the same name. A synchronisation reading that row would
  overwrite a file that has nothing to do with the one it came from. Comparison
  rows now state, when the two names differ in bytes, WHICH transformation
  paired them (protocol **0.42.0**), and they keep the dangerous case separate
  from the two ordinary ones — a consumer that could only see "these differ in
  bytes" would have to choose between trusting every normalised pairing, which
  is the bug, and rejecting all of them, which breaks the macOS-to-Linux case
  the pairing exists to serve. The corpus gains the two names that prove it.
  Deciding what a synchronisation should DO about such a pair is the next step
  and is tracked separately (#207); this is the fact it needs in order to decide
  anything at all (#152).

- **An operation is now recorded whole, or not at all — never half.** This is a
  trap the retry above opened, closed in the same release. Because norte can now
  regain the journal partway through a long session, and because it used to ask
  "am I recording?" once per file rather than once per operation, a copy or a
  delete that began while another process held the journal and ran longer than
  half a minute would start recording in the middle: the first files unrecorded,
  the rest recorded, inside one operation. `undo` then reversed the recorded
  tail and silently left the head — and could not even tell you which files it
  had skipped, because there were no entries for them. "Nothing was recorded" is
  something you can sort out by hand; "half was recorded" is not. Each operation
  now settles the question once, before it touches anything, and keeps that
  answer to the end — and if the journal has become unreadable while the
  operation waited its turn, it is refused there too rather than running
  silently. That covers batch renames as well, where the split was worse: the
  entries recorded partway through would have carried no batch label, so an
  action the interface presents as one undoable unit would have been half
  recorded and not grouped. A batch that runs with no journal at all now also
  stops claiming its steps were recorded, which had been sending people to look
  for an undo that had nothing to undo. The message you get when the journal
  comes back says all this in one line: journalling resumes from your *next*
  operation, because one already running keeps the answer it began with (#205).

- **The journal's format marker can now be signed, so re-declaring it stops
  being free.** The journal records which format it was written in, inside its
  own tamper-evident chain, so an older build can say "I cannot verify this"
  instead of accusing an untouched file of tampering. The hole that design
  conceded: anyone who could write the database could *re-declare* the format
  with three column writes and no key — and a verdict that said "your history
  was altered at entry 40" then said "I cannot read this file" instead. The
  alarm survived; the blame did not. The HMAC anchors did not catch it either,
  contrary to what is intuitive: the edit moves no stored digest, so every
  anchor still verified.
  `norte audit anchor` now signs the marker too, in a file of its own, and
  `norte audit verify` checks it. A re-declaration is then a hash mismatch **at
  the marker**, located and with a key behind it, for any journal that had a
  marker when it was anchored. `verify` also says when a marker is present and
  nothing anchors it — the state you are left in if someone removes that file,
  and also what an *injected* marker looks like on the older journals that
  never had one and by design never will. What it cannot cover is a journal
  nobody ever anchored, which was always the boundary of the anchors and is
  why keeping a copy of them somewhere else is the point (#146).

- **Cancelling a mirror halfway through a deletion erased part of a folder and
  recorded nothing.** A mirror removes what the source does not have, and
  against a destination with no trash — an object bucket, an SFTP, a FAT
  stick — that removal is permanent. It walks the tree file by file and checks
  for cancellation between each one, so pressing `Ctrl+K` (or closing the sync
  pane, which cancels it) partway through left a subtree partly and
  irreversibly deleted with **no journal entry, therefore no undo, and no row
  in the report either** — nothing anywhere said it had happened. The
  comment in the code promised the entry survived a half-finished delete; the
  condition beneath it excluded the one case where it mattered. It now records
  what it actually removed, and a test cancels a real mirror mid-tree to prove
  it. **Recorded is not the same as recoverable**, and the interfaces now say
  which one you get: against a destination with no trash the entry is marked
  irreversible, so `undo` names the subtree it cannot give back rather than
  pretending to restore it. The cancelled step still gets no row in the
  synchronisation report — the run stops at the cancellation, so it counts as
  neither done nor failed — and the journal is what tells you. What changed is
  that the deletion is no longer invisible (#186).

- **A file could be moved to the trash and then not written down.** This one
  was hit for real, not imagined: `sync.apply` trashed a destination file and
  the journal write that should have recorded it failed, leaving the file
  somewhere the user did not put it and nothing to say where. norte already
  tried to put it back; what it could not do was *tell anyone* when putting it
  back also failed — the trash location existed only inside a log line, so
  "where is my file?" was answerable only by whoever happened to be reading the
  daemon's log at that moment. The failure now travels as its own kind of
  error carrying both the buried path and its trash destination, and the step
  is reported as failed instead of leaving a report that reads "nothing
  happened" — which is what you saw when the very first step was the one that
  broke. The run stops before touching anything else. Where the file went is
  still written to the log rather than shown to you: the protocol has no
  category for "your file is in the trash and nothing recorded it", and
  inventing one is a bigger change than this (#160).

- **Corrupting one file switched off the record of everything norte did, and
  norte carried on as if nothing had happened.** Without the background service
  running, every change you make is recorded in `journal.db` in the state
  directory — that record is what `undo` reads, and what an audit reads.
  Anyone able to write to that directory could make the file unreadable
  (`chmod 000`, one byte of garbage, a stale schema) and from then on every
  `ntc` session, every `norte cp/mv/rm/mkdir`, and — the valuable one — every
  `norte ai rename --yes` ran completely unrecorded, behind a warning that also
  fires in the entirely ordinary "the background service is running" case and
  which people had therefore learned to ignore. The background service refuses
  to start on exactly the same file; the two disagreeing was the bug.
  An unreadable journal now **refuses the change before it happens** and says
  which file to repair or remove, while a journal merely held by another norte
  process still lets you work — refusing there would turn "a service is
  running" into "the file manager does not work", and a script's `norte cp`
  holding the file for a quarter of a second must not be able to stop you.
  On the wire that is protocol **0.41.0**: one new error category,
  `journal_unavailable`, so a client can say what happened in the reader's
  language instead of showing a raw English string. Older clients degrade it to
  "unknown error" as they do every category they do not know.
  **This closes the clumsy half of the hole, not all of it**, and the
  distinction is worth being plain about: a process running as you that simply
  *holds* the journal open still makes your session run unrecorded, because
  refusing there is exactly what must not happen. Tracked as #203 (#178).

- **A quarter of a second of bad luck marked a three-hour session as
  unrecorded, for its whole life.** Without the background service running,
  norte records what it changes in a journal file that only one process may
  hold at a time, and it takes that file at the first change you make. If
  something else held it at that exact instant — a script's `norte cp`, an
  audit, the service restarting — the session gave up on journalling
  permanently, and the "NOT journalled" warning in the status bar stayed true
  for the rest of the day even though the file had been free again a second
  later. It now tries again — at most once every thirty seconds when the file
  is merely held by someone else, so that a genuinely busy journal costs
  nothing per operation, and immediately when it is unreadable, so that
  repairing it takes effect on your very next action — and **the warning
  switches off when the journal comes back** rather than lying at you until you
  quit.
  The mechanism underneath is what took the work: reopening a journal this
  process once owned has to re-read the chain's position from the file, and a
  stale one collides with the row it is about to write — which would fail
  every later change, applying each effect with nothing recorded. Releasing
  therefore destroys the handle rather than parking it, and a test pins that a
  third writer's rows are followed rather than overwritten. The prompt norte
  shows before an AI rename — "this batch will not be recorded" — deliberately
  ignores the thirty-second brake, because that one is a question a person
  answers, and answering it from a half-minute-old verdict would talk them out
  of a rename the journal would have recorded fine (#179).

- **One invalid byte in a name disabled its whole collision key.** The
  filename comparison shared by `fs.compare` and batch rename ran
  `str::from_utf8` over the ENTIRE name and gave up on normalising or folding
  it the moment a single byte was not valid UTF-8 — so a name that is
  99% ordinary text with one stray byte (a truncated encoding, a copy from a
  mixed-locale volume) skipped both NFC and case folding entirely. `CAFÉ`
  (NFC) plus a trailing invalid byte and its NFD twin plus the same trailing
  byte therefore answered two different keys instead of one: `fs.compare`
  would have reported them as two unrelated files, and batch rename would
  have missed the collision between them. The text ahead of an invalid byte
  now folds and normalises on its own; only the bytes that are not text pass
  through untouched (#154).

- **Comparing two directories over a network mount hydrated their sizes and
  dates one file at a time, in series.** `fs.compare` asks a provider for
  `size`/`mtime` only when its own directory listing did not already carry
  them, which keeps a local comparison free — but those `stat` calls
  themselves ran one after another, a left `stat` then a right `stat` then
  the next pair, so a tree of N paired files could cost up to 2N chained
  round trips. Over `file://` served by a real network mount (SMB, NFS,
  sshfs) or a remote provider whose listing does not always carry a date,
  that latency was the whole runtime. Every file pair in one directory now
  hydrates concurrently, bounded, instead of one at a time; row order and
  per-pair error reporting are unchanged (#156).

- **A name containing `↔` could spoof the pair in the terminal's comparison
  pane.** The block title joined both roots into one string —
  `left ↔ right` — and `↔` is an ordinary printable character: it is not a
  terminal hazard, so it was never masked and carried no badge. A directory
  legally named `docs ↔ ⟨file⟩/home/victim/backup` therefore read as a
  different pair of roots than the ones actually being compared. The same
  join also let a long left root push the right one out of the title with no
  `…` and no other sign that anything had been cut. Both roots are now built
  and width-budgeted separately before either reaches the title, and the
  separator is its own styled span, so an embedded `↔` stays inside its
  root's half instead of being read as the boundary between the two (#185).

- **Lowering the case of a name can re-spell it, and batch rename was not
  looking again.** When a directory does not distinguish upper from lower case,
  the batch planner compares names by normalising them and then folding the
  case. Those two steps do not commute. `J` followed by a combining caron has no
  single-character capital, so normalising leaves it as two characters; lower
  its case and the result *does* have one, `ǰ`. The planner therefore called
  those two names different, and every case-insensitive volume — Apple's APFS
  and HFS+, a case-folding Linux directory, a case-insensitive network share —
  calls them the same file. A batch aiming at one while the other sat in the
  directory saw no collision and asked for a rename onto a name that was taken.
  The comparison now normalises again after folding, so the collision is
  reported and the plan is refused before anything moves. Both spellings are in
  the canonical hostile-name corpus, which is what the fix is tested against.
  One relative of this is still open and now written down where the code makes
  the decision: a filesystem folds case with case *folding*, this folds with a
  lowercase *mapping*, and the two disagree on about twenty characters —
  a word-final Greek sigma, the micro sign against Greek mu, the long s. See
  [#129](https://github.com/compilando/norte/issues/129).

- **Batch rename asked a directory about its case rules before looking at it.**
  A provider is allowed to work out what a directory does with names lazily, on
  its first real operation — the local provider probes exactly then, because the
  question cannot be answered without touching the disk and answering it must
  not block. The planner asked before it listed, so the very first plan on a
  freshly opened directory got the operating system's *guess* instead: a volume
  that folds case planned as though it distinguished it, which is a collision
  not reported and a preview missing the one line that mattered. That is the
  normal path for an agent bridge or a one-shot command, where planning is the
  first thing that happens. It now lists first and asks afterwards.

- **Eight invisible characters were walking straight past the mask (#125).**
  `is_terminal_hazard` claimed in its own documentation to cover "the invisible
  Cf/Zl/Zp", and it was a list somebody wrote by hand. `U+2064` INVISIBLE PLUS,
  `U+2061` FUNCTION APPLICATION, `U+206E`, `U+FFF9`, `U+180E` and `U+2800`
  BRAILLE PATTERN BLANK were not in it — and `U+3164` HANGUL FILLER and
  `U+115F` never could have been, because they are category **Lo**: letters
  that paint nothing. No enumeration of Cf was ever going to catch them, and
  they are the classic invisible-smuggling code points.
  What that costs is the premise the mask exists to protect. `a<U+3164>b.txt`
  and `ab.txt` are two different names that look identical, and neither one
  raised the hostile badge — so approving the one you read approved the one you
  did not. Every frontend delegates here, so there was no defence downstream.
  The set is now decided by the Unicode property
  `Default_Ignorable_Code_Point` plus the characters that paint nothing without
  being ignorable to anyone (braille blank, the interlinear annotations, the
  line and paragraph separators). ZWJ and the variation selectors stay allowed,
  deliberately and now explicitly: they are default-ignorable too, and masking
  them would break composed emoji for the sake of a twin that differs only in
  that. Two corpus fixtures pin each half of the hole, and a test walks the
  whole code point space so a set that grows by accident is loud.

### Changed

- **A menu bar.** `Alt+M` opens one: File, Mark, Panels, Tabs, Find, View,
  Help. It adds nothing the keyboard cannot do — it adds a way to find it. The
  palette asks you to know the name of what you want and the help asks you to
  read; a menu you can walk. Arrows move, `Enter` runs, `Esc` closes, and with
  the mouse you just click.

- **The tab bar is a set of buttons.** Click a tab to switch to it, `[+]` to
  open one, `[x]` to close the one you are looking at.

- **More than two panels.** Split the focused panel side by side or top and
  bottom, as many times as you like. The new panel starts in the directory you
  were in, already filled, and takes the focus — splitting is asking for room
  to work in. Splitting a tab cuts inside that tab, not around its siblings.

- **From three panels on, you say where a copy goes.** With two, the
  destination was always the other one and nobody had to be told. With three
  that stops being obvious, so norte stops guessing: designate a destination
  and it is marked on its panel, or a copy asks you for a path instead of
  choosing for you.

- **A layout you can name and keep.** `[ui] layout` points at a file in
  `layouts/` and norte starts with that arrangement. One that does not load
  never leaves you without a screen: it says so and starts with the usual two
  panels. `norte doctor` tells you before you find out the hard way.

- **Tabs.** A panel can hold several tabs, and each one is a whole listing with
  its own directory, cursor, marks and history — switching tabs remembers
  nothing because it forgot nothing. `Ctrl+T` opens one in the directory you are
  already in, filled, and `Ctrl+W` closes it; when one tab is left the bar
  disappears and the panel is a panel again. A tab you cannot see costs nothing:
  it does not watch its directory and asks for nothing until you come back.
  Total Commander and Krusader users get the keys their preset already showed in
  grey.

- **Panels can be resized and closed.** Give the focused panel room, take it
  away, or put them back to equal. Closing refuses to take the last one: a
  screen with no listing is not a layout.

- **A narrow terminal now shows one panel instead of two useless ones.** Below
  about forty columns two panes cannot show a name next to its size, so the
  split folds and one panel takes the width. The focus follows it.

- **The screen is now built from a layout, and nothing about it has moved.**
  The two panes used to be a fixed field and a split written out by hand in
  three places — once to paint, once so the mouse could tell which row you
  clicked, once so pagination knew how many rows fit. They are now slots in a
  tree that one function divides, and the painter and the mouse read the same
  answer. You will not notice: the default layout is exactly the screen you had,
  and there is deliberately no way yet to change it. What it buys is the next
  part — tabs, a places sidebar, a docked preview, and a session that remembers
  your arrangement across the terminal and the graphical app.

- **The terminal binary is `ntc`.** Nobody types `norte-tui` twice a day;
  Norton Commander was `nc`. `norte tui` still launches it and the CRATE keeps
  its name — renaming that would touch five manifests and a crates.io identity
  to spare nobody any typing. The rename had one defect that could half-land:
  the CLI hands the process over BY BINARY NAME, a string that compiles whether
  or not a binary answers to it and fails at `exec` time, with the user in
  front of it. A test now reads the expected name out of the TUI's own
  manifest, so the two cannot drift apart. `cargo uninstall norte-tui` still
  takes the crate name, which is why the `justfile` says so out loud.

- **A tag now produces something you can download.** cargo-dist had been
  configured since before the first tag and had never run: its model is
  CI-driven, and CI is off. `just dist` builds the artefacts on a developer
  machine, `just dist-smoke` unpacks each archive and runs the binary inside it
  — the one packaging failure a user finds before we do — and `just
  dist-publish <tag>` uploads them, with the protocol, config and keymap JSON
  Schemas alongside, so a third party can write a client without cloning
  (#13). The five configured targets stay in `dist-workspace.toml` because
  they describe the release the project should produce; what we build today is
  **x86_64 Linux only**, and the release notes say so rather than the config
  quietly pretending otherwise. The checksums dist generates are checksums, not
  signatures, and nothing calls them that.

- **`norte` ships with `ntc`.** The CLI was marked out of the release back when
  it was an M0 test bench. It is now the non-interactive half of the product —
  `daemon`, `connect`, `mcp`, `policy`, `undo`, `index`, `ai`, `audit`,
  `doctor` — and an artefact carrying `ntc` alone leaves a user with no daemon
  and no `doctor`. dist builds one archive and one installer per package, so
  there are two of each; the README gives both.

- **The graphical interface stays source-only, and the gate says why.**
  `norte-gui` joined the workspace (one lockfile, one resolution) but is kept
  out of `default-members` and out of the release. The reason is not build
  time. GPUI enables `serde_json/preserve_order`, and cargo unifies features
  per invocation: with the GUI in the same `cargo` as the core, the core's
  `serde_json` swaps sorted maps for insertion order — and with it the protocol
  JSON Schema we publish, the CLI's `--json`, and the goldens that pin them.
  Five tests went red without a line of code changing. So the gate names its
  packages instead of saying `--workspace`, which was silently overriding
  `default-members`, and the core is now tested exactly as it is distributed.
  A GPUI binary also links against the graphics stack of the machine that built
  it, which is the honest limit of shipping one at all. Its own gate,
  `just gui-ci`, keeps auditing it against its own licence policy — excluded
  from the workspace audit, never unaudited.

- **The help overlay reads like a page now.** The sidebar is sized to its own
  titles instead of a flat 24 cells — floored at that 24 so nothing narrows, and
  capped at a third of the screen — so the list of topics stops cutting five of
  its nine rows on a normal terminal. The prose it points at gained a gutter and
  lost its 90-cell lines: the body is laid out at a 72-cell measure, which is
  what a line of text is read at. Groups are separated by a blank line, a
  heading inside a page gets more air than a paragraph break, and the synthetic
  keyboard entry no longer carries a header that repeats its own name. The
  footer says where you are in the page (`{line}/{total}`, the viewer's idiom)
  whenever the page does not fit — the runnable rows of a topic are painted
  behind all of its prose, and nothing used to say they were down there.

- **Keys are spelled the way the documentation spells them.** `F5`,
  `Shift+F8`, `Ctrl+Alt+F5`, `PgUp` — everywhere a chord is painted: the F1
  cheatsheet, the prose of every help topic, the command palette's chord
  column and the footer of every overlay. `Chord`'s `Display` stays raw and
  lower case, because logs and debug output want the literal chord; the
  conventional spelling is a single shared presentation home
  (`norte_frontend::keymap::paint_chord`) that the three painters now route
  through, and which masks terminal hazards FIRST — an untrusted project
  `./.norte/keymap.toml` can bind any codepoint, and nothing cosmetic may
  resurrect it. A key bound to a single printable character is left exactly as
  it is: `y` is not painted `Y`, because `Y` is a different binding and would
  be telling you to press Shift.

- **The help footer fills the terminal it is given.** It used to drop
  `[enter]` and `[esc]` from the printed hint unconditionally so the rest
  would fit at 80 columns, which left a 113-column footer half empty with two
  keys hidden for no reason. All five verbs are now offered in priority order
  and the width decides: a wide terminal shows them all, a narrow one keeps
  the ones you cannot guess (`[/] filtrar`, `[backspace] atrás`,
  `[tab] otro panel`) and marks the cut with `…`. Whole `[chord] label` groups
  as always — never half of one.

- **`q` no longer closes the help.** The overlay now resolves its keys through
  the `dialog` context like every other one, and `q` is `app.quit` there — a
  key that means "leave the app" cannot also mean "leave this page". `Esc`
  closes it, the key that opened it (`F1`) closes it, and `Backspace` goes back
  a page or out of the help when there is nowhere left to go back to.

### Fixed

- **A help page no longer breaks a word at a style change.** Only whitespace
  is a break opportunity: two styled fragments with nothing between them are
  one word and wrap together. A sentence closing on an inline code span used to
  render as `…dentro de un .zip` with the full stop alone on the next line, at
  any width where the boundary landed near the margin.

- **The F1 cheatsheet stops quoting its own translation keys.** The `dialog`
  keymap merges the preset's `[global]` section, so `app.quit` and its
  neighbours turn up while the dialog half of the page is drawn — and that half
  was asking the `dialog-cmd-*` catalogue for them. A missing Fluent message
  answers with its own id, so eight rows read `dialog-cmd-app-quit` at whoever
  came looking for the key. Each command is now labelled from the catalogue it
  actually lives in.

## [0.3.0-alpha.2] - 2026-08-02

### Added

- **The TUI watches the visible directories (#106):** external changes to
  the panes' local directories now refresh automatically — a native
  watcher (inotify/FSEvents/ReadDirectoryChangesW) over both visible
  `file://` dirs, with a graceful fallback to a 2-second mtime poll (with
  a one-time status notice) when the watcher cannot start or the inotify
  watch limit is hit, never a failure. Events are debounced with a true
  trailing edge plus a floor between refreshes, so a large copy into the
  watched directory coalesces instead of refreshing every 300 ms; a watch
  event never interrupts an open dialog, overlay, or quick search — it
  queues and fires when the interaction ends. The refresh takes the same
  cancellable path as Ctrl+R (marks survive, #118 ritual). Remote and
  archive panes remain manual-refresh (no inotify there); polling-mode
  limits are stated honestly in the notice (edits to existing file
  contents don't change the parent dir's mtime). GUI watching is still
  pending on #106.

- **Plugin column cells rendered in both frontends (#117 follow-up):**
  `plugin:<plugin>/<column>` ids configured in `[ui.columns]` now paint
  real cells through the shared column funnel — default width 12 (spec
  width/align/header overrides apply), headers via the shared label
  resolver, capped at 8 plugin columns per list (painted always equals
  requested; `norte doctor` reports the excess as
  `columns-plugins-over-cap` and retires `columns-no-renderer`). Values
  arrive asynchronously per listing (piggybacked on the decoration fetch
  in the TUI, the session Columns command in the GUI), are validated
  against the live catalog (approved + enabled + the column declared by
  THAT plugin), sanitized and capped on ingest, and defensively re-masked
  at render; absent stays blank. The GUI's previous behavior of
  unconditionally painting every declared plugin column at a fixed 96px
  is retired: `[ui.columns]` is now the single source of truth. Deferred
  to #120: offering declared plugin columns in the picker, and
  disambiguating duplicate bare column ids across plugins.

- **Provider attribute columns rendered in both frontends (#117):** the
  `attr:` columns the config, model and picker already accepted now paint
  real cells. The shared render funnel is `ColumnId`-typed end to end;
  attr cells format by the value's own tag refined by the catalog hint
  (sizes IEC/SI/exact, timestamps relative/ISO, POSIX modes `rwx`/`octal`
  — two new spec format words), third-party `Text`/`Bytes` values render
  masked and capped (bytes lossy-with-U+FFFD, originals untouched), blank
  strictly means absent (`?` = present but unpaintable). Panes request the
  configured attr ids on every listing and cache the provider catalog once
  per scheme (`fs.capabilities`); the picker now OFFERS advertised
  provider columns (disabled rows, localized or masked labels) and cycles
  attr formats by hint. Column headers resolve localized → masked catalog
  label → sanitized id. `norte doctor` retires `columns-no-renderer` for
  `attr:` (plugin cells still pending) and gains
  `columns-attrs-over-cap` (>16 configured) and
  `columns-attr-id-not-wire-safe` (an id that parses but is illegal on the
  wire is skipped instead of failing remote listings). The TUI/GUI refresh
  affected panes when a picker apply or hot-reload changes the requested
  attr set; painted always equals requested. Follow-up filed: #118
  (pre-existing Ctrl+R refresh ritual gap). No wire change (additive 0.30
  contract). Sorting stays on the closed name/size/mtime vocabulary —
  sorting by attr columns is future work.

- **GUI column picker — Alt+C (#108 block 7c):** the GUI gets the same
  picker the TUI ships, as an overlay panel over the shared model: toggle
  (Space/E), reorder (Shift+↑/↓), sort by the cursor's column (S), cycle
  format (F); Enter applies in-session and persists (columns + sort +
  changed formats), Esc discards. Applying clears the session header-click
  sort override for the panes the save targets — the persisted sort
  supersedes it. Opaque ids render masked and length-capped (screen readers
  included); the panel scrim occludes mouse input; all GUI config writes are
  now serialized (follow-up for atomic persist: #116). Closes the last
  block of the columns design.

- **Provider attributes produced end-to-end (#108 block 2):** the wire that
  0.30.0 shipped now carries real data. `Provider` gains defaulted
  `attrs()`/`list_with()`/`stat_with()` (`ListOptions`/`AttrRequest`);
  producers: local (`posix.mode`/`uid`/`gid`/`nlink`/`ctime_ms` on unix,
  `win.attributes` on Windows — the #52 lazy listing stays untouched unless
  an advertised id is requested), sftp (`posix.mode`/`uid`/`gid` off the
  already-parsed SFTP attrs), object (`s3.etag`; `s3.content_type` on stat),
  archive-zip (`archive.method`/`packed_size`/`crc32`, kept even for
  encrypted entries) and the testkit `MemProvider` (hostile synthetic
  values). The daemon publishes the catalog through `fs.capabilities`,
  rejects malformed or over-16 requested ids (`-32602`), forwards only
  advertised ids and enforces emit caps per entry; paginated listings keep
  the request from the opening call. The conformance suites gain an
  attributes contract (type agreement, request scoping, caps,
  unknown-id absence). CLI: `ls --attrs <id>` (repeatable; wire-exact under
  `--json`, masked column in human output). Deferred with issues:
  `sftp.owner`/`group` names (#114), `s3.storage_class` (#115).

- **Per-column presentation — `[[ui.columns.spec]]` (#108 block 7b):** a
  spec entry keyed by column `id` sets `width` (`"auto"` / `{ fixed = n }` /
  `{ min = n, weight = m }`), `align` (`left`/`right`), `format` (size:
  `exact`/`iec`/`si`; mtime: `relative`/`iso`) and a custom `header`
  (sanitized and capped at resolve), globally or per scheme (scheme wins,
  last-wins per field). Both frontends honor the resolved style at the same
  points they already read the shared layout: custom headers replace the
  Fluent label, cells format through `styled_cell`, align picks the padding
  side, and width overrides flow through the shared layout. Every
  vocabulary is closed and validated at load (a typo is a load error naming
  the path); whether a format fits its column is a resolve-time diagnostic:
  the default is applied and `norte doctor` reports it as
  `columns-bad-spec` (masked, capped) — never a silent skip.
- **Column format cycling in the picker — `f` (#108 block 7b):** inside the
  Alt+C picker, `f` rotates the format of the row under the cursor through
  its closed vocabulary (size: `iec`/`si`/`exact`; mtime: `relative`/`iso`;
  name/kind/opaque rows have no format) and the row shows the current value
  (` · iec`). The cycle starts from the RESOLVED style of the pane's scheme,
  and Enter persists only the formats that actually changed, each as a
  replace-by-id `[[ui.columns.spec]]` entry that preserves the entry's other
  fields (header/width/align) — the session sees the new format immediately,
  in lockstep with the file. One exception: a row whose format is pinned by
  a scheme-level spec is LOCKED in the picker (cycling would write a global
  entry the scheme override keeps masking, and leak into other schemes) —
  edit the scheme spec in `norte.toml` instead. Width cycling stays
  deferred (needs numeric entry UX).
- **TUI column picker — Alt+C (#108 block 7a):** a keyboard-driven overlay
  over the shared picker model: `e`/space toggles a column on or off (name
  is pinned first and immutable), Shift+↑/↓ — or vim-style `K`/`J` —
  reorders below the pinned name, Ctrl+S applies the header-click sort
  semantics to the row under the cursor, Enter applies to the session AND
  persists to `norte.toml`, Esc discards. The save target is one rule,
  stated in the title: the pane's scheme if the config already has an entry
  for it, otherwise the `[ui.columns]` default. Non-builtin ids (attr:/
  plugin:/unparseable) appear as inert-but-editable rows, masked in the
  render, and survive a save verbatim — cleaning the user's config is
  doctor's job. Also fixed here: editing `[ui.columns]` outside now
  hot-reloads into the session (dead since block 4), and a configured
  mid-list `name` is normalized to the front (the TUI budgets the first
  width as the name).
- **Size and date in the GUI listing (#108 block 6):** the pane paints the
  same default column set as the TUI — name, size (IEC), modified (relative
  time) — from the shared layout, under a header row with the ▲/▼ sort
  indicator; the plugin-column headers (G3c) join that row over their fixed
  cells. Sortable headers (Name/Size/Mtime) are clickable: a click flips or
  switches the sort and is remembered per pane for the session, surviving
  cd — persisting it is the picker's job (block 7, with the context menu).
  Absent values stay blank (never a fabricated 0).
- **Column and sort configuration (#108 block 4):** `[ui.columns]` chooses
  which built-in columns each pane paints and the sort order — globally and
  per scheme (an override REPLACES the list; sort vocabulary is closed and
  validated at load). Both frontends seed the sort at startup and re-apply
  it when a cd lands on another scheme (the GUI consumes the sort only —
  its cells arrive with block 6). A configured column id that does not
  parse, or that has no renderer yet (attr:/plugin:), never disappears
  silently: `norte doctor` names it. Per-column width/format overrides
  (`[[ui.columns.spec]]`) land with the picker block.
- **Size and date in the TUI listing (#108 block 5):** the pane paints the
  default column set — name, size (IEC), modified (relative time) — under a
  dim header line carrying the sort indicator; absent values stay blank
  (never a fabricated 0), hostile names keep their badge and never break
  the column alignment, and the first-render budget is unchanged (~0.7ms
  for 100k entries). Column choice/config and the picker are the next
  blocks of the approved design.
- **Manual refresh — Ctrl+R (#106, beta minimum):** reloads both panes
  through the same cancellable path as the post-mutation refresh — marks
  survive by identity with visible pruning, cursor is kept by index, and
  a live-search results pane is left alone. Real directory watching stays
  tracked in #106.
- **Rename and editable destination name (#105):** Shift+F6 renames in
  place (a Move to the entry's own parent — correct inside search results
  too), and F5/F6 with a single item opens an editable destination name
  prefilled with the original; multi-item batches keep the list confirm.
  Byte-exact by rule 1: an untouched prefill copies the ORIGINAL bytes
  (never the lossy form), editing works on the displayed text, and a name
  still containing U+FFFD is rejected instead of writing mojibake.
  Collisions reuse the existing dialog; a failed submit keeps the typed
  name. TUI only — the GUI still has no text input.
- **Create directory — F7 (#104, proto 0.31.0):** `fs.mkdir` as a policy-
  gated, journalled Task (`Created` with undo; clean cancellation), wired
  end to end: engine, daemon (rpc.cancel-able), both backend modes, a TUI
  F7 dialog with the same masking discipline as the pattern dialog, and
  `norte mkdir` in the CLI. Not `mkdir -p`: the parent must exist, and any
  occupant — a directory included — is a conflict. The GUI picks it up
  when it grows text input (same explicit gap as mark-by-pattern).
- **Hidden-entry toggle (#107):** Ctrl+H (and Alt+.) shows or hides unix
  dot-entries per pane, in both frontends; `[ui] show_hidden` seeds the
  startup state. Presentation only — the provider keeps listing everything,
  and while hiding, the pane says how many entries are stashed. Hiding
  prunes marks of the entries it removes (reported, never silent), and the
  live-search results pane is exempt: a hit you asked for is never
  swallowed by the filter.
- **First-class selection (#103):** mark, mark all, invert, clear, and mark or
  unmark by glob, in both frontends; marks survive a refresh (vanished entries
  are pruned, and the status bar says how many), and copy, move, and delete
  operate on the whole selection, consuming the marks on submit. The pattern
  dialog is TUI-only for now — the GUI has no text input yet.
- **Provider attributes on the wire (proto 0.30.0, ADR 0039):** protocol-specific
  metadata — POSIX mode/uid/gid, an SFTP owner string, an S3 storage class, an
  archive member's packed size — can finally reach a client *typed* rather than
  pre-rendered, so a later block can paint it as a configurable column that still
  sorts and formats correctly. Four additive fields across three surfaces —
  catalog, request ×2, entry. `FsCapabilitiesResult.attrs` (an `AttrCatalog`)
  advertises what a provider offers (`AttrInfo` = `id`, `label`, `AttrType`,
  `AttrHint` — the declared type and the suggested format/alignment are separate,
  because two `Uint`s are painted very differently as a byte count and as a
  permission word), `FsListParams.attrs`/`FsStatParams.attrs` request the ids a
  client will actually paint (nothing is delivered unrequested), and `Entry.attrs`
  carries the values as `AttrValue` (`Uint | Int | Text | Bytes | TimeMs | Bool |
  Unknown`). Ids are namespaced by construction (at least one `.`, every segment
  starting with an ASCII letter and continuing in `[a-z0-9_-]`, ≤ 64 bytes — so
  neither the argv-shaped `-x.y` nor the float-shaped `0.0` is an id) and the
  caps — 16 requested ids per call, 64 advertised descriptors, 64-byte label,
  256-byte `Text`/`Bytes`, all counted in BYTES where the schema's `maxLength`
  counts code points — travel in the published JSON Schema (ADR 0038). The
  request surfaces are `fs.list`/`fs.stat` only: `search.hits` and
  `index.query` carry no attributes at 0.30. Wire-only for now: no provider
  advertises an attribute yet and the daemon ignores requested ids, which is a
  valid answer under the contract. `norte-proto` gains `base64` (0.22, already a vetted workspace dep) so
  `AttrValue::Bytes` owns its decode. Three properties are worth stating exactly:
  - All four fields are `skip_serializing_if`-guarded, so a **0.29 peer emits and
    receives byte-identical payloads**; the window becomes N=0.30.x / N-1=0.29.x.
  - **Any malformed attribute VALUE degrades to `AttrValue::Unknown`** — an
    unrecognised tag from a protocol-N+1 daemon (ADR 0004 applied at value
    granularity), a wrong JSON type, a `null`, two known tags at once, undecodable
    base64, an over-cap `Text`/`Bytes`. It costs one cell, never the entry and
    never the page.
  - **The two receive-side fields filter and never error**, while the two
    request fields deliberately do not. `Entry.attrs` drops a malformed key
    and bounds the map at 16 (smallest ids in byte order, so the surviving set
    does not depend on the peer's key order); `FsCapabilitiesResult.attrs` is an
    `AttrCatalog` — a newtype with a private field whose only constructor drops a
    malformed or repeated id keeping the first, clamps an over-long label on a
    char boundary, and truncates at 64, preserving the provider's own meaningful
    order. Making it a type rather than a call is what covers the EMBEDDED
    TUI/CLI path, which never crosses the deserialisation boundary where a plain
    filter would sit. A request keeps a bad id verbatim on purpose: the daemon
    must be able to answer `-32602` instead of silently laundering a caller's bug.
- **Protocol JSON Schema artifact (#13, ADR 0038):** `docs/schema/proto.schema.json`
  is now generated from the same `norte-proto` serde types that speak the wire,
  behind an optional `schema` cargo feature (off by default — the shipped crate
  gains no dependency at runtime). A golden test pins it byte-for-byte and a
  source scan guards that every schema-deriving type reaches the artifact, so it
  cannot silently drift. External clients (the MCP bridge, third-party tooling)
  get a machine-readable contract for the request/response/notification payloads.
  A standalone `just semver` recipe (cargo-semver-checks over the publishable
  crates) is staged for the gate once the binary and a release baseline exist.
- **Plugin previews mark lossy decoding (#101, proto 0.29.0):**
  `PluginPreview` and `PluginPreviewStyled` gain an additive `lossy: bool`.
  When the core's host-side text decoding (§6.2, #29) had to substitute `�`
  for invalid bytes, the viewer now shows a `[lossy decode]` marker next to
  the `via <plugin>` indicator — the same honesty the raw viewer already
  gives via its encoding status. Additive over 0.28.x (`#[serde(default)]`,
  so an N-1 peer reads it as `false`); the window becomes N=0.29.x /
  N-1=0.28.x.

- **Styled plugin previews, end-to-end (G3a):** `plugin.preview_styled`
  (proto 0.27.0, ADR 0037) is now wired from a real WASM guest through the
  daemon and both frontends. `Backend::plugin_preview_styled` (embedded:
  resolve → read → `render-styled`; remote: the wire call) mirrors
  `plugin_preview`'s resolve/read steps but treats ANY runtime failure in
  the styled render (a guest trap, a guest-side error, or a cap violation —
  `RuntimeError::StyledPreviewTooLarge`) as `Ok(None)` rather than an
  error — a styled preview is a pure enrichment over the plain one, so it
  must never block the file; the caller falls back to `plugin_preview`,
  which falls back to the raw view. The daemon handler
  (`handle_plugin_preview_styled`) mirrors that same fallback contract
  server-side. A pre-0.27 daemon is never reachable (handshake rejects it);
  within the 0.27 window a daemon that hasn't wired the handler yet answers
  `MethodNotFound` (-32601), which the remote client also folds into
  `Ok(None)`. `SpanWire::role` travels **unvalidated** across
  `norte-core` (it has no dependency on `norte-theme`, which owns the
  closed `Role` set) — validation happens once, at the frontend boundary
  that actually paints: `norte_frontend::viewer::Viewer::
  with_plugin_preview_styled` resolves each `role` string through the new
  `norte_theme::Role::from_kebab` (reuses the existing serde kebab-case
  derive as the single source of truth for role names, rather than a
  hand-duplicated table), collapsing an unrecognized name to `None` — never
  a panic, never a raw string leaking into a frontend's paint path. `role`
  wins over the raw `fg` fallback when a span carries both (the user's
  theme outranks a plugin's fixed color); every span's `text` is masked
  through the same `display_name` the ANSI-derived preview already used —
  `ansi::StyledSpan` grew a `role: Option<Role>` field shared by both
  preview paths (ANSI-SGR-derived and WIT-structured), so `draw_viewer`
  (TUI) and `render_viewer` (GUI) paint them with one code path. TUI/GUI
  viewer-open flows try the styled preview first and fall back to the
  plain one. TUI: per-span role resolves through `TuiTheme::role`
  (ratatui `Style`, falls back to raw RGB, then to the theme default). GUI:
  `styled_span_color` resolves role through `Theme::style(..).fg` (with G1
  glow applied on top, same as `entry_color`) or the raw `fg` (via
  `norte_theme::Color::rgb` + the existing `theme_map::to_gpui_rgba`, no
  parallel conversion), rendering a flex-row of per-span child divs — as a
  side effect, the GUI now also paints the pre-existing ANSI-derived
  preview in color (it shares the same `StyledSpan` type and render path),
  closing a gap noted in ADR 0037's context section. Covered by a real-WASM
  e2e (`previewer-demo`'s mini-highlighter: digits → `role: "number"`,
  `TODO`/`FIXME`/`norte` → `role: "keyword"` + a fixed `fg` — both are
  deliberately *not* valid `Role` names, proving the unvalidated-wire /
  validated-at-frontend boundary end to end) through `Backend::Remote`
  against a real daemon socket, plus a TUI buffer-inspection test pinning
  role-over-fg precedence and GUI unit tests for the pure color-resolution
  function.

- **Row decorators and plugin columns, host + backend + both frontends
  (G3b, ADR 0037):** `plugin.decorate`/`plugin.column_values` (wire-only
  since 0.27.0/G3a) now have a real handler and are painted end to end.
  New manifest `Category::Decorator` plus an (initially empty, additive)
  `contributions.decorator` — the digest follows the SAME optional-section
  pattern as `[config]`: a manifest without `[[contributions.decorator]]`
  digests byte-identical to before this change, so no existing human
  approval is invalidated by the mere existence of the new category.
  `PluginRegistry::resolve_decorators` returns **every** approved+enabled
  decorator plugin (unlike `resolve_previewer`'s first-match: multiple
  decorators can badge the same page); `resolve_columns(id)` resolves the
  one `columns` plugin declaring that column id. Both are gated on the
  primary `category` (decorator/columns each get their own dedicated WIT
  world, unlike previewer/command which share `norte-plugin`). The
  entries that cross to a guest are **basenames**, never full paths
  (`plugins::paths_to_basenames`) — a decorator/columns plugin sees a
  name, not where it lives in the tree. `Backend::plugin_decorate`/
  `plugin_column_values` (embedded + remote, daemon handlers
  `handle_plugin_decorate`/`handle_plugin_column_values` gated by the same
  read-gate as `fs.list`, extended to the whole batch) are fail-closed
  **per plugin**, never per batch: a plugin that fails to instantiate,
  traps, or breaks the positional 1:1 contract (checked by
  `decorations_to_wire_checked`/`column_values_checked`) is dropped from
  the result with a log warning — the rest of the page still paints.
  TUI: after a listing lands, a background fetch (mirroring the existing
  `Fill`/`StatProbe` one-in-flight pattern) decorates the loaded page and
  installs the result on `PaneState`; `entry_item` paints a badge span
  after the hostile-name-badge slot (role resolves through the theme,
  unstyled falls back to dim). GUI: the same fetch rides the existing
  `SessionCmd`/`SessionEvent` session channel (`Decorate`/`Decorated`,
  double guard on generation *and* dir); `render_row` becomes a flex row
  (name flex-grows and truncates, badge never does) and reuses
  `styled_span_color` for role resolution — `DecorationWire` carries no
  raw `fg`, so an unrecognized role derives a dim tone from the row's own
  color instead. Every badge is masked and capped to 8 chars *after*
  masking (`norte_frontend::sanitize_decoration`, shared by both
  frontends — the same module also flattens the wire's per-plugin overlay
  to one winning decoration per path, `merge_decorations`). Two new
  columns/decorator demo guests (`examples-wasm/decorator-demo`,
  `examples-wasm/columns-demo`) back a real-WASM e2e through
  `Backend::Remote` against a real daemon socket. **Scope note:** GUI/TUI
  column *cells* are not rendered in this change — `plugin.list`'s
  `PluginInfo` does not expose `contributions.columns` today, so a
  frontend has no wire-level way to discover which column ids exist
  without a further (additive) protocol change; that's follow-up work,
  tracked separately from this change's registry/wire/decorator-UI scope.

- **Plugin config on the wire, GUI palette + extension manager, column
  cells (G3c, ADR 0037, proto 0.28.0):** closes the two deferrals G3
  accumulated. `PluginInfo` gains `columns: Vec<PluginColumnInfo>`
  (id + masked header, additive, discovery for the column UI) and two new
  methods expose P2's `[config]` on the wire, which was host-only by
  design until now: `plugin.get_config` (schema + effective value
  together, `PluginConfigKeyWire`) and `plugin.set_config` (validates
  against the SAME schema `config.toml` uses —
  `norte_plugin_host::encode_wire_value` reuses the private
  `encode_override` validator, never a parallel path — before persisting;
  `PluginRegistry::set_config` re-resolves settings in memory so the very
  next `run_command`/`get_config` sees the new value without a fresh
  `discover`). `persist_plugin_setting_typed` fixes a latent gap in P2's
  write primitive: it writes the NATIVE TOML type (`bool`/`int`/`string`)
  the schema declares instead of always a string, which a later
  `resolve_settings` re-parse requires. `plugin.set_config` is gated to
  non-agent connections (same criterion as `plugin.set_approval`: a
  plugin's settings are user data, not something an agent edits on its
  own).
  TUI: the extension manager gains a `[config]` drill-down (Enter on a
  plugin fetches its schema and opens a panel; `bool`/`enum` cycle
  immediately, `string`/`int` open inline editing with client **and**
  server-side range validation) built on a new shared
  `norte_frontend::plugin_config::PluginConfigState`; the settings
  overlay's Plugins section drops the old "edit `config.toml` by hand"
  note and shows one row per plugin with declared settings, drilling into
  the same panel.
  GUI: `app.palette`/`app.extensions` join `crate::keymap::COMMANDS` — the
  shared presets already bound `ctrl+p`/`f12` to them, but the GUI's own
  keymap supplement had claimed `ctrl+p` for `task.prev` (a layer that
  outranks the preset), silently shadowing the binding; `task.prev` moves
  to `ctrl+b` to free it. The command palette (`palette_view.rs`, an
  overlay painted like the modal, same key-capture priority, "modal
  preempts palette" preserved by construction) and the extension manager
  (`extensions_view.rs`, a full-view swap like the settings view, with the
  same `[config]` drill-down as the TUI) are new. `norte_frontend::palette`
  hoists the TUI's `Row`/`plugin_rows`/`rows_for_context`/`first_chord`
  (pure, no `COMMANDS` coupling) so the GUI doesn't re-implement plugin-row
  masking and the `[extension]`-prefix anti-spoofing discipline from
  scratch; each frontend keeps its own `build_rows` (genuinely different
  `COMMANDS`/help-id sources, not incidental duplication). Column *cells*
  (the G3b GUI deferral) now render: a new `SessionCmd::Columns` discovers
  approved+enabled `columns` plugins via `plugin.list`, fetches
  `plugin.column_values` for every declared column over the visible page,
  and `render_row` appends one fixed-width, monospace, masked cell per
  column (TUI columns remain deferred, tracked separately). `norte-i18n`
  gains help text for the four GUI-only commands
  (`mark.toggle`/`task.next`/`task.prev`/`task.dismiss`) that the palette
  now needs to describe, and messages for the config drill-down's
  save/empty feedback, in both locales. Session tests, daemon wire tests
  (validation-then-`INVALID_PARAMS`-without-persisting, agent-denied),
  registry tests, a real-WASM e2e proving a guest reads a value written
  through `set_config` on its very next run (extends
  `plugins_config_e2e.rs`), and a `Backend::Remote`-level e2e for the new
  wrapper methods.

- **GUI settings view (S4):** `app.settings` (`F11`, same shared preset
  binding as the TUI) opens a searchable, VSCode-style full-view swap over
  the same General catalog (S2) — search box, grouped list (General/
  Plugins) with descriptions, mouse AND keyboard (click/hover to select,
  click cycles a bool/enum/theme/keymap-preset row or opens inline text/int
  editing; Enter/Esc mirror the TUI overlay). Writes persist off the UI
  thread through GPUI's background executor (no new OS thread, no coupling
  to the daemon session — config I/O has nothing to do with that
  connection's lifecycle) and, on success, re-resolve what the GUI can
  apply live from the freshly reloaded config: theme + `[effects]`, fonts
  (family/size — resolved once at startup until now, but cheap enough to
  redo on every write), reduce-motion, confirm-quit, quick-search (closes a
  pre-existing gap: the GUI had always hardcoded `Filter` mode, ignoring
  this setting entirely), and the keymap preset (rebuilds both resolvers).
  Only the UI language can't apply live in this frontend (Fluent negotiates
  it once at process startup) — that row carries a static "restart
  required" badge, and any write that couldn't apply live says so in the
  save confirmation. The pure editor state machine (search/cursor/inline
  edit, `SettingsState`) and row builder (`build_rows`) that power the S3
  TUI overlay moved to `norte_frontend::settings` unchanged (they had no
  ratatui/crossterm coupling to begin with) so both frontends share the
  exact same behavior instead of duplicating it; the TUI's own modules
  re-export the same names for source compatibility.

- **TUI settings overlay (S3):** `app.settings` (`F11` in all three bundled
  presets — `F9`/`F10`/`F12` were already taken) opens a searchable overlay
  over the General settings catalog (S2): type to filter by id, name, or
  description; Enter toggles a bool, cycles an enum/theme/keymap-preset
  setting immediately, or opens inline text/int editing (Int validates its
  range before writing — an invalid value shows a status-bar error and
  changes nothing). Every write goes through the same comment-preserving
  `norte_config::persist_set` as the theme picker, and the existing
  hot-reload picks it up live; the overlay stays open across a reload and
  refreshes its rows in place instead of closing, unlike the help/palette
  overlays. The Plugins section shows a single informational row for now:
  editing plugin settings from the UI needs a protocol bump the wire
  doesn't have yet (P2's `ConfigKeySpec`/`settings_of` aren't exposed to a
  remote frontend) — until then, edit `plugins/<id>/config.toml` by hand
  and validate with `norte doctor`.

- **`[ui] confirm_quit` (S2):** a new `norte.toml` setting controls whether
  quitting asks for confirmation — `"auto"` (default, unchanged behavior)
  confirms only with pending work, `"always"` always confirms even with
  nothing pending, and `"never"` closes immediately. Wired end to end in
  both frontends: the GUI's existing quit-confirmation modal now honors the
  three modes (and shows a generic title instead of "0 task(s), 0 mark(s)"
  when `"always"` fires with nothing pending), and the TUI gains its own
  confirmation modal on `app.quit` (Ctrl+C's emergency-exit shortcuts stay
  immediate everywhere, unaffected by this setting) — `"auto"` there
  confirms only when the task board has work in flight. A generic
  comment-preserving config writer (`norte_config::persist_set`) and a
  curated, Fluent-localized settings registry
  (`norte_frontend::settings::catalog`) land alongside it as the shared
  foundation the upcoming in-app settings UI (VSCode-style, searchable) will
  build on.

- **Per-directory cursor memory (S1):** both the TUI and the GUI now
  remember where the cursor was in each directory you visit this session
  (in-memory only, capped at 64 directories, byte-exact identity — hostile
  path twins are never folded together). Navigating to the parent directory
  now selects the folder you just came from, instead of always landing on
  the first entry.

- **GUI opt-in motion (G2, ADR 0036 amendment):** the `[effects]` theme
  schema grows to v1.1 with `flicker = { strength }` (CRT flicker, clamped
  to `[0.0, 0.15]` — deliberately tiny, an accessibility guard against
  photosensitive-trigger risk) and `cursor_blink` (bool); both render in the
  GUI. `fade_ms` (clamp `[0, 400]`) is parsed but not yet animated — a
  documented, deliberate schema/render split, not an oversight. The bundled
  `retro-crt`/`retro-crt-amber` presets now declare `flicker`+
  `cursor_blink` by default. A new `[ui] reduce_motion` config key (spec
  §17 a11y; last-wins across every layer including Project) forces all
  motion off via GPUI's native `App::set_reduce_motion`, which also frees
  `with_animation`-driven cursor blink from any hand-rolled reduce-motion
  check. The frame loop stays alive ONLY while a motion effect is active
  AND the window is focused AND `reduce_motion` is off — spot-checked with
  `NORTE_GUI_DEBUG`'s render counter (a focused retro-crt window renders
  continuously; the same theme under `reduce_motion = true`, or any theme
  with no motion keys, settles after the initial listing and goes fully
  event-driven, exactly as before this feature landed). This manual check
  is not yet pinned by an automated test — tracked as follow-up debt
  (no `gpui::test` harness exists in `norte-gui` yet to drive one).

- **Declarative per-plugin configuration (P2):** a plugin manifest can now
  declare typed settings under `[config.<key>]` (`string`/`bool`/`int`/`enum`,
  with an in-range default, an optional description, and per-type caps —
  ≤32 keys, key charset `[a-z0-9-]{1,32}`, ≤280-char strings/descriptions,
  ≤16 enum values). The schema is **inside the approval digest** (it decides
  what a plugin can be configured to do, same as `category`/`contributions`):
  a manifest with no `[config]` digests byte-identical to before P2 (existing
  human approvals are never reset), and any change to the schema — including
  just a default value — moves the digest and forces re-consent. Values live
  in `config_dir/plugins/<id>/config.toml` (flat `key = value`, validated
  fail-closed at discover time: an unknown key, a wrong TOML type, an
  out-of-range int, or a non-member enum value excludes the **whole plugin**
  from the catalog as a load error naming the offending key — never the
  value, #73) and are resolved to defaults-with-overrides applied.
  `norte doctor` gained a `plugin-config` finding per resolved key
  (`{id}: {key}={value}`, masked and capped like everything else untrusted in
  its report) for every plugin that declares `[config]`. Delivery to the
  sandboxed guest is a new WIT interface, `host-config` (`get`/`all`,
  package `norte:plugin@0.5.0`), linked for every plugin the same way
  `host-log` already is; values reach the guest through it for `command`,
  `previewer`, **and** provider guests alike, wired at every instantiation
  site (the embedded CLI/backend path, the daemon's `plugin.run_command`/
  `plugin.preview` handlers, and the previewer path in both). A provider
  guest (today, only FTP) never receives `[config]` — not an oversight:
  providers aren't discovered through the plugin manifest/catalog system at
  all, they're driven by `connections.toml`, a structurally separate config
  path with no `[config]` schema to resolve; the delivery plumbing
  (`PluginProvider::set_settings`) exists and is safe to call, ready for the
  day a provider *does* originate from a plugin manifest. The WIT package
  bump is **not** backward compatible with previously-compiled `.wasm`
  artifacts, verified empirically (not just by the pre-existing shared-package
  caveat in the WIT file's own history comment): instantiating a `command-demo`
  build from before the bump against the post-bump host fails outright
  (`component imports instance norte:plugin/host-log@0.4.0, but a matching
  implementation was not found in the linker`) — every precompiled guest,
  including the embedded `ftp-provider.wasm`, had to be rebuilt
  (`just build-ftp-wasm`). The extension manager's settings display is
  **deferred** to the wire (`PROTOCOL_VERSION`) bump G3 already requires:
  settings live host-side and the manager is wire-fed, so there is nothing to
  show there yet — `norte doctor` (which runs embedded) carries the display
  burden in the meantime.

- **`norte doctor` (H2):** read-only diagnostics over config layers,
  keymaps, plugins, and connections — `[config]` (parse errors per layer,
  and a split-brain warning when `NORTE_CONFIG_DIR` shadows a legacy dir
  with its own config files), `[keymap]` (structural errors — bad TOML,
  ambiguous prefixes, bad chords — vs. an honest per-screen approximation
  that downgrades an unrecognized layer command to a warning against the
  three bundled presets' own vocabulary), `[plugins]` (broken manifests,
  a plugin approved but whose capabilities digest went stale since —
  re-approval required — and a missing `plugin.wasm`), and `[connections]`
  (parse errors, invalid endpoints, and — side-effect-free v1 — whether the
  `NORTE_SECRET_<CONN>` env var a password/access-key connection falls back
  to is present; the OS keyring and `secrets.age` are explicitly NOT probed,
  since either could prompt or touch the keychain). `--json` emits a stable
  `{ findings, summary }` shape for tooling — locale-free and secret-free by
  construction: a `Finding`'s `detail` only ever carries machine values (ids,
  paths, var names), never the underlying library error's raw `Display`
  (a `connections.toml` syntax error inside a `password = "…` line is
  reported generically, never echoing the fragment); the handful of
  narrative sentences (e.g. "re-approval required") live in the text
  renderer, keyed by finding code, and are looked up in the user's locale.
  A layer's `lua:<name>` binding with an invalid Lua identifier is reported
  as a single structural error instead of retrying a fix that can never
  converge. Exit code is non-zero only when a finding is an error (a
  warning-only report still exits clean), and the full report always prints
  regardless of the exit code.

- **Command palette (H1, `Ctrl+P`; vim preset also `:`):** a filterable
  overlay lists every command with its Fluent description and its first
  bound chord (falling back from the browse to the viewer keymap); Enter
  dispatches the highlighted row through the exact same path a keypress
  would. Rows are precomputed from the effective keymap, like the F1 help
  and the dialog footer hints below, and refreshed on every hot-reload.

- **Plugin descriptions and commands on the wire, in the extension manager,
  and in the palette (P1, `PROTOCOL_VERSION` 0.26.0):** a plugin manifest can
  now declare an optional `description` (cosmetic, capped at 280 characters,
  outside the approval digest — editing it never resets an already-approved
  plugin's consent) and its `contributions.command` entries are exposed on
  `PluginInfo` alongside it. The extension manager (F12) shows the
  description as a dimmed second line under each plugin, masked and
  ellipsized like the rest of third-party text. The command palette
  (`Ctrl+P`) now fetches the plugin catalog on open and appends one row per
  command of every *approved and enabled* plugin, masked and tagged with an
  `[extension]` prefix that no built-in row can carry (a hostile plugin
  cannot spoof a built-in command by copying its exact display text); Enter
  runs it through `plugin.run_command` and shows the (masked, capped) result
  on the status bar. The row's internal dispatch key is never painted — a
  command id from the manifest has no charset validation of its own, unlike
  the plugin id.

- **Generated dialog footer hints (#24):** the confirm/collision/agent
  approval/host-key-trust modals and the theme picker, extension manager, and
  favorites popup now show a footer built from the *effective* `dialog`
  keymap — the join of the overlay's supported commands, the keys actually
  bound (preset plus any user layer), and a short label. Rebinding a dialog
  key can no longer desync its own hint. Notable key changes that came out of
  this: the collision dialog's "keep newer" moved from `n` to `w` — on
  collision, `n` (`dialog.deny`) is simply **inert**, not a cancel; it is not
  bound to anything the collision dialog listens for, it just no longer does
  "keep newer" by accident. The extension manager's approval toggle moved
  from a hardcoded `a` to `dialog.approve` (`y` in the bundled presets — `a`
  is now `dialog.add`, used by the favorites popup), and its `q`-to-close
  fallback was removed (`Esc` closes it, like every other overlay). The
  orthodox/cua presets also lost a hardcoded `k`/`j` fallback in the theme
  picker and extension manager: `k`/`j` now only navigate overlays under the
  **vim** preset, via its own `[dialog]` bindings, not as a blanket default.
  A later pass (encoding audit H1) found and fixed a masking gap: a hostile
  `./.norte/keymap.toml` project layer could bind a bidi-override or other
  hazardous codepoint to a supported dialog command, and that raw codepoint
  would reach the generated footer and the palette's chord column unmasked —
  both render sites now mask hazards the same way the query bar already did.
  A follow-up pass also fixed hint text that could get cut mid-word on an
  80-column overlay by dropping self-evident arrow/paging keys from
  non-modal hints and sizing the theme picker and extension manager boxes to
  their footer instead of a fixed width.

- **Content-match preview in live search (#81):** with a content search
  active, the status bar shows the line number and a sanitised preview of the
  match for the hit under the cursor.

- **Show names as… (#57):** `Alt+E` cycles a per-pane reinterpretation of
  non-UTF-8 file names for display (cp437, cp866, Shift-JIS, GBK,
  windows-1252), with a chardetng suggestion as the first step. Display only:
  bytes never change, reinterpreted names keep their hostile badge, and the
  status bar shows the active mode persistently. Valid UTF-8 names are never
  reinterpreted. Quick search matches against the reinterpreted text (typing
  "П" finds the entry shown as "Папка"), and decision surfaces — confirm and
  collision dialogs, viewer title, navigation popups, the search dialog root —
  follow the pane's active reinterpretation (#98).

- **Omitted-entries badge for archives (#93, protocol 0.22.0):** listings of
  zip/tar/tar.gz containers now report how many entries the index omitted
  (hostile names, anti-bomb limits) through the new optional
  `FsListResult.skipped` field. The TUI shows a persistent status-bar badge
  ("N entries omitted") and `norte ls` prints a warning to stderr — an
  incomplete listing is never silent.

- **Configurable archive limits (#95):** the new `[archive]` section of
  `norte.toml` (`max_entries`, `max_decompressed_bytes`) lowers the anti-bomb
  limits for browsing containers. The project layer (`./.norte`) is ignored
  for this section — a foreign repository must not be able to raise safety
  limits.

- **Themes (MT milestone, ADR 0020):** the TUI now uses the shared
  `norte-theme` crate. It provides semantic roles, true-colour values with
  256- and 16-colour terminal fallbacks, styles by node type and extension, and
  bundled presets (`default`, `catppuccin-mocha`, `gruvbox-dark`, and `nord`).
  Select a preset or a custom TOML file with `[ui].theme`. The setting is hot
  reloaded and falls back to `default` on error. An `[effects]` section is
  reserved for the GPU-backed GUI. See [the theme guide](docs/theming.md).
- **Light themes and explicit backgrounds:** added `gruvbox-light` and
  `catppuccin-latte`, plus a `background` role so both light and dark themes
  control the terminal's base colour.
- **Theme picker:** press `F9` to preview bundled themes. Enter applies and
  saves the choice to the user's `norte.toml` without discarding comments or
  formatting; Esc restores the previous theme.
- **Roadmap update:** after M2, the planned order became MT (themes), M4
  (plugins), M3 (agent integration), and M5 (GUI).

### Changed

- **Streaming prefix rename on object storage (#49):** renaming an S3 prefix
  no longer materialises the whole tree in memory (peak is now proportional
  to the number of directories) and deletes in batches (`DeleteObjects`).
  The operation stays non-atomic: an object created concurrently under the
  source prefix during the rename is left unmoved; a concurrent overwrite of
  an already-copied object can be lost.

- **Honest resource errors for archives (#95, protocol 0.23.0):** a container
  that exceeds a local anti-bomb limit now fails with the new `limit_exceeded`
  error (closed vocabulary: `entries`, `decompressed-bytes`) instead of
  masquerading as `corrupt` — a legitimate huge tar.gz is not "corrupt".
  Older clients degrade to a generic error.

- **Listing sort keys allocate less (#94):** the persisted NFC sort key is
  only materialised when it differs from the raw name bytes (non-ASCII NFD
  names); ASCII, already-NFC, and non-UTF-8 names no longer allocate.

### Added

- **`norte tui` and `norte gui`:** the CLI now launches either frontend,
  handing the process over (unix `exec`: same pid, same terminal, same
  signals) and preferring the binary installed next to itself over
  whatever the `PATH` finds first. Arguments pass through verbatim.

- **`norte-gui` takes a starting directory** too, plus `--socket`,
  `--help` and `--version`. Command line beats `NORTE_DIR`/`NORTE_SOCKET`,
  which beat the current directory and the daemon's own socket — resolved
  in one place (`LoadConfig::resolve`), with no `set_var` detour. The
  binary also stops reporting version `0.0.0`.

- **`norte-tui` takes a starting directory** and real `--help`/
  `--version`. The positional argument used to be the keymap preset,
  which nobody guessed; it is now the directory to open, with the preset
  behind `--preset`. An unknown flag is named and refused instead of
  being silently ignored — `--help` used to fall into that branch and
  the binary died trying to take over a terminal.

### Fixed

- **The F1 help no longer disappears, and neither do the columns
  (bugfixing session):** seven defects that only showed up by driving
  the real app.
  - Overlays are painted over the viewer. `ui::draw` returned right
    after the viewer, so any overlay opened on top of it stayed
    invisible while still swallowing every keystroke — the run loop
    routes them before the viewer, and `f1 -> app.help` is a `[global]`
    binding, so it is live on the viewer screen too.
  - An in-flight modal wins the key over every overlay, not just the
    palette and the settings pane. The modal is painted last, above
    everything, but the key chain resolved first against the theme
    picker, columns picker, extension manager, nav popup, search dialog
    and help — so a keystroke aimed at the modal landed in a text field
    or toggled the highlighted plugin.
  - The transfer-name modal elides its paths in the middle instead of
    letting the box border cut them, which used to expel the tail of the
    destination with nothing to signal it.
  - A cursor at the top stays at the top while a paginated listing
    fills. It was re-anchored to the path under it, and the first page
    of a local listing arrives in readdir order, so a 5000-file
    directory opened showing its tail.
  - Size and Date are hydrated for every visible row in both panes
    (TUI and GUI, #123), not just the focused one — with the lazy local
    listing (#52) those columns were otherwise blank.
  - Paging and the stat probe use the real viewport height (#124)
    instead of a fixed 10 rows and a fixed radius.
  - Config hot-reload only fires for `norte.toml`/`keymap.toml`/
    `openers.toml`. The native watcher can only watch a directory and
    forwarded every event, so `index.db`/`journal.db` writes — SQLite,
    in the same directory — reloaded the config and closed the open help
    and palette. Read events (`Access`) are ignored too: the reload
    re-reads the layers, so counting an open as a change fed the cycle.
  - Text inherits the THEME's foreground. Only the background came from
    the theme, so every span without an explicit `fg` (including the
    column cells and header, painted with a bare `DIM`) kept the
    TERMINAL's foreground: with a light theme in a dark terminal they
    were painted almost in the background color. Two tests over every
    shipped preset now pin this and a 3:1 floor for text.

- **Semantic signals reach WCAG AA in every preset:** `error`, `warning`
  and `hostile-badge` — the last one marks a masked name (spec §6), a
  security surface — fell as low as 2.31:1 against their own theme
  background. Adjusted in `catppuccin-latte`, `gruvbox-light`, `nord`
  and `gruvbox-dark`, keeping each palette's hue, and pinned by a test
  over all presets. Decorative roles (borders, status bar accents) keep
  their looks and their lower floor.

- **Dialog keys are now discoverable in-app (#113):** the F1 help gains a
  "Dialogs and overlays" section built from the effective `dialog` keymap —
  the same generated-from-config invariant as the other sections. Overlay
  footers stay space-filtered (arrows and, in the columns picker, the
  reorder verbs are dropped to fit 80 columns), but every dialog verb and
  its real chord is now listed somewhere reachable without opening the
  manual; a note clarifies each dialog supports its own subset.
- **Config writes are now atomic and cross-process safe (#116):** every
  `norte.toml` persist helper (theme, settings, columns, formats, hotlist)
  used to read-modify-write the file in place — two writers (e.g. the GUI
  and the TUI on the same config) could interleave and silently drop each
  other's changes, and a concurrent reader could catch a truncated file
  whose partial parse a later write would then rewrite, losing unrelated
  user sections. Writers now take a cross-process advisory lock
  (`norte.toml.lock`, held for the whole read-modify-write; released by
  the OS even on crash) and replace the file via a synced sibling tmp +
  atomic rename, preserving existing file permissions — readers see the
  old or the new file, never a torn one. Per-plugin `config.toml` writes
  got the same treatment (#119).
- **Ctrl+R skipped the post-refresh ritual (#118):** `pane.refresh` ran from
  the command dispatcher, which cannot see the run loop's paginated fill or
  the stat-probe dedup — with a large listing still streaming in, the old
  drainer kept appending batches onto the freshly refreshed pane
  (duplicated rows until the next cd) and the focused entry could refuse to
  re-hydrate its lazified size. The refresh outcome now travels back to the
  run loop (`Cd::Refreshed`), which applies the same ritual as
  mutation-completed refreshes: the drainer is released only for panes that
  truly got a complete listing (an Esc mid-refresh keeps the other pane's
  still-valid fill), and the probe dedup is invalidated.
- **Palette and quick-search dispatch sites missed the resolver's
  post-command tail (#118 review):** a cd chosen from the command palette
  (or a quick-search Enter landing in a hit directory) could exit a pane's
  virtual search mode without cancelling the live search task, and a
  `pane.open` picked from the palette left the resolved external command
  queued until the next keypress. Both sites now reap the search run; the
  palette also launches the pending opener immediately.
- **Daemon plugin previews skipped host-side text decoding (#101):** the
  `plugin.preview`/`plugin.preview_styled` daemon handlers passed RAW bytes
  to the previewer guest, unlike the embedded backend, which decodes to text
  first (§6.2, #29) — a latent behavior gap between embedded and daemon
  mode. Both handlers now decode through the shared
  `plugins::decode_for_preview`, matching embedded and surfacing the new
  `lossy` flag.
- **`norte doctor`'s keymap check used an O(n) retry loop (#102):** unknown
  `run` names were discovered by rebuilding the effective keymap once per
  distinct typo (capped at 256), a loop that could never converge for a
  `lua:<name>` binding failing the charset and had to special-case it. It is
  replaced by a single-pass `Effective::build_diagnostics` that reports every
  finding at once — no retry, no cap, no non-convergent case.
- **`persist_set` panicked on a malformed `[section]` (S review I1):** a
  hand-edited `norte.toml` with a scalar section (`ui = 3`) or an
  array-of-tables (`[[ui]]`) made the settings-write primitive panic instead
  of returning an error — reachable in both frontends' background write
  task (GUI: could take the process down; TUI: the panic was swallowed
  silently, leaving the settings row optimistically showing "edited" even
  though nothing was written). Now a clean `io::ErrorKind::InvalidData`; the
  TUI's previously-silent panic arm now shows a status message too.
- **Settings paint order (S review M3):** a modal (e.g. an async policy
  approval) painted UNDER the TUI's command palette or settings overlay
  when both were open, even though key input already treated the modal as
  authoritative — the pixels lied about who was in control. The modal now
  paints last, on top of every overlay.
- **`nav.parent`'s cursor-memory hint could survive a failed `cd` (S review
  M2):** landing back on the child you came from only worked after a
  *successful* navigation; a failed one (permission denied, a dead session)
  left the hint set, ready to hijack an unrelated future navigation's
  cursor placement. Both frontends now clear it on failure.
- **`ui.font-size` couldn't be edited to a fractional value (S review M4):**
  the settings UI's `Int` editor only accepted whole numbers, even though
  `[ui] font_size` is a float — a hand-set `14.5` was invisible to the
  editor (typing it back always failed). It now accepts a fractional part
  and round-trips it.
- **Silent short reads from zip entries (#95):** a zip whose central directory
  promises more bytes than the deflate stream delivers now fails loudly with
  `corrupt` mid-stream instead of silently returning a partial file.

## [0.3.0-alpha.1] - 2026-07-15

The first tagged release completes the M2 milestone: remote providers and
archives. norte can manage local files, remote storage, and compressed archives.
This is an alpha release; the interface and configuration may still change,
and some daemon/socket tests are only available in CI environments.

### Added

#### M2: remote providers and archives

- SFTP provider based on `russh`, with accurate capabilities, byte-safe names,
  and containment for hostile names and symlinks.
- Object-storage provider based on `opendal`, with server-side S3 copies,
  cursor pagination for large listings, and byte-exact UTF-8 keys.
- Read-only ZIP and TAR provider that exposes archives as virtual directories
  (`zip+...!/path`), honours ZIP filename encoding, and enforces zip-bomb
  limits.
- Cross-provider copy engine with resumable `.norte-partial` files, multipart
  S3 support, and destination-side overwrite protection.
- Remote logical trash at `.norte-trash/` for providers without an operating
  system trash facility, including byte-safe origin metadata.
- JSON-RPC 2.0 daemon over Unix-domain sockets or Windows named pipes, with
  peer-credential authentication, NDJSON framing, automatic startup, and idle
  shutdown. Frontends can use embedded or daemon mode.
- Connection profiles and secret handling through `connections.toml`, system
  keyrings, and trust on first use for SSH host keys.
- End-to-end coverage of the release criterion (remote ZIP to S3 to local),
  framing and ZIP-name fuzzing, copy benchmarks, and nightly tests using
  testcontainers.

#### M1: usable terminal interface

- A ratatui dual-pane TUI, configurable keymaps, layered hot-reloaded
  configuration, an encoding-aware viewer, and explicit fallback when trash is
  unavailable.
- Fluent localization resources for English and Spanish.

#### M0: foundation

- Cargo workspace, protocol and VFS crates, byte-preserving `VPath`, cancellable
  task scheduling, local copy/move/delete with progress, and CI on three
  operating systems.

### Notes

- Filenames remain bytes throughout the stack. The canonical `norte-testkit`
  corpus covers hostile and non-UTF-8 paths.
- `norte-proto`, `norte-vfs*`, and `norte-testkit` are available under either
  Apache-2.0 or MIT. `norte-core` and the official frontends are
  AGPL-3.0-only.

### Planned

- Agent-facing MCP server, policy engine, journal, session undo, and audit
  export.
- Writes inside ZIP archives; list, restore, and purge operations for logical
  trash; and the M5 GUI.

[Unreleased]: https://github.com/compilando/norte/compare/v0.3.0-alpha.4...HEAD
[0.3.0-alpha.4]: https://github.com/compilando/norte/compare/v0.3.0-alpha.3...v0.3.0-alpha.4
[0.3.0-alpha.3]: https://github.com/compilando/norte/compare/v0.3.0-alpha.2...v0.3.0-alpha.3
[0.3.0-alpha.2]: https://github.com/compilando/norte/compare/v0.3.0-alpha.1...v0.3.0-alpha.2
[0.3.0-alpha.1]: https://github.com/compilando/norte/releases/tag/v0.3.0-alpha.1
