//! The envelope EVERYTHING that crosses to the renderer travels in, and its
//! caps.
//!
//! A renderer —a webview, a Flutter shell, a headless test— shares no
//! memory with the host: it receives messages. This module says what those
//! messages look like and what is required of each, which is what stops a
//! renderer written by someone else from interpreting half a message and
//! carrying on as if nothing happened.

use serde::{Deserialize, Serialize};

/// Bridge contract version.
///
/// Not the daemon protocol's: they are two different boundaries and move
/// for different reasons. A renderer that does not recognize this version
/// does NOT interpret the message: it shows an incompatibility screen (ADR
/// 0066).
///
/// **The rule for when the number moves is in ADR 0068**, section "And the
/// version rule, written down", and it is SINGLE-LEVEL: any shape change
/// —even a purely additive one— bumps the number, and there is no
/// compatible level. This is deliberate and has written preconditions: a
/// compatible level is only honest if an old peer can decode a new payload,
/// which requires `#[serde(default)]` on every added field and a renderer
/// that resyncs on a patch it does not know instead of dropping it. Neither
/// holds today, and adding the `default`s without the other would let an
/// old payload decode as a new one with the screen half-done — which is
/// worse than one that says it cannot read it. Reopening the rule is a new
/// ADR, not a patch here.
///
/// - **50**: a preview span carries a BACKGROUND (`SpanView::bg`, proto
///   0.66.0, D4): the image previewer paints half-blocks with two pixels
///   per cell, and without a background half the image does not exist. And
///   the host tells the previewer the viewer's width in cells so it can
///   shrink to fit.
/// - **49**: the viewer carries the SPANS of a plugin preview
///   (`ViewerView::styled`): text, theme role or its own color, one entry
///   per `lines` row. The TUI painted roles and colors since ADR 0037 and
///   the window flattened them to text; now both frontends show the same
///   thing.
/// - **48**: the log panel ALSO reads the daemon's (#328). The slot says
///   which source is being shown (`source_mode`), whether there really is
///   a second one to offer (`sources_available`) and what to say about it
///   (`source_note`); each line says which process it came from, and
///   `log_cycle_source` cycles through the three.
/// - **47**: a slot in ERROR can be retried on its own (`refresh_slot`).
///   The common case on reopening is a remote connection asking for its
///   password, and with nothing to press the only way out was navigating
///   elsewhere to be able to come back.
/// - **46**: the screen can carry the LOG PANEL (#326): the in-memory
///   ring's visible window, with its level, its filter and whether it
///   follows the end. Says which PROCESS the lines are from, because the
///   window starts its own daemon and its lines are not that daemon's.
/// - **45**: a dialog can ask for a PASSWORD (#327). The field is marked
///   secret and what travels from host to renderer is DOTS, never the
///   text: the renderer does not paint it, does not re-seed it and cannot
///   log it.
/// - **44**: the renderer can DRAG the border between two slots. It sends
///   where the pointer is in layout cells; which pair splits and how much
///   each gets is decided by the host, which has the split and the
///   minimums.
/// - **43**: the screen can carry the PROFILE picker (ADR 0079), with the
///   active one marked, what fails to load stated with its reason, and the
///   two warnings the spec asks for by name: what else is called the same
///   and which profile cannot save state.
/// - **42**: the theme screen CHOOSES: it carries the theme list and the
///   cursor, and moving through it previews live. It used to show only the
///   one already set, because the host resolved the theme once at
///   startup; now the host tells it over the native channel and the
///   catalogue crosses again.
/// - **41**: the screen carries the MENU BAR —the titles, the open menu and
///   its entries with their shortcut— and the renderer can open, point,
///   run and close. Menus are `norte_frontend::menu`, the same model the
///   TUI paints.
/// - **25**: a plan's review says what it was missing to be approved
///   knowingly: how much is shown out of how much there is and how many
///   renames it will ACTUALLY do —both already translated, because the
///   catalogue does not substitute variables—, whether there is an altered
///   name OUTSIDE the window, and whether the reader has walked the whole
///   plan. And it can be answered with the mouse.
/// - **36**: a listing says how many entries it is HIDING because they are
///   hidden, permanently and not as a message the next key overwrites. A
///   listing that shows less than there is must not go silent (#107,
///   #293).
/// - **35**: the layout carries its TAB groups —what is behind what is
///   painted, with each one's label and its flag—, because an inactive tab
///   is not painted and without this the window showed the front one
///   without saying there were others open.
/// - **34**: the agents panel carries a GENERATION —the list reorders
///   itself, so a click has to say which one it is speaking against—, how
///   many sessions have been forgotten because of the cap, what to say
///   when it is empty (which is not always the same thing), and whether a
///   session already has an undo in progress.
/// - **33**: the window carries the AGENT sessions it has seen ask for
///   permission, with how many each one asked for and how many were
///   approved from here. It is where the operand for undoing a whole
///   session comes from (#276): chosen from a list, never typed.
/// - **32**: what 6.4's reviews changed in shape: a command's output
///   travels by LINE and with a flag per string —who, what and what was
///   printed, each with its own—, plus the extension's reverse-DNS id, and
///   a palette row says whether what is painted differs from what whoever
///   contributes it declares.
/// - **31**: the extension manager GOVERNS: the detail card carries the
///   `[config]` editor (which key is chosen, what is being typed and which
///   keys this build knows how to edit), the commands the extension
///   contributes, and the output of the last one run.
/// - **30**: the sync panel APPLIES: the second question, the report's
///   failures and the cancellation's ack cross, and a path's anchor can be
///   `either`. A renderer at 29 would read `undefined` where there is now
///   a list.
/// - **29**: the screen can carry a sync PLAN: its steps with the undo
///   perspective, its summary, what blocks it and whether it can be
///   approved.
/// - **28**: the screen can carry the DIFFERENCES panel: rows paired by
///   the core, its per-category filters and a window of the visible ones.
/// - **27**: a search can be SEMANTIC, and then its rows carry how similar
///   they are.
/// - **26**: what is masked is also STATED in persistent notices, in a
///   layout's diagnostics, in an unprojected kind's name, in a theme's
///   effect keys and in a key's label; and an approval says what it asks
///   for, who asks and until when, in its own fields.
/// - **24**: the screen can carry a RENAME PLAN under review: the pairs the
///   model proposes (from name to name, each whole and separate — never
///   concatenated with an arrow), the core's verdict and its collisions.
///   Arrives in two steps: first the plan, then the verdict afterward,
///   because checking it against the directory is another trip.
/// - **23**: a dialog stops being plain text. Its body is LINES
///   ([`crate::dto::DialogLine`]), each saying whether what is painted
///   differs from the real thing; the DESTINATION travels in its own field
///   and not as a line with an arrow, because a directory can be named
///   `docs → /home/DELETE` and that arrow is legitimate; and if the body
///   shows fewer items than the operation touches, it says so. A task also
///   says whether the file it is currently on paints differently from what
///   it is.
/// - **22**: the viewer says whether what it shows is a paintable IMAGE and
///   how big it says it measures, or why it refuses to paint it. Its bytes
///   do NOT travel in the snapshot: they are requested separately (ADR
///   0069).
/// - **21**: the viewer says whether what it shows was produced by a
///   PLUGIN, and whether the decoding it was given was lossy.
/// - **20**: the screen can carry the COLUMN PICKER: what is painted, in
///   what order, with what format and against which scheme.
/// - **19**: a listing says how many entries the provider SKIPPED.
/// - **18**: a row can carry the BADGE a plugin put on it, with the theme
///   role to paint it with, and a `plugin:` column brings its value. Only
///   requested for the visible WINDOW.
/// - **17**: the sidebar and the picker carry a GENERATION, and a click
///   with a mismatched one is rejected. Breaking: volumes arrive from a
///   background task and are inserted into the middle of the list, so a
///   bare index could navigate to somewhere nobody clicked.
/// - **16**: the screen can carry a subtree SEARCH, with its hits arriving
///   in batches as it runs.
/// - **15**: the screen can carry the LAYOUT PICKER, with each layout's
///   shape painted by the same engine that does the real split.
/// - **14**: a slot can be the PLACES SIDEBAR.
/// - **13**: a slot can be the ATTRIBUTE SHEET or the PROCESSES PANEL,
///   instead of a gray rectangle with its type's name.
/// - **12**: the screen can carry the THEME from the inside (role by role,
///   with the effects this renderer does not paint) and the VOLUME
///   PICKER.
/// - **11**: the screen can carry the EXTENSION MANAGER read-only: what is
///   installed, what each one asks for and what has been configured on it.
/// - **10**: the screen can carry SETTINGS read-only: the shared catalogue
///   with its effective value, and where each thing lives.
/// - **9**: the screen can carry HELP: the corpus in closed blocks, with
///   its live marks already resolved against the reader's keymap, and the
///   keyboard sheet generated from the effective map.
/// - **8**: the screen can carry the command PALETTE.
/// - **7**: a half-typed prefix carries its CONTINUATIONS (which keys
///   follow, what each does and which cannot be done here).
/// - **6**: headers and the viewer travel as a PATCH, and the renderer
///   declares how many lines fit in the viewer.
/// - **5**: every action that names a row ALSO carries the generation in
///   which the renderer saw it, and the host compares it to the listing's
///   epoch. Without that pair the key is an index, and an index from the
///   previous screen names a different file.
/// - **4**: the screen can carry a VIEWER (decoded text in lines, or
///   hexadecimal if the content is binary).
/// - **3**: every listing carries its HEADERS (translated label, sort
///   column and direction), and sorting by column can be requested.
/// - **2**: the snapshot carries the screen's split
///   ([`crate::dto::LayoutView`]) and goes FULL (dialogs and the board
///   included); a focus change travels as a patch and not a snapshot.
/// - **1**: phase 2's initial contract.
///
/// - **51**: the snapshot carries the PANE BAR (#324): buttons derived from
///   the kind registry, with state and novelty, and one action by index to
///   press them. Also travels as a patch (`ViewChange::PanelBar`) in any
///   send that changes it.
/// - **52**: A PROGRAM'S OUTPUT (#312): what a program the host ran and
///   waited for printed —the two-file comparer—, as a snapshot and as a
///   patch, and the action the host uses to send it back.
/// - **53**: the renderer declares how many COLUMNS the viewer's body has
///   (`SetViewerCols`), as it already declared the rows: it is the width
///   the previewer receives.
/// - **54**: a transfer dialog says where its DESTINATION CHECK stands
///   ([`crate::dto::DestCheckView`]): whether it fits (#149) and whether it
///   knows how to hold its writes (#164). The number goes up even though
///   the field carries `serde(default)`, and that is the reason to bump
///   it: an old renderer paired with this host does not know the field,
///   would not paint #164's line and would not say so — and that line's
///   absence MEANS the destination confines. The webview is embedded in
///   the binary, so that pairing is what comes of forgetting `just
///   link-gui`.
/// - **55**: a listing's header carries the FOUR marks it was missing that
///   the terminal has always had: that it is filling in —and how many are
///   left—, that names are reinterpreted (#57), that a refresh ate marks,
///   and how many are marked and how much they weigh. All under the same
///   rule: a listing that shows less than there is, or that does not show
///   what there is, is never silent.
/// - **56**: a slot that is LOADING says where it is going (#323). The body
///   keeps showing the previous listing until the new one arrives —on
///   purpose, so a failure leaves the reader where they were—, and without
///   the destination that mix cannot be read. The 250 ms threshold is set
///   by the renderer, which is where a purely visual delay costs nothing.
/// - **57**: the task board's patch ALSO carries which row of the processes
///   panel is selected. Travels with the board for the same reason
///   `total_rows` travels with a listing's rows: it is the extension of
///   whatever sits next to it and both move together. A task that expires
///   after ten seconds removes a row and shifts the rest, and before that
///   cursor only traveled in the whole snapshot — meaning the panel
///   highlighted row N, which was already another task or none, while the
///   cancel key acted on the one the host has pinned. Highlighting one and
///   stopping another is the bug.
/// - **58**: a dialog with a trimmed list says whether any of the ones it
///   does NOT show would paint altered. A visible path's badge says "what
///   you read is not the bytes that are there"; about what is trimmed that
///   cannot be said —it is not in front of you— but it can say there is
///   something like that out there, which is what decides whether it is
///   worth expanding before approving. The terminal has always said so in
///   its summary and this window did not, about the same paths.
/// - **59**: the viewer says how much there is SIDEWAYS (`total_cols`) and
///   where it is (`first_col`). The viewer does not wrap, so without this
///   a minified HTML painted clipped and the window had nothing to draw a
///   horizontal bar with: a file cut off on the right read as a short
///   file. `lines` already arrive clamped —the clamping is done once by
///   the shared model— and these two fields are the other half: what is
///   visible and how much there is. `viewer_scroll` arrives with them,
///   which is the WHEEL over the viewer: a wheel is not a key, and
///   manufacturing arrows to express it left the gesture dependent on
///   nobody having rebound those arrows.
/// - **60**: settings (F11) are WRITTEN from the window. `SettingsView`
///   loses `read_only`, which was a phase 4 promise and is no longer true;
///   and `settings_activate` arrives, the double click on a row, which
///   does what `enter` does: cycling whatever cycles and asking in a
///   dialog for whatever is typed. The editor is the one shared with the
///   terminal (`norte_frontend::settings`), and what is written is
///   re-read and applied through the same path as a profile change.
/// - **61**: the extension manager (F12) is GOVERNED with the mouse.
///   `extension_govern` arrives —approve or revoke, turn on or off, and
///   uninstall (ADR 0104), the pointed-to row and the same path as the
///   keyboard verb, questions included— and `extension_help`, an
///   extension's help page. No DTO changes: what the window paints with
///   buttons already traveled.
/// - **62**: the icon column (ADR 0105). `RowView` gains `icon` and
///   `icon_hostile`: what a slot's `icon` decorator set, to the LEFT of
///   the name; the badge still sits on the right, and both coexist.
///   `BrowserSlotView` and the `rows` patch gain `icon_column`: whether the
///   column is open is decided by the host from the whole listing, not by
///   the renderer from the rows it sees, or scrolling to a page with no
///   icons would close it and shift every name.
/// - **63**: the usability wave (spec 2026-09-10), in ONE jump.
///   `PaletteRowView.recent`: the row goes to the top for being one of the
///   last launched, only with an empty query. `PanelBarView.names`:
///   whether the buttons show their name (`[ui] panel_bar_style`) or just
///   the letter. `BrowserSlotView.footer` and `BrowserHeader.footer`: the
///   listing's footer (counts, marked, free space), already composed;
///   empty with `[ui] pane_footer` off. `ViewSnapshot.key_bar` and the
///   `key_bar` change: the function-key bar, derived from the keymap of
///   the screen holding the keyboard; `key_bar_activate` presses it and
///   the host synthesizes the key. `StatusView.notices_unread`: notices
///   that expired (`[ui] notice_seconds`) without anyone opening the log;
///   the badge opens the log through its pane-bar button.
///   `ViewSnapshot.wizard`, the `wizard` change and the `wizard_open` and
///   `wizard_activate_row` actions: the first-run wizard, which the
///   renderer requests when the catalogue says `first_run` and the host
///   writes through the settings path.
/// - **64**: column widths (spec 2026-09-11, V2). `ColumnHeader` gains
///   `width` —the FIXED width in cells `[ui.columns] spec.width`
///   configures, `None` for `auto`/`flex`— and `align`, the configured
///   alignment, the same one the terminal applies. `resize_column`
///   arrives: dragging a header's edge fixes that column's width in
///   memory and in `norte.toml`, and every slot's header comes back,
///   because the width belongs to the column and not the slot.
/// - **65**: breadcrumbs and a space indicator (spec 2026-09-11, V5).
///   `BrowserSlotView` and `BrowserHeader` gain `path_segments` —the root
///   and one segment per directory, each masked— and `used_ratio`, how
///   much of the volume is used. `breadcrumb_activate { slot_id, depth,
///   generation }` arrives: navigates to the directory with the path's
///   first `depth` segments, by depth and not by name, because a masked
///   segment is not a name again; with the generation of the listing that
///   painted the breadcrumbs, so a stale breadcrumb is not reinterpreted
///   over a different path. The `name` column's `ColumnHeader.width`
///   becomes its floor.
/// - **66**: the theme colors ENTRIES (spec 2026-09-11). `RowView` gains
///   `name_color` —the `#rrggbb` `[files.ext]` (wins) or `[files.kind]`
///   gives the name, empty if the theme says nothing— and `name_bold`,
///   `name_dim`, `name_italic`, `name_underline`. They travel RESOLVED and
///   not as a rule name because extensions are an OPEN set: a theme
///   colors whichever it likes, so the renderer cannot have classes for
///   them, unlike `badge_role`. Closes a divergence from the terminal that
///   had existed since the window did: `[files.kind]` and `[files.ext]`
///   —half of what a theme file declares— went unpainted, and a
///   monochrome listing does not read as a poor theme, it reads as a
///   broken one.
///
///   Of `norte_theme::Style`'s six attributes, FOUR cross. `bg` and
///   `reverse` stay out on purpose: a row's background is already
///   contested by the cursor, the hover and the mark, and a fifth owner
///   would let the theme hide where the cursor is. A theme that paints
///   `bg` in `[files.ext]` will see it in the terminal and not here, and
///   that is also written in `docs/theming.md`.
/// - **67**: the desktop's scheme crosses. `set_color_scheme { dark }`
///   arrives, sent by the renderer on startup and on every
///   `prefers-color-scheme` change.
///
///   Needed as of bridge 66 and not before: the variant's CSS VARIABLES
///   (V6) are plugged in by the renderer on its own and synchronously, to
///   avoid a flash of the wrong palette, so until now the host did not
///   need to know the scheme. With the entry's color baked into the row it
///   does: with `theme_dark = "vscode-dark"` and `theme_light =
///   "vscode-light"`, switching the desktop to light repainted the chrome
///   with the light variant and left the NAMES with the dark one's colors
///   — `dir` in #4daafc on white, 2.6:1, below the floor those presets
///   promise in their own header.
///
///   The rule "that side's variant if there is one, and `theme` if not" is
///   written on both sides (`themeFor` in `ui/src/main.ts`,
///   `HostTheme::for_scheme` in Rust) because each needs a different
///   thing —variables for the renderer, the whole `Theme` for the host,
///   the only one that can resolve `[files.ext]`. What keeps them from
///   drifting apart is `the_variant_rule_is_the_renderers`, which
///   pins the three cases.
/// - **68**: `menu_toggle` arrives, Alt pressed and released alone. It
///   collapses the open menu or opens it like `app.menu`, and does nothing
///   with a screen holding the keys in front. Crosses as its own action
///   because a lone modifier is not a keymap chord.
/// - **69**: the startup screen and how long a task takes (spec 2026-09-15,
///   ADR 0115). Three new shapes, all additive: `ViewSnapshot.splash`
///   (with its `ViewChange::Splash`), `TaskView.rate` and `TaskView.eta`,
///   and `RowView.progress`.
///
///   The rate does NOT come from the wire: `TaskProgress` says how much is
///   done and not at what speed, so each frontend estimates it from its
///   own snapshots (`norte_frontend::tasks::Rate`). It crosses already
///   formatted —`"12.3 MiB/s"`, `"1m 04s"`— and not as numbers because the
///   renderer has neither the locale nor the units, and two frontends
///   rounding on their own diverge in the last digit without anything
///   catching it.
///
///   `RowView.progress` is the bar INSIDE the listing's row: the task says
///   which file it is currently on, and that row is the one the reader is
///   looking at. It is `Option` because a row with no task behind it has
///   no bar, which is almost all of them.
/// - **70**: the panel a PLUGIN paints (phase 3, proto 0.74.0).
///   `SlotView::Panel` arrives with `PanelSlotView`: the `title` —the
///   `<kind>` the plugin declared, with no prefix—, the `lines` its guest
///   described (`SpanView` spans, the same as a styled preview) and the
///   `hits`, the clickable zones. A `HitView` carries `row`/`col`/`width`
///   and **does not carry its command**: the renderer sends the CELL with
///   the `panel_click { slot_id, row, col }` action and the host resolves
///   against the frame it holds which zone it was and which command
///   applies, filtered by `norte_frontend::frame::zone_can`. A command
///   traveling over the wire would be a command anyone talking to the
///   renderer could send, and the plugin chooses the label AND the
///   command with nothing tying them together. Empty `lines` is the panel
///   that does not have a frame yet —the first request in flight, or a
///   plugin that failed—: its border is painted with its title, never a
///   silent gap.
/// - **71**: the disk map (phase 4, proto 0.75.0). `SlotView::DiskMap`
///   arrives with `DiskMapSlotView`: the treemap's already-laid-out
///   `lines` and its `hits`, in the same shape as a plugin panel and for
///   the same reason — the layout is done by the host with
///   `norte_frontend::treemap::squarify`, because a treemap computed twice
///   is two different treemaps the moment someone touches a rounding.
///
///   A `HitView` still carries no destination: the renderer sends the CELL
///   (`panel_click`) and the host resolves against ITS frame which child
///   was hit. Here that matters more than in a plugin panel, because what
///   is resolved is a file's NAME: if it crossed the wire, it would be a
///   name anyone talking to the renderer could send, and it would also
///   have to travel masked —which is what is painted— and come back
///   reversible, which are two different shapes of the same string.
///
///   `measuring` says whether the measurement is still in progress: a
///   half-done map that does not say so reads as a small directory.
/// - **72**: the ORGANIZE review (phase 8, proto 0.77.0). `OrganizeView`
///   arrives, twin to `AiRenameView` with two differences that come from
///   the same thing — here what changes is the directory's SHAPE:
///
///   The body is a TREE (`OrganizeLineView`: `depth`, the sanitized name
///   with its mark, and a three-value `kind`) and not a list of pairs.
///   Forty rows of `a.pdf → invoices/2026/a.pdf` do not let you see how
///   many folders appear or what ends up inside each one, which is exactly
///   what is being approved. `kind` travels as DATA and not resolved to a
///   color: a monochrome theme needs to be able to mark a folder that is
///   about to be created some other way.
///
///   And there is no verdict to wait for. The plan's token travels WITH
///   the plan (`AiOrganizePlanResult::plan_hash`), so this screen is born
///   approvable instead of opening in `Pending` — what is given up in
///   exchange is the `status` field, which here would say nothing.
///
///   `organize_scroll { down }` exists in addition to the keys because
///   approving requires having reached the end: without a gesture to walk
///   it, the screen was one a mouse-only reader could never approve.
/// - **73**: `handoff_failed { no_terminal }` (phase 9, ADR 0123
///   amendment). Sent by the native-effects thread when a handoff's
///   terminal did NOT open, so the window stays, recovers the session and
///   says so. The window used to launch the emulator and forget about it:
///   it neither closed on success nor learned of failure, and stayed
///   saying "handing off the screen…" with the session already released.
///
///   No free text: a bool and not a reason, because anyone talking to the
///   host can send the action, and a reason written by whoever sends it
///   would be a message the host would paint without having written it.
///   And it only does something with a handoff IN PROGRESS; outside of
///   one it is a stale action.
/// - 74: `MenuItemView` gains `section` and `role` (ADR 0125): menus go in
///   sections, with or without a label, and an entry says whether it
///   deletes or whether an AI does it. Empty `chord` instead of `—` when
///   there is no shortcut.
/// - 75: `HelpSpanView::Link` gains `action`, the index of the
///   `HelpView::actions` row it follows: a prose `[[link]]` can be
///   clicked. An index and not the topic's id, which is a key the
///   renderer does not need.
/// - 76: `HelpView::scroll`, the request to scroll help's body. The keys
///   that scroll go through the reader's keymap in the host; the renderer
///   used to handle them as fixed keys and a rebind never reached them.
/// - 77: "go anywhere" (#357): `ViewSnapshot::goto` and `ViewChange::Goto`
///   with `GotoView` —query, lines (section header or row, each row with
///   its `hostile`), cursor and the empty-state text. The connections and
///   index sections arrive in a separate patch when they answer, without
///   moving the cursor.
/// - 78: the journal timeline (#359): `SlotView::Timeline` with
///   `TimelineSlotView` —already-paintable rows (time, actor for the
///   color, verb, path with its `hostile`, translated tail), cursor,
///   empty-state text and the footer with what an `Enter` would do. A
///   batch is ONE row.
/// - 79: an extension that failed to load is a row of the manager (ADR
///   0113; written like #69 on a branch that reached `main` after 78).
///   `ExtensionErrorView` carries `id` —its directory's, if it is named
///   like one— and `ExtensionsView`'s cursor keeps following `rows` with
///   `errors`. `extension_select_row` and `extension_govern` name those
///   rows by the same count, and on a broken one the only change handled
///   is `uninstall`. ADR 0104 had left it written as a gap: the handler
///   deleted it and the window had no way to ask for it.
/// - 80: the listing's "stripes" (spec 2026-09-20): `View::row_stripes`
///   says whether odd rows sit on a band. Crosses as a boolean and not a
///   color because the color already crosses: it is the theme's `stripe`
///   role, which travels with the rest in `--stripe-bg`. An old renderer
///   ignores it and paints the listing as always, which is exactly the
///   default.
///
///   And the image viewer's ZOOM: `ViewerView::image_zoom`, the percentage
///   of what the image would occupy FITTED. Percentage and not pixels
///   because the one that knows how much "fitted" is is the renderer,
///   which has the slot; the host keeps count of the steps. With
///   `serde(default)` at 100, which is fitted: an earlier host does not
///   send it and the renderer paints what it painted.
/// - 81: settings, by section. `SettingsView` gains `index` —every section
///   this surface has, with how many rows of each are visible—, `query`,
///   `shown` and `total`; `SettingRowView` gains `modified`, the point of
///   "this is not the factory value", computed against the default value
///   and not against "there is a key in your file". `sections` stops
///   being a list of two —General and the paths— and carries one per
///   section that has rows to show.
///
///   The index travels SEPARATELY from `sections` because a section the
///   filter emptied stays in the index, dimmed, with nothing to paint
///   underneath: an index that changes length while you type is no good
///   as a map.
///
///   And three actions: `settings_query` (the search box's WHOLE text, not
///   the key — printable ones never reach the host, which is why this
///   screen had no filter until now), `settings_jump_section` (by the
///   section's STABLE key, so the jump does not depend on the language)
///   and `settings_reset` (removing the key from the write layer).
/// - 82: `SettingsView.focus` —`"index"` or `"list"`—, which half of that
///   screen has the keyboard. `tab` changes it, as in help, and BOTH
///   cursors are always painted: the one without it, dimmed (ADR 0128).
///   Without this field the renderer can only paint one, which is exactly
///   what makes it impossible to know where focus is. An earlier renderer
///   ignores it and paints what it painted — the index by mouse, the list
///   with its cursor.
/// - 83: settings CONTROLS. `SettingRowView` gains `control` —`toggle`,
///   `choice`, `number`, `text`, `args`—, `choices` with the accepted
///   values and a number's `min`/`max`. Without this the renderer cannot
///   paint a toggle: only the value arrived as text, and a setting's class
///   is not guessed by looking at the word `true`.
///
///   LIVE lists —installed themes, presets— arrive already resolved
///   inside `choices`, so the renderer does not tell a catalogue list
///   apart from one that changes on the fly.
///
///   And the `settings_set { id, value }` action: SETS a value instead of
///   cycling it. `settings_activate` is still what Enter does, but with a
///   ten-theme dropdown choosing the seventh would be seven trips and six
///   writes to `norte.toml`. Goes by ID and not by row because a control
///   takes as long as the reader takes to release it, and the filter
///   behind it may have changed which rows there are.
///
///   And `default`, each row's FACTORY value: what an empty field shows as
///   a placeholder. "Empty" is not a gap, it is that value, and saying
///   which one informs — a sentence saying there is one takes the data's
///   place without giving it.
/// - **84**: the window drops the function-key bar (spec 2026-09-21).
///   `ViewSnapshot.key_bar`, the `key_bar` change and the
///   `key_bar_activate` action are gone, with `KeyBarView` and
///   `KeyCellView`. The F1–F10 bar is inherited from the terminal; the
///   window has a menu, a palette and an activity bar, and the cell row
///   was the heaviest thing on screen and the one that said the least.
///   `[ui] key_bar` still governs the TUI. And the pane bar can be the
///   activity bar (ADR 0131): `PanelBarView.vertical` (`[ui]
///   panel_bar_position` already resolved) and `PanelButtonView.count`,
///   its badge's number.
/// - **85**: the status bar by items (ADR 0132). `ViewSnapshot.status_items`
///   and the `status_items` change: the right half, already composed,
///   trimmed by priority and in `[ui] status_items`'s order. The
///   `status_item_activate { id }` action presses one, by ID because the
///   list moves with the cursor.
/// - **86**: layout and tab buttons (ADR 0133). `ViewSnapshot.layout_buttons`
///   (`ChromeButtonView`: id, name, shortcut) and the
///   `layout_button_activate { id }` action; the `tab_action { slot_id,
///   verb: "new" | "close" }` action, which chooses the tab and runs the
///   command, like the TUI's `[+]`/`[x]`.
/// - **87**: places-bar drives (2026-09-21 capture). `PlaceRowView::Drive.label`
///   becomes the SHORT name (`places::drive_name`, the TUI's) and gains
///   `mount` —the whole mount point, for the title—, `free` —the right
///   column's short free-space— and `kind`, which chooses the icon.
/// - **88**: panels on the same edge group into tabs (ADR 0134).
///   `TabGroupView.panels`: the group is of panels, not listings, and
///   carries no `+`. A panel tab's label is its name from the pane bar,
///   not the kind's id.
/// - **89**: the mark ruler (ADR 0135). `BrowserSlotView.mark_ruler` and
///   `ViewChange::BrowserHeader.mark_ruler`: which segments of the
///   listing, out of `MARK_RULER_SPANS`, carry any mark.
/// - **90**: moving a slot by dragging it (ADR 0138). The `move_slot {
///   slot_id, target, zone }` action, with `zone` one of `left`, `right`,
///   `top`, `bottom` or `center`.
/// - **91**: a dialog can be a FORM. `DialogView.fields`, a generic list of
///   `DialogFieldView` (text, toggle or cycle), and the `dialog_field {
///   id, field, value }` action to touch them. Absent and empty = the
///   usual dialog, so a formless one's JSON is byte for byte the same as
///   90's. The first to use it is the window's search, which used to ask
///   for a glob and nothing else while the terminal offered seven fields
///   (protocol 0.81.0).
/// - **92**: the lightweight progress bar (ADR 0146). `StatusItemView.progress`
///   (`StatusProgressView`: `percent` and `phase`) in the `tasks` item: the
///   renderer paints the bar BEHIND the text, `BAR_CELLS` cells wide.
///   Absent = no bar, so any other item's JSON is byte for byte 91's.
/// - **93**: a task can be paused (ADR 0147). `TaskStateView` gains
///   `paused`: alive and stopped. The host used to paint it as `running`,
///   which is exactly what it is not doing.
/// - **94**: a slot's thin progress line (ADR 0148). `BrowserSlotView.progress`
///   and the `slot_progress { slot_id, progress }` change: what is
///   arriving at THAT directory, to paint two pixels on its border
///   without resending the listing.
/// - **95**: the terminal panel (#362). `SlotView::Terminal`
///   (`TerminalSlotView`: `TerminalSpanView` rows, cursor and
///   `no_shell`). What crosses is ALREADY-PAINTED ROWS and not the pty's
///   bytes: the emulation is done by `norte-term` on the host side, the
///   same one the terminal uses, so both frontends show the same thing by
///   construction. Its colors travel UNRESOLVED —`indexed` stays an
///   index— because which blue "color 4" is is decided by the palette of
///   whoever paints, not the host.
pub const BRIDGE_VERSION: u32 = 95;

/// Cap on a string that crosses to the renderer, in bytes.
///
/// Everything paintable is clamped in Rust and not in the renderer: a 700
/// KB hostile name cannot become whoever paints it problem.
pub const MAX_STRING_BYTES: usize = 4096;

/// Rows ONE message can carry.
pub const MAX_ROWS_PER_BATCH: usize = 2048;

/// Live notices at once; the oldest fall off.
pub const MAX_NOTICES: usize = 32;

/// Tasks projected at once.
pub const MAX_TASKS: usize = 256;

/// Tasks RETAINED at once, projected or not (#271).
///
/// [`MAX_TASKS`] caps what crosses the bridge; this caps what the host
/// keeps. They are not the same number because they are not the same
/// question: a row that falls out of the projection still has progress to
/// pump and a directory to re-list when it finishes, and dropping it for
/// not fitting on screen would lose the refresh.
///
/// `registrar_task`'s eviction can only drop TERMINAL tasks, so without
/// this second cap a batch of three thousand queued copies —none terminal
/// yet— retained all three thousand. 512 is what a daemon accepts alive at
/// once (`MAX_LIVE_TASKS`), that is, the real ceiling on the other side.
pub const MAX_TASKS_RETAINED: usize = 512;

/// Entries ONE transfer accepts (#271).
///
/// `pane.copy` operates on the marks, and marking has no cap: a batch used
/// to be queued whole and the limit was discovered when the daemon started
/// rejecting on `MAX_LIVE_TASKS`, that is, halfway through, with half done
/// and nothing saying where it was cut off. Saying it BEFOREHAND is more
/// honest than discovering it halfway.
pub const MAX_TRANSFER_BATCH: usize = 512;

/// Dialogs stacked at once.
///
/// The stack used to be of human gestures and that is why it had no
/// ceiling. Since task 5.3 the WIRE feeds it: one approval per agent op,
/// and one report per batch or terminal undo that left something half
/// done —including another client's, on the same session. Another
/// frontend running two hundred stuck batches stacked two hundred
/// dialogs, each asking two questions, and every dialog patch CLONES the
/// whole stack.
///
/// Eight is what a person can answer without losing track; on hitting the
/// ceiling the oldest UNACKNOWLEDGED one falls —whichever nobody has
/// gotten to look at yet— and never the top one, which is the one being
/// answered.
pub const MAX_DIALOGS: usize = 8;

/// Bytes of a preview that cross to the renderer.
pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;

/// Identity of ONE host instance.
///
/// Orders everything else: a `sequence` only means something within the
/// instance that emitted it, and an action arriving with a different
/// instance is from an earlier life of the host (a reattach after a
/// restart) and mutates nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    /// Builds the identity. Manufactured by the host on startup.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The opaque string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A ROW's key, opaque to the renderer.
///
/// Only valid within `(instance, slot, generation)`. When the host
/// re-lists, the generation goes up: a click arriving with the previous
/// one is answered [`StaleAction::Generation`] and does nothing. This is
/// what stops a late double click from acting on the file that took that
/// row's place AFTERWARD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RowKey(pub u64);

/// The identity of an open dialog.
///
/// Confirming is idempotent because of this: a second `Confirm` with the
/// same id does not re-launch the operation, and one with an old id does
/// not close the dialog that is open NOW.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModalId(pub u64);

/// The token for a request in flight.
///
/// Does NOT cross to the renderer: it is the host's internal bookkeeping
/// to discard, in Rust, the response to something no longer of interest.
/// Lives in this module out of historical proximity, and stays because
/// moving it would be a renaming with no reader; that it is not on the
/// wire is stated by the corpus, where it does not appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestToken(pub u64);

/// The envelope of every message from the host to the renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeEnvelope<T> {
    /// Contract version ([`BRIDGE_VERSION`]).
    pub bridge_version: u32,
    /// Who emits it.
    pub instance_id: InstanceId,
    /// Order within this instance. Starts at 0 and never skips.
    pub sequence: u64,
    /// What is sent.
    pub payload: T,
}

impl<T> BridgeEnvelope<T> {
    /// Puts `payload` into an envelope of THIS version.
    pub fn new(instance_id: InstanceId, sequence: u64, payload: T) -> Self {
        Self {
            bridge_version: BRIDGE_VERSION,
            instance_id,
            sequence,
            payload,
        }
    }

    /// Can this renderer interpret the envelope?
    ///
    /// An all-or-nothing question on purpose: half an interpretation of a
    /// contract it does not know is worse than a screen that says so.
    ///
    /// ```
    /// use norte_ui_host::{BridgeEnvelope, InstanceId, BRIDGE_VERSION};
    ///
    /// let mut e = BridgeEnvelope::new(InstanceId::new("host-1"), 0, 7u32);
    /// assert!(e.is_supported());
    /// e.bridge_version = BRIDGE_VERSION + 1;
    /// assert!(!e.is_supported(), "a future version is NOT interpreted");
    /// ```
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.bridge_version == BRIDGE_VERSION
    }
}

/// Why an action did nothing, without it being an error.
///
/// All three are normal races between a renderer that paints and a host
/// that has already changed state, and none is anyone's fault: they are
/// answered and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleAction {
    /// The action came from ANOTHER host instance.
    ///
    /// RESERVED: unreachable today, because actions travel without an
    /// envelope and therefore without an instance. A renderer that
    /// survives a host restart is the situation that justifies it, and
    /// then the inbound direction will need wrapping too. Declared so a
    /// renderer that receives it someday already knows what it means.
    Instance,
    /// The row (or the slot) is from an earlier generation: there was a
    /// re-list.
    Generation,
    /// The dialog it answers is no longer open.
    Modal,
}

/// The answer to an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ActionAck {
    /// Applied. `sequence` is the first update that reflects it.
    Applied {
        /// The update in which it will be seen.
        sequence: u64,
    },
    /// Not applied, and nothing is wrong: see [`StaleAction`].
    Stale {
        /// Which of the three races it was.
        reason: StaleAction,
    },
    /// The action is not available RIGHT NOW (dimmed command, no
    /// permission, no connection). Carries the reason's Fluent key, not
    /// the sentence: the renderer does the translating with the host's
    /// catalogue.
    Unavailable {
        /// Fluent key for the reason.
        reason_key: String,
    },
}

/// Clamps a string to the bridge's cap without splitting a cluster.
///
/// It is STATED (`…`), the same rule the rest of the project applies to
/// anything paintable: nothing is ever lost silently.
///
/// ```
/// use norte_ui_host::bridge::{clamp_display, MAX_STRING_BYTES};
///
/// // What fits travels intact.
/// assert_eq!(clamp_display("café.txt".to_owned()), "café.txt");
///
/// // What does not is clamped AND marked.
/// let long = clamp_display("a".repeat(MAX_STRING_BYTES * 2));
/// assert!(long.len() <= MAX_STRING_BYTES);
/// assert!(long.ends_with('…'));
/// ```
#[must_use]
pub fn clamp_display(s: String) -> String {
    if s.len() <= MAX_STRING_BYTES {
        return s;
    }
    // The clamp is the SHARED one. A character boundary is not enough: it
    // can cut inside a cluster and leave an orphaned combining mark that
    // composes with the `…`. That rule was already solved and tested
    // against the corpus in `norte-frontend`; having a second one here
    // would be having two.
    norte_frontend::display::ellipsis_at_bytes(&s, MAX_STRING_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_envelope_from_another_version_is_not_interpreted() {
        let mut e = BridgeEnvelope::new(InstanceId::new("i"), 0, 7u32);
        assert!(e.is_supported());
        e.bridge_version = BRIDGE_VERSION + 1;
        assert!(!e.is_supported(), "a future version is NOT interpreted");
    }

    #[test]
    fn a_long_string_is_clamped_and_stated() {
        let long = "a".repeat(MAX_STRING_BYTES * 2);
        let out = clamp_display(long);
        assert!(out.len() <= MAX_STRING_BYTES);
        assert!(out.ends_with('…'), "the clamp is visible");
    }

    /// The clamp never splits a multibyte character in half.
    #[test]
    fn the_clamp_respects_characters() {
        let long = "é".repeat(MAX_STRING_BYTES);
        let out = clamp_display(long);
        assert!(out.len() <= MAX_STRING_BYTES);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    /// A string that already fits is untouched (and gets no marker added).
    #[test]
    fn what_fits_travels_intact() {
        let s = String::from("café.txt");
        assert_eq!(clamp_display(s.clone()), s);
    }
}
