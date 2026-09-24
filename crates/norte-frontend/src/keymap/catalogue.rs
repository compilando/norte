//! The command vocabulary, shared. Before this table each frontend owned a
//! private `COMMANDS` list and passed it to the engine as `known_commands`, so
//! the same preset resolved differently in the TUI and the GUI, silently —
//! that is how F1 did nothing in the GUI for several releases (see the H3f
//! comment in `norte-gui/src/keymap.rs`).
//!
//! A frontend still declares WHICH of these it implements. What it no longer
//! does is decide which names EXIST.

/// One command in the shared vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDef {
    /// Stable name, e.g. `pane.copy` — what a preset binds, the palette shows
    /// and `help_id` mangles into a Fluent id.
    pub name: &'static str,
    /// Whether a numeric count prefix means anything here (K2 consumes it:
    /// `5j` moves five, `5` before `app.quit` is nonsense). Declared with the
    /// command because that is where the answer is known.
    pub counts: bool,
    /// Why a binding to this name may resolve to nothing.
    pub status: Status,
    /// What running it does to the reader's world (ADR 0126). Declared on
    /// every row, with no default, because a mutating command classed as
    /// harmless by omission is one a read-only window would run.
    pub effect: Effect,
}

/// What a command does to the reader's files and data — the ONE effect a
/// reader must be warned about first (ADR 0126).
///
/// A read-only window runs only [`Effect::Inert`] commands, and the menu
/// paints [`Effect::Destroys`] and [`Effect::SendsOut`] apart. *Where* a
/// command writes (source, destination) is not this: that depends on runtime
/// facts and is [`crate::availability::verdict`]'s question.
///
/// ```
/// use norte_frontend::keymap::catalogue::{Effect, effect};
///
/// assert_eq!(effect("pane.copy"), Some(Effect::Writes));
/// assert_eq!(effect("pane.delete"), Some(Effect::Destroys));
/// // Renames too, but the listing has left the machine first.
/// assert_eq!(effect("pane.ai-rename"), Some(Effect::SendsOut));
/// assert!(effect("cursor.down").is_some_and(Effect::is_inert));
/// assert_eq!(effect("pane.no-exists-jamas"), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Touches only norte's own state: the view, marks, layout, tabs, and
    /// norte's own configuration (a saved profile, the settings). Showing a
    /// file in the viewer is inert too — looking is what a read-only window
    /// is for, and so is testing an archive, which answers "is it sound?"
    /// and nothing more.
    ///
    /// A command that only OPENS a screen is classified by the opening.
    /// Whatever writes from inside that screen (`app.agents` can undo a
    /// session, a dialog can approve) is guarded where it happens, not here.
    Inert,
    /// Reads whole files and produces a result that leaves the screen: a
    /// fingerprint of the reader's files (checksums), which can be copied,
    /// compared and published. That, not the reading, is what sets it apart
    /// from testing an archive.
    ReadsContent,
    /// Hands control to a program norte does not govern: an editor, the
    /// desktop's opener, a shell, another frontend. What it then does to the
    /// files is not norte's to say.
    Launches,
    /// Creates or changes the reader's files, through the journal.
    Writes,
    /// Deletes the reader's files, to the trash or for good.
    Destroys,
    /// Sends the reader's data out of the process, to an AI provider.
    SendsOut,
}

impl Effect {
    /// Whether a window that promised to only look may run it.
    #[must_use]
    pub fn is_inert(self) -> bool {
        self == Self::Inert
    }
}

/// Whether norte has built this command at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// At least one frontend implements it. WHICH ones is not this table's
    /// business — each frontend declares its own set.
    Live,
    /// A preset may legitimately bind it; norte has not built it yet. Carries
    /// the reason a user is owed and the issue that tracks it.
    Planned {
        /// Fluent id of the short, user-facing reason — NOT the prose itself.
        /// The catalogue must not carry a locale; the frontend translates it
        /// when it prints the message.
        reason: &'static str,
        /// The GitHub issue. Never zero, never invented — pinned by test.
        issue: u32,
    },
}

const fn live(name: &'static str, counts: bool, effect: Effect) -> CommandDef {
    CommandDef {
        name,
        counts,
        status: Status::Live,
        effect,
    }
}

/// A command a preset can honestly name and that norte does not do yet.
///
/// **There is none right now**, and the constructor stays for what it cost
/// to discover: with #132 the table ran out of `Planned` entries, and then
/// phase 6's parity matrix uncovered three (`task.next`/`prev`/`dismiss`)
/// declared LIVE with no frontend implementing them — which is worse,
/// because a key like that does nothing and does not say why either. This
/// machinery (dimmed row, translated reason, issue number) is the answer
/// to that, and rebuilding it would cost more than keeping it.
#[allow(dead_code, reason = "the vocabulary is complete: see the doc above")]
const fn planned(
    name: &'static str,
    reason: &'static str,
    issue: u32,
    effect: Effect,
) -> CommandDef {
    CommandDef {
        name,
        counts: false,
        status: Status::Planned { reason, issue },
        effect,
    }
}

use Effect::{Destroys, Inert, Launches, ReadsContent, SendsOut, Writes};

/// Every command either frontend knows, plus the ones a preset may honestly
/// bind before norte builds them.
pub const CATALOGUE: &[CommandDef] = &[
    // --- app ---
    live("app.quit", false, Inert),
    live("app.help", false, Inert),
    live("app.theme", false, Inert),
    live("app.settings", false, Inert),
    live("app.extensions", false, Inert),
    // The AGENT sessions this client has seen ask for permission, and
    // undoing a whole one (#276). Lives in the shared catalogue — and not
    // only in the graphical host — because the command vocabulary is ONE:
    // a preset can bind it, help documents it, and the frontend that does
    // not implement it yet says so with the same sentence as any other
    // that lacks it.
    live("app.agents", false, Inert),
    live("app.palette", false, Inert),
    // "Go anywhere" (phase 6 of the WOW program): a screen over what was
    // already split across five — history, popular, favorites, connections
    // and the palette — plus a typed path and whatever the index finds. It
    // does not replace any of them: each still has its own key, and this
    // is the one that helps when you do not know which of the five it is
    // in.
    live("app.goto", false, Inert),
    live("app.menu", false, Inert),
    // `--pick` (S2): being in this table only means the NAME is known to the
    // vocabulary (help, palette, rebind checks). No preset binds it — the
    // TUI's run loop decides, per keystroke, whether Enter/Ctrl+Enter means
    // this or `nav.enter`, because that decision needs runtime state
    // (`--pick` was passed, the cursor is on a directory) that a keymap file
    // cannot express. A preset that bound it directly would fire outside
    // `--pick` too, which is exactly what keeping it out of every preset
    // prevents.
    live("app.pick-accept", false, Inert),
    // #135 (S4): the TUI builds these by SUSPENDING itself — it hands the
    // whole terminal over and takes it back. `app.terminal` is also live in
    // the GUI, which cannot suspend and launches the desktop's terminal
    // emulator instead; the other two are TUI-only and resolve to
    // `Availability::NotHere` there, which is the truth and is what the
    // reference sheet greys out.
    live("app.terminal", false, Launches),
    live("app.toggle-panels", false, Launches),
    // Phase 9: the HANDOFF between frontends. Lives here and not in a
    // preset because what decides whether it can be done is runtime state
    // — is there a daemon, is there somewhere to open a window — that a
    // keymap file cannot express; availability says so with its reason.
    live("app.handoff", false, Launches),
    // --- pane ---
    live("pane.command-line", false, Launches),
    live("pane.switch", false, Inert),
    // --- tabs (L1b, closes #137) ---
    //
    // The first four were RESERVED here as `planned` since K2, and several
    // presets already bound them: `total-commander` and `krusader` had them
    // written and greyed out. Building them under another name would have
    // left two vocabularies for the same thing and those keys dead forever.
    live("pane.tab-new", false, Inert),
    live("pane.tab-close", false, Inert),
    live("pane.tab-next", false, Inert),
    live("pane.tab-prev", false, Inert),
    live("pane.tab-move-left", false, Inert),
    live("pane.tab-move-right", false, Inert),
    live("pane.tab-goto-1", false, Inert),
    live("pane.tab-goto-2", false, Inert),
    live("pane.tab-goto-3", false, Inert),
    live("pane.tab-goto-4", false, Inert),
    live("pane.tab-goto-5", false, Inert),
    live("pane.tab-goto-6", false, Inert),
    live("pane.tab-goto-7", false, Inert),
    live("pane.tab-goto-8", false, Inert),
    live("pane.tab-goto-9", false, Inert),
    // --- layout (L1b) ---
    live("layout.split-h", false, Inert),
    live("layout.split-v", false, Inert),
    live("layout.focus-next", false, Inert),
    live("layout.focus-prev", false, Inert),
    live("layout.close-slot", false, Inert),
    live("layout.grow", true, Inert),
    live("layout.shrink", true, Inert),
    live("layout.equalize", false, Inert),
    // ADR 0138: flip the focused pane's split. Deliberately no key in any
    // preset: it lives in the layout button, the menu and the palette, and
    // every preset says so in its header.
    live("layout.flip", false, Inert),
    live("layout.set-target", false, Inert),
    // L3: the places sidebar. In the `layout.*` family and not `pane.*`
    // because what it does is REORGANIZE the screen — it docks a new pane
    // — not operate on a listing.
    live("layout.places", false, Inert),
    // L3: the docked viewer. Same reason to be in `layout.*`: it docks a
    // pane. What is INSIDE it is the usual `viewer` kind.
    live("layout.preview", false, Inert),
    // Phase A: the processes panel and the attribute sheet. No chord in
    // any preset: fifteen `layout.*` across seven presets is #228, and
    // binding two here would leave the family half-done with no rule
    // saying which half. They are reached from the palette and the menu.
    live("layout.processes", false, Inert),
    live("layout.metadata", false, Inert),
    // The log (#323). This one DOES carry a chord in all seven, unlike its
    // neighbors: what opens here is what explains why something just
    // failed, and looking for it in the palette right when something goes
    // wrong is asking the reader for one extra step at the worst moment.
    // `alt+l` was free in all seven presets.
    live("layout.log", false, Inert),
    // The disk map (phase 4). Carries a chord in all seven for the same
    // reason as the log: it is a panel that keeps the KEYBOARD — you walk
    // the rectangles and enter one — and a panel like that has to be
    // enterable and exitable without a mouse. The test
    // `keyboard_panels_open_and_are_browsed_in_all_seven_presets` requires
    // this by machine.
    //
    // `alt+z`, and the mnemonic is bad on purpose: it was the ONLY free
    // letter in all seven. From `a` to `y` none is left unbound in some
    // preset — `alt+d` is `pane.disconnect` in orthodox and krusader,
    // `alt+m` the menu, `alt+j` processes, `alt+l` the log — so it was
    // either this or a chord that already means something else in the
    // manager someone is imitating. An odd key is learned; one that does
    // two things is not.
    live("layout.disk-map", false, Inert),
    // The journal's timeline (phase 7): what has been done and how far
    // back you can go. NO key in any preset, and it is deliberate: the
    // `alt+<letter>` space for panels is exhausted — b, j, l, t, z are
    // already others — and none is free in all seven at once. Binding it
    // in three and in four is no worse than binding it in none: it is a
    // capability half the readers would not have and nobody would tell
    // them why. It is reached through the panel bar, which is generated
    // from the kind registry and therefore has it in ALL SEVEN, and
    // through the View menu.
    live("layout.timeline", false, Inert),
    // The terminal panel (#362). `Launches` and not `Inert`: what it opens
    // is a SHELL, with the environment and directory of whoever opened it,
    // and that is the first thing the reader must be warned about — the
    // same effect as `app.terminal` and `app.toggle-panels`, which do the
    // same thing in another form.
    //
    // Carries a chord in all seven, and here it is not debatable: it is
    // the panel that keeps the keyboard the MOST of all: the others
    // consume catalogue commands and this one consumes bytes, i.e. also
    // the chords that would be norte's. With no key you cannot enter and,
    // above all, cannot leave.
    //
    // `ctrl+alt+s`, from "shell". The obvious letter would be `t`, and it
    // is free in all seven, but `ctrl+alt+t` is HIJACKED by the desktop —
    // GNOME and KDE bind it out of the box to "open a terminal" — so norte
    // would never see it: it is the same class of failure as
    // `ctrl+<UPPERCASE>`, a binding that exists, that help prints and that
    // does nothing. `alt+<letter>` was not an option: that space ran out
    // with the disk map.
    //
    // And unlike its neighbors, the second tap does NOT close the panel:
    // it gives back the focus and leaves the shell alive, which is what
    // `app.toggle-panels` does with the subshell. Closing it kills a
    // process of the reader's, and that is what `layout.close-slot` is
    // for, and says so.
    live("layout.terminal", false, Launches),
    // The layout picker. No chord for the same #228, and also because a
    // layout's name is NOT a keymap preset's even when they coincide: the
    // dialog says so in its footer.
    live("layout.pick", false, Inert),
    // The profiles (ADR 0079). No chord for the same #228 — binding four
    // new keys in seven presets with nobody asking for it is the opposite
    // error #228 fixed — reached through the palette and the menu.
    //
    // The picker also warns that a profile name matching a layout's or a
    // keymap preset's is NOT that other thing: they are three different
    // settings that can share a name.
    live("profile.pick", false, Inert),
    live("profile.next", false, Inert),
    live("profile.prev", false, Inert),
    live("profile.save-as", false, Inert),
    live("pane.mirror", false, Inert),
    live("pane.mirror-target", false, Inert),
    // The PERMANENT mirror: while it is on, every navigation of the
    // focused pane is repeated by the other one. It is a switch, not a
    // gesture — that is why it is not called `pane.mirror-mode`: what gets
    // turned on is not a mirror, it is that the two panes walk together.
    // And nothing to do with `pane.sync-dirs`, which WRITES files.
    live("pane.sync-nav", false, Inert),
    live("pane.pull", false, Inert),
    live("pane.swap", false, Inert),
    live("pane.copy", false, Writes),
    live("pane.move", false, Writes),
    live("pane.delete", false, Destroys),
    live("pane.delete-permanent", false, Destroys),
    live("pane.mkdir", false, Writes),
    live("pane.rename", false, Writes),
    live("pane.rename-batch", false, Writes),
    live("pane.refresh", false, Inert),
    live("pane.view", false, Inert),
    live("pane.open", false, Launches),
    live("pane.quick-search", false, Inert),
    live("pane.history", false, Inert),
    live("pane.hotlist", false, Inert),
    // The whole history (spec 2026-09-15, phase 1). `pane.popular` is the
    // session's list sorted by visits (Krusader `Ctrl+Z`); `-left`/`-right`
    // name a SIDE, like the volumes. None carries a count: they are lists.
    live("pane.popular", false, Inert),
    live("pane.history-left", false, Inert),
    live("pane.history-right", false, Inert),
    // `pane.select-drive*` (2026-08-10-volumes.md, closes #131): the focused
    // pane and the two sides Total Commander's `Alt+F1`/`Alt+F2` name. None
    // is a clamped mover — a drive picker has no count to take.
    live("pane.select-drive", false, Inert),
    live("pane.select-drive-left", false, Inert),
    live("pane.select-drive-right", false, Inert),
    // `pane.compare-dirs` (2026-08-11-directory-comparison.md, roadmap item 1
    // spec 1): compares the two panes and opens the diff pane. `pane.sync-dirs`
    // (2026-08-11-directory-sync.md, spec 2) is the half that WRITES, and it
    // joined it here rather than staying Planned: it is built, and what it
    // needs that comparing does not — a journal, therefore the daemon — is a
    // fact about the SESSION, not about norte. That is `Facts::journalled` in
    // `crate::availability`, which dims it with a reason the reader can act on
    // instead of an issue number nobody can close. Neither takes a count: two
    // whole trees are a task, not a clamped mover (ADR 0044).
    live("pane.compare-dirs", false, Inert),
    // #312: the PAIR, which is a different question than comparing two
    // trees. It delegates to the `[ui] diff` program, so what norte decides
    // is the operand — two files, or it says so — and not the diff's
    // format.
    live("pane.compare-files", false, Launches),
    live("pane.sync-dirs", false, Writes),
    live("pane.search", false, Inert),
    live("pane.names-encoding", false, Inert),
    live("pane.toggle-hidden", false, Inert),
    live("pane.columns", false, Inert),
    live("pane.ai-rename", false, SendsOut),
    live("pane.organize", false, Writes),
    live("pane.semantic-search", false, SendsOut),
    live("pane.copy-path", false, Inert),
    // --- cursor (the count-aware family) ---
    live("cursor.up", true, Inert),
    live("cursor.down", true, Inert),
    live("cursor.page-up", true, Inert),
    live("cursor.page-down", true, Inert),
    live("cursor.top", false, Inert),
    live("cursor.bottom", false, Inert),
    // --- nav ---
    live("nav.enter", false, Inert),
    live("nav.parent", false, Inert),
    live("nav.back", true, Inert),
    live("nav.forward", true, Inert),
    // Krusader's jump point (`Ctrl+J`). No count: there is ONE point, and
    // jumping to it five times is jumping to it.
    live("nav.jump-back", false, Inert),
    live("nav.set-jump-point", false, Inert),
    // --- mark ---
    live("mark.toggle", false, Inert),
    live("mark.all", false, Inert),
    live("mark.invert", false, Inert),
    live("mark.clear", false, Inert),
    live("mark.pattern-add", false, Inert),
    live("mark.pattern-remove", false, Inert),
    // #313: the three gaps Total Commander has in its Gray family that
    // norte did not. `extension-*` acts on the extension of the entry
    // UNDER THE CURSOR; `files`/`dirs` are additive like `pattern-add`; and
    // `restore` gives back the selection from before the last block
    // gesture, which is the safety net for whoever clicked "unmark all" by
    // accident.
    live("mark.extension-add", false, Inert),
    live("mark.extension-remove", false, Inert),
    live("mark.files", false, Inert),
    live("mark.dirs", false, Inert),
    live("mark.restore", false, Inert),
    // Marking WHILE MOVING, which is the missing half of the family:
    // `space` and `insert` mark going down and there was nothing for going
    // up or for a range. `toggle-up` is `mark.toggle`'s exact mirror; the
    // two `page-*` ones apply to the whole range the opposite of what the
    // cursor's row has, which is what makes the gesture reversible; and
    // `to-top`/`to-bottom` are Krusader's `Shift+Home`/`Shift+End`, which
    // also UNMARK the other side — that is literal from its documentation
    // and is what distinguishes them from "adds a range".
    live("mark.toggle-up", false, Inert),
    live("mark.toggle-page-down", false, Inert),
    live("mark.toggle-page-up", false, Inert),
    live("mark.to-top", false, Inert),
    live("mark.to-bottom", false, Inert),
    // --- task ---
    live("task.cancel", false, Inert),
    // Pauses and resumes the same task it would cancel (ADR 0147).
    live("task.pause", false, Inert),
    // Relaunches the failed transfer (ADR 0148).
    live("task.retry", false, Inert),
    // The serial queue (ADR 0149): the switch and the reordering.
    live("task.queue", false, Inert),
    live("task.up", false, Inert),
    live("task.down", false, Inert),
    // The three for BROWSING the board sat in `Planned` for a while: phase
    // 6's parity matrix uncovered that the table declared them live with
    // NO frontend implementing them. They go back to live because the
    // window already does it (#292); the TUI still binds only
    // `task.cancel`, which is a normal asymmetry — what is not normal is
    // the table promising what nobody does.
    live("task.next", false, Inert),
    live("task.prev", false, Inert),
    live("task.dismiss", false, Inert),
    // --- viewer ---
    live("viewer.close", false, Inert),
    live("viewer.up", true, Inert),
    live("viewer.down", true, Inert),
    live("viewer.page-up", true, Inert),
    live("viewer.page-down", true, Inert),
    live("viewer.top", false, Inert),
    live("viewer.bottom", false, Inert),
    // SIDEWAYS. With a count, like its vertical twins: the viewer does not
    // wrap, so a long line is browsed just like a tall file.
    live("viewer.left", true, Inert),
    live("viewer.right", true, Inert),
    live("viewer.encoding", false, Inert),
    live("viewer.encoding-auto", false, Inert),
    live("viewer.hex", false, Inert),
    // An image's ZOOM (spec 2026-09-20). No count: `3` in front of "zoom
    // in" would read as "three steps", and the step is already the unit —
    // pressing it three times is exactly that and is seen as it happens.
    live("viewer.zoom-in", false, Inert),
    live("viewer.zoom-out", false, Inert),
    live("viewer.zoom-fit", false, Inert),
    // The listing's SIBLINGS: moving to the next photo without leaving the
    // viewer. `Inert` like `pane.view`, which does the same thing — open to
    // READ — and no count for the same reason as the zoom: the step is
    // already the unit and pressing it three times is seen as it happens.
    live("viewer.next", false, Inert),
    live("viewer.prev", false, Inert),
    // --- dialog ---
    live("dialog.confirm", false, Inert),
    live("dialog.cancel", false, Inert),
    live("dialog.approve", false, Inert),
    live("dialog.deny", false, Inert),
    live("dialog.overwrite", false, Inert),
    live("dialog.skip", false, Inert),
    live("dialog.rename", false, Inert),
    live("dialog.newer", false, Inert),
    // The four dialog movers are the shape that WOULD take a count, and they
    // declare `false` anyway (ADR 0044, rust-reviewer MAJOR-2): no overlay
    // dispatcher honours one. Every overlay handler resolves against this
    // screen and then resets the resolver on `Resolution::Counting` — the
    // same decision that gives overlays no multi-key sequences — so a count
    // typed over a dialog is destroyed at the digit and can never reach the
    // command. `true` here would be the catalogue claiming a capability
    // nothing implements, which is precisely the drift the shared catalogue
    // exists to end. Flip them the day an overlay learns to repeat.
    live("dialog.up", false, Inert),
    live("dialog.down", false, Inert),
    live("dialog.page-up", false, Inert),
    live("dialog.page-down", false, Inert),
    // The two ends. Same count rule as the movers above: no overlay repeats.
    live("dialog.top", false, Inert),
    live("dialog.bottom", false, Inert),
    // Jumping between sections in a text page (help).
    live("dialog.section-prev", false, Inert),
    live("dialog.section-next", false, Inert),
    live("dialog.add", false, Inert),
    live("dialog.toggle-enabled", false, Inert),
    live("dialog.remove", false, Inert),
    live("dialog.move-up", false, Inert),
    live("dialog.move-down", false, Inert),
    live("dialog.sort", false, Inert),
    live("dialog.cycle-format", false, Inert),
    live("dialog.pane", false, Inert),
    live("dialog.back", false, Inert),
    live("dialog.filter", false, Inert),
    // The history lists (spec 2026-09-15 D2): opening what is chosen in
    // the OTHER pane without moving the focus, and clearing the whole
    // list.
    live("dialog.confirm-other", false, Inert),
    live("dialog.clear", false, Inert),
    // --- planned: named by a preset, not built yet ---
    //
    // K2b imports four foreign keymaps (Total Commander, Krusader, Norton,
    // Far), and every one of them binds keys norte has not built. The choice
    // is between binding them HONESTLY — the key exists, says what it would
    // do and names the issue — and leaving them unbound, where the user
    // presses F4 and gets silence. K2b left twenty-eight entries in ten
    // families here (nine of them new: `pane.select-drive` was already here
    // and its family just grew two siblings); S4 built the shell family and
    // took its three away, 2026-08-10-volumes.md built the drive family (moved
    // to `live` above, closes #131), and 2026-08-11-directory-sync.md built the
    // second half of the compare/sync one (also `live` above, closes #134), so
    // seven families remain. Each is one capability and one issue;
    // `planned()` forces `counts: false`, which is right for all of them —
    // none is a clamped in-memory mover (ADR 0044).
    // #132: the five, built. None writes INSIDE a container — the archive
    // provider is still read-only, ADR 0018 — packing, splitting and
    // combining make new files, checking only reads, and unpacking is the
    // usual copy from inside the archive.
    live("pane.pack", false, Writes),
    live("pane.unpack", false, Writes),
    live("pane.test-archive", false, Inert),
    live("pane.split-file", false, Writes),
    live("pane.combine-files", false, Writes),
    // #133: norte does not bring an editor — its business is the file
    // manager — and F4 opens YOURS, which is what the four presets do when
    // binding it.
    live("pane.edit", false, Launches),
    live("pane.edit-new", false, Writes),
    // #134's second half (`pane.sync-dirs`) left this block and is `live`
    // above; `keymap-reason-sync` went with it, out of both locales, because
    // nothing else claimed it — same disposal as `keymap-reason-shell` when
    // S4 shipped, recorded below.
    // The three of issue #135 left this block in S4 and are `live` above; the
    // family's reason id (`keymap-reason-shell`) went with them, out of both
    // locales, because nothing else claimed it.
    // #136: the directory tree, docked to the left of the listing.
    live("pane.tree", false, Inert),
    // Sort by key (#138). `sort-menu` does not open its own menu: it opens
    // the columns dialog, which is where the sort has lived since #108 —
    // it has the column, the direction and `dirs_first` in one place — so
    // there are not two screens saying the same thing with a different
    // letter.
    live("pane.sort-name", false, Inert),
    live("pane.sort-ext", false, Inert),
    live("pane.sort-size", false, Inert),
    live("pane.sort-time", false, Inert),
    live("pane.sort-menu", false, Inert),
    // #139: properties come out of the listing; a folder's size is
    // COUNTED, and that is why it is a cancelable Task and not a field of
    // the dialog.
    live("pane.properties", false, Inert),
    // #314: the one category where the three reference managers TOUCH and
    // norte only looked. It is a whole mutation — journal with a reversal,
    // policy — and that is why it lives here and not inside the properties
    // dialog.
    live("pane.chmod", false, Writes),
    live("pane.dir-size", false, Inert),
    live("pane.checksum", false, ReadsContent),
    live("pane.checksum-verify", false, ReadsContent),
    // #140: opening is picking from `connections.toml`; disconnecting
    // actually DROPS the session, not just leaves the pane.
    live("pane.connect", false, Inert),
    live("pane.disconnect", false, Inert),
];

/// The entry for `name`, or `None` if the vocabulary has never heard of it —
/// which is a typo, and Task 3 keeps failing the load on it.
///
/// ```
/// use norte_frontend::keymap::catalogue::{Status, lookup};
///
/// assert_eq!(lookup("cursor.down").map(|d| d.counts), Some(true));
/// assert_eq!(lookup("app.quit").map(|d| d.counts), Some(false));
/// // #132 built the vocabulary's last `Planned` entry: today none is left,
/// // and `pane.pack` is `Live` like everything else a preset binds.
/// assert_eq!(lookup("pane.pack").map(|d| d.status), Some(Status::Live));
/// assert_eq!(lookup("pane.select-drive").map(|d| d.status), Some(Status::Live));
/// assert_eq!(lookup("pane.compare-dirs").map(|d| d.status), Some(Status::Live));
/// assert_eq!(lookup("pane.sync-dirs").map(|d| d.status), Some(Status::Live));
/// assert!(lookup("pane.no-exists-jamas").is_none());
/// ```
#[must_use]
pub fn lookup(name: &str) -> Option<&'static CommandDef> {
    CATALOGUE.iter().find(|d| d.name == name)
}

/// The [`Effect`] of `name`, or `None` if the vocabulary does not know it.
///
/// A caller deciding whether something may RUN must treat `None` as not
/// inert: an unknown name is not a licence.
#[must_use]
pub fn effect(name: &str) -> Option<Effect> {
    lookup(name).map(|d| d.effect)
}

#[cfg(test)]
mod tests {
    use super::{CATALOGUE, Status, lookup};

    /// A duplicated name would make `lookup` order-dependent, and the table is
    /// hand-maintained: pin it.
    #[test]
    fn no_hay_names_duplicados() {
        let mut names: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicated name in CATALOGUE");
    }

    /// A `Planned` entry with an empty reason or a zero issue is a promise
    /// nobody can chase — the exact failure this state exists to prevent.
    #[test]
    fn every_planned_one_has_a_reason_and_an_issue() {
        for d in CATALOGUE {
            if let Status::Planned { reason, issue } = d.status {
                assert!(!reason.is_empty(), "{} has no reason", d.name);
                assert!(issue > 0, "{} has no issue", d.name);
            }
        }
    }

    /// A reason id with no Fluent message renders as the raw id — an unbuilt
    /// key would then explain itself with `keymap-reason-...`, which is worse
    /// than saying nothing. Pin both locales.
    #[test]
    fn every_planned_reason_is_translated_in_both_locales() {
        for d in CATALOGUE {
            if let Status::Planned { reason, .. } = d.status {
                for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                    let s = norte_i18n::t_in(lang, reason);
                    assert_ne!(s, reason, "{} not translated in {lang:?}", d.name);
                }
            }
        }
    }

    /// `counts: true` is a licence to run a command up to 9 999 times from
    /// ONE keystroke (ADR 0044), so the set that holds it is pinned by name
    /// rather than by a rule. Every entry here is a clamped, in-memory,
    /// relative mover: no task submitted, no allocation per call, no screen
    /// opened. Adding to the set must fail this test, so that the argument
    /// gets made once — in review — instead of being discovered by a user who
    /// typed a number.
    ///
    /// `nav.back`/`nav.forward` are the two that reach the network, and they
    /// are here on a second bound: `nav::HISTORY_MAX` caps the trail at 64
    /// steps, and the TUI's repeat stops the moment a step does not land
    /// (a failed or cancelled step is put BACK on the trail, so without that
    /// the next turn would re-issue the identical listing).
    #[test]
    fn the_set_with_a_counter_is_exactly_this() {
        let mut with_count: Vec<&str> = CATALOGUE
            .iter()
            .filter(|d| d.counts)
            .map(|d| d.name)
            .collect();
        with_count.sort_unstable();
        assert_eq!(
            with_count,
            [
                "cursor.down",
                "cursor.page-down",
                "cursor.page-up",
                "cursor.up",
                // A count REPEATS the dispatch (ADR 0044), so "3 grow" grows
                // three times. The same reading as `cursor.down`, not an
                // exception.
                "layout.grow",
                "layout.shrink",
                "nav.back",
                "nav.forward",
                "viewer.down",
                // The two of the horizontal axis, for the same reason as
                // their vertical twins: clamped to the longest line, in
                // memory, with no task or new screen.
                "viewer.left",
                "viewer.page-down",
                "viewer.page-up",
                "viewer.right",
                "viewer.up",
            ],
            "a command gained or lost `counts`: see ADR 0044 before touching this list"
        );
    }

    /// A reason id is the name of ONE capability, so it must name ONE issue.
    /// K2b adds twenty-six Planned entries in nine families, transcribed by
    /// hand: a family whose issue number drifts on one line would send a user
    /// to the wrong tracker and nothing else would notice.
    #[test]
    fn each_reason_points_to_a_single_issue() {
        let mut seen: Vec<(&str, u32)> = Vec::new();
        for d in CATALOGUE {
            if let Status::Planned { reason, issue } = d.status {
                if let Some(&(_, other)) = seen.iter().find(|(r, _)| *r == reason) {
                    assert_eq!(
                        other, issue,
                        "{reason} points to #{other} and to #{issue} ({})",
                        d.name
                    );
                } else {
                    seen.push((reason, issue));
                }
            }
        }
    }

    /// What a command does to the reader's world is decided ONCE, here
    /// (ADR 0126), and a read-only window and the menu's colours are derived
    /// from it. A command that stops being `Inert`, or starts, must fail this
    /// test, so the argument is made in review: a mutating command classed
    /// `Inert` is one a read-only window would run.
    #[test]
    fn the_set_that_is_not_inert_is_exactly_this() {
        use super::Effect::{Destroys, Launches, ReadsContent, SendsOut, Writes};
        let mut acting: Vec<(&str, super::Effect)> = CATALOGUE
            .iter()
            .filter(|d| !d.effect.is_inert())
            .map(|d| (d.name, d.effect))
            .collect();
        acting.sort_unstable_by_key(|(n, _)| *n);
        assert_eq!(
            acting,
            [
                ("app.handoff", Launches),
                ("app.terminal", Launches),
                ("app.toggle-panels", Launches),
                // Opens a shell inside a panel: the same as `app.terminal`
                // in another form, and therefore the same effect.
                ("layout.terminal", Launches),
                ("pane.ai-rename", SendsOut),
                ("pane.checksum", ReadsContent),
                ("pane.checksum-verify", ReadsContent),
                ("pane.chmod", Writes),
                ("pane.combine-files", Writes),
                ("pane.command-line", Launches),
                ("pane.compare-files", Launches),
                ("pane.copy", Writes),
                ("pane.delete", Destroys),
                ("pane.delete-permanent", Destroys),
                ("pane.edit", Launches),
                ("pane.edit-new", Writes),
                ("pane.mkdir", Writes),
                ("pane.move", Writes),
                ("pane.open", Launches),
                ("pane.organize", Writes),
                ("pane.pack", Writes),
                ("pane.rename", Writes),
                ("pane.rename-batch", Writes),
                ("pane.semantic-search", SendsOut),
                ("pane.split-file", Writes),
                ("pane.sync-dirs", Writes),
                ("pane.unpack", Writes),
            ],
            "a command changed effect: see ADR 0126 before touching this list"
        );
    }

    #[test]
    fn lookup_finds_and_fails_correctly() {
        assert!(lookup("pane.copy").is_some());
        assert!(lookup("pane.no-existe-jamas").is_none());
    }
}
