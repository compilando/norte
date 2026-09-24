//! What commands this host knows how to run, and what it does with the ones
//! it does not.
//!
//! The list matters for two reasons. One: it is what the effective keymap
//! needs to decide whether a bound key can run HERE (`Availability::NotHere`
//! is "this frontend does not implement it", and without the list there is
//! no way to tell that apart from "norte has not built it"). And two: it is
//! the only honest declaration of how far the host reaches, instead of a
//! `match` that silently swallows what it does not recognize.

/// How far a frontend reaches: whether it can MUTATE or only look.
///
/// It is not an amputation of the host — the host knows how to delete and
/// create, and its tests prove it — but a STARTUP decision made by whoever
/// mounts it. The graphical window starts read-only until phase 5 gives it
/// the safe path (phase 4's exit gate requires it), and until then a key
/// bound to `pane.delete` in the preset is answered instead of executed:
/// the key existing is not permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Efectos {
    /// Only look: navigate, mark, sort, view. Nothing that writes, and no
    /// approving an agent's write either.
    SoloLectura,
    /// Everything the host implements.
    Completo,
}

/// The commands the host runs in each mode.
///
/// The read-only list is the usual one MINUS what is not inert: what
/// writes, deletes, launches a foreign program, reads whole content or sends
/// data out of the process. That judgment is NOT made here: it is the
/// `effect` the catalogue declares on each row, with no default value (ADR
/// 0126). It used to be its own list, `MUTAN`, and forgetting to update it
/// when adding a command that writes left the "look only" window running it.
#[must_use]
pub fn implementados(efectos: Efectos) -> Vec<&'static str> {
    match efectos {
        Efectos::Completo => IMPLEMENTADOS.to_vec(),
        Efectos::SoloLectura => IMPLEMENTADOS
            .iter()
            .copied()
            .filter(|c| inerte(c))
            .collect(),
    }
}

/// Whether the catalogue declares `command` inert. A name the catalogue
/// does not know is NOT one: not knowing what it does does not authorize
/// running it.
fn inerte(command: &str) -> bool {
    norte_frontend::keymap::catalogue::effect(command)
        .is_some_and(norte_frontend::keymap::Effect::is_inert)
}

/// The commands the host runs TODAY.
///
/// Grows with each task of phase 2. Everything else in the catalogue
/// resolves to [`norte_frontend::keymap::Availability::NotHere`] and is
/// STATED in the bar, exactly what the TUI does with its own.
pub const IMPLEMENTADOS: &[&str] = &[
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "nav.back",
    "nav.forward",
    "nav.jump-back",
    "nav.set-jump-point",
    "mark.toggle",
    "mark.clear",
    "mark.all",
    "mark.invert",
    "mark.pattern-add",
    "mark.pattern-remove",
    "mark.extension-add",
    "mark.extension-remove",
    "mark.files",
    "mark.dirs",
    "mark.restore",
    "mark.toggle-up",
    "mark.toggle-page-down",
    "mark.toggle-page-up",
    "mark.to-top",
    "mark.to-bottom",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.set-target",
    "layout.grow",
    "layout.shrink",
    "layout.equalize",
    "layout.flip",
    "layout.pick",
    "layout.split-h",
    "layout.split-v",
    "layout.close-slot",
    "layout.places",
    "layout.processes",
    "layout.log",
    "layout.disk-map",
    "layout.timeline",
    "layout.terminal",
    "layout.metadata",
    "layout.preview",
    "pane.tree",
    "pane.tab-new",
    "pane.tab-close",
    "pane.tab-next",
    "pane.tab-prev",
    "pane.tab-move-left",
    "pane.tab-move-right",
    "pane.tab-goto-1",
    "pane.tab-goto-2",
    "pane.tab-goto-3",
    "pane.tab-goto-4",
    "pane.tab-goto-5",
    "pane.tab-goto-6",
    "pane.tab-goto-7",
    "pane.tab-goto-8",
    "pane.tab-goto-9",
    "pane.columns",
    "app.palette",
    "app.goto",
    "app.help",
    "app.settings",
    "app.extensions",
    "app.agents",
    "app.terminal",
    "app.handoff",
    "pane.open",
    "pane.compare-files",
    "pane.edit",
    "pane.copy-path",
    "app.theme",
    "app.menu",
    // Quit via the key, like in the terminal: the window used to treat it
    // as "the window manager closes it", and `F10`, `q` and "Quit" in the
    // menu did nothing. It goes through the same path as the close button.
    "app.quit",
    "profile.pick",
    "profile.save-as",
    "profile.next",
    "profile.prev",
    "pane.select-drive",
    "pane.connect",
    "pane.disconnect",
    "pane.view",
    "pane.quick-search",
    "pane.search",
    "pane.mkdir",
    "pane.edit-new",
    "pane.delete",
    "pane.delete-permanent",
    "pane.copy",
    "pane.move",
    "pane.rename",
    "pane.chmod",
    "pane.checksum",
    "pane.checksum-verify",
    "pane.ai-rename",
    "pane.organize",
    "pane.rename-batch",
    "pane.semantic-search",
    "pane.compare-dirs",
    "pane.sync-dirs",
    // #290 phase A: the pane gestures the TUI had and the window did not.
    // None of them writes or sends data out of the process, so the
    // catalogue declares them inert: re-listing is the same thing
    // navigating already does.
    "pane.sort-name",
    "pane.sort-ext",
    "pane.sort-size",
    "pane.sort-time",
    "pane.sort-menu",
    "pane.refresh",
    "pane.toggle-hidden",
    "pane.names-encoding",
    "pane.properties",
    "pane.dir-size",
    "pane.pack",
    "pane.unpack",
    "pane.test-archive",
    "pane.split-file",
    "pane.combine-files",
    "pane.mirror",
    "pane.mirror-target",
    "pane.sync-nav",
    "pane.pull",
    "pane.swap",
    "pane.history",
    "pane.hotlist",
    "pane.popular",
    "pane.history-left",
    "pane.history-right",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "task.cancel",
    "task.pause",
    "task.retry",
    "task.queue",
    "task.up",
    "task.down",
    "task.next",
    "task.prev",
    "task.dismiss",
];

/// The commands of the VIEWER screen that the host runs.
///
/// A separate list because it is another screen, and its effective keymap
/// is built with `Screen::Viewer`: a command not here resolves to
/// [`norte_frontend::keymap::Availability::NotHere`] and is STATED, same as
/// in the listing.
/// The DIALOG verbs this host handles.
///
/// Four, not the catalogue's twenty-two: this window's dialogs are
/// questions with two answers — confirm/cancel, approve/deny — and the
/// other verbs (`dialog.overwrite`, `dialog.sort`, `dialog.pane`…) name
/// answers to dialogs that do not exist here. A preset can bind them: the
/// key will say no, here, with the same phrase as any other command this
/// window does not do.
pub const IMPLEMENTADOS_DIALOGO: &[&str] = &[
    "dialog.confirm",
    "dialog.cancel",
    "dialog.approve",
    "dialog.deny",
    // The four ways out of a collision (#287). Each names ITS answer:
    // "confirm" does not say which of the four.
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    // Walking a modal list, and its two ends (`dialog.top`/`bottom`).
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.top",
    "dialog.bottom",
    "dialog.section-prev",
    "dialog.section-next",
    // The column selector and the extension manager.
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
    "dialog.add",
    // `dialog.remove` was DEFERRED (#287) for a reason that was true: removing
    // a row only means something for a list that can be EDITED, and the only
    // one in this window — settings — is read-only. Since #309 there is one
    // that can: favorites. And until this name entered here, binding it in
    // the preset did nothing — the effective keymap filters by this list, so
    // the key existed and went nowhere.
    "dialog.remove",
    // Switching sides (compare, help), going back (help) and filtering
    // (help).
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
    // The history lists (spec 2026-09-15 D2): open in the other slot, and
    // clear the history or the popular ones.
    "dialog.confirm-other",
    "dialog.clear",
];

/// The VIEWER's commands this host implements.
///
/// A separate list because the viewer is another SCREEN: with it open the
/// keys are its own, and mixing them with the listing's would be an input
/// context that does not exist in any preset.
pub const IMPLEMENTADOS_VISOR: &[&str] = &[
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.left",
    "viewer.right",
    "viewer.hex",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.zoom-in",
    "viewer.zoom-out",
    "viewer.zoom-fit",
    "viewer.next",
    "viewer.prev",
];

/// Everything the host implements, across both screens.
///
/// It is what gets passed to `Effective::build_for` in BOTH: the effective
/// keymap needs to know what exists to be able to tell "this frontend does
/// not do it" apart from "norte does not have it", and that question is not
/// per screen.
#[must_use]
pub fn todos() -> Vec<&'static str> {
    todos_con(Efectos::Completo)
}

/// Same, with the effects mode stated.
#[must_use]
pub fn todos_con(efectos: Efectos) -> Vec<&'static str> {
    let mut v = implementados(efectos);
    v.extend_from_slice(IMPLEMENTADOS_VISOR);
    v
}

/// What a VIEWER command asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EfectoVisor {
    /// Closes the viewer.
    Cerrar,
    /// Scrolls this many lines (negative is upward).
    Linea(i64),
    /// Scrolls this many PAGES (negative is upward).
    Pagina(i64),
    /// Scrolls this many COLUMNS (negative is toward the left).
    ///
    /// The viewer does not wrap: without this, the tail of a line wider
    /// than the window was nowhere to be found.
    Columna(i64),
    /// To the beginning or the end.
    Extremo {
        /// `true` = to the end.
        al_final: bool,
    },
    /// Toggles hexadecimal.
    Hex,
    /// Reloads with the next encoding in the cycle.
    Encoding,
    /// Goes back to automatic detection.
    EncodingAuto,
    /// Moves the image's zoom one notch (spec 2026-09-20).
    Zoom {
        /// `true` = zoom in.
        acercar: bool,
    },
    /// Returns the image to FIT.
    ZoomAjustar,
    /// Opens the next (or previous) sibling of the same class, without
    /// exiting.
    Hermana {
        /// `true` = the next one.
        adelante: bool,
    },
}

/// Translates a viewer-screen command into its effect.
///
/// `None` = the host does not implement it; the caller turns it into an
/// `Unavailable` the user sees.
#[must_use]
pub fn efecto_visor_de(command: &str, veces: u32) -> Option<EfectoVisor> {
    let n = i64::from(veces.max(1).min(u32::from(u16::MAX)));
    Some(match command {
        "viewer.close" => EfectoVisor::Cerrar,
        "viewer.up" => EfectoVisor::Linea(-n),
        "viewer.down" => EfectoVisor::Linea(n),
        "viewer.page-up" => EfectoVisor::Pagina(-n),
        "viewer.page-down" => EfectoVisor::Pagina(n),
        "viewer.top" => EfectoVisor::Extremo { al_final: false },
        "viewer.bottom" => EfectoVisor::Extremo { al_final: true },
        "viewer.left" => EfectoVisor::Columna(-n),
        "viewer.right" => EfectoVisor::Columna(n),
        "viewer.hex" => EfectoVisor::Hex,
        "viewer.encoding" => EfectoVisor::Encoding,
        "viewer.encoding-auto" => EfectoVisor::EncodingAuto,
        "viewer.zoom-in" => EfectoVisor::Zoom { acercar: true },
        "viewer.zoom-out" => EfectoVisor::Zoom { acercar: false },
        "viewer.zoom-fit" => EfectoVisor::ZoomAjustar,
        "viewer.next" => EfectoVisor::Hermana { adelante: true },
        "viewer.prev" => EfectoVisor::Hermana { adelante: false },
        _ => return None,
    })
}

/// What a command asks of the focused slot.
///
/// It is the host's INTERNAL vocabulary: the renderer never sees it. It
/// exists so the resolver and the mouse end up in the same place — a gesture
/// and a key that mean the same thing have to do the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Efecto {
    /// Moves the cursor this many rows (negative is upward).
    Cursor(i64),
    /// Moves the cursor this many PAGES (negative is upward). How many rows
    /// that is is decided by the slot, with the window the renderer told it.
    Pagina(i64),
    /// Cursor to the start or the end of the listing.
    Extremo {
        /// `true` = to the end.
        al_final: bool,
    },
    /// Enters whatever is under the cursor.
    Entrar,
    /// Goes up to the parent directory.
    Subir,
    /// Navigation trail.
    Rastro {
        /// `true` = back.
        atras: bool,
    },
    /// Marks or unmarks the cursor's row.
    Marcar,
    /// Marks or unmarks the cursor's row and MOVES UP (`shift+↑`).
    MarcarSubiendo,
    /// Marks (or unmarks) a page's span and moves there.
    MarcarPagina {
        /// `true` = downward.
        abajo: bool,
    },
    /// Marks from the cursor to one end and UNMARKS the other side
    /// (Krusader's `shift+Home`/`shift+End`).
    MarcarHastaElBorde {
        /// `true` = upward.
        arriba: bool,
    },
    /// Clears all marks.
    DesmarcarTodo,
    /// Moves focus to the next focusable slot (or the previous one).
    ///
    /// With two panes it is the usual switch; with more, it follows the
    /// tab ORDER the shared layer resolves, which already skips what is not
    /// visible and what cannot be focused.
    Foco {
        /// `true` = backward.
        atras: bool,
        /// `true` = only LISTINGS stop; side panels are skipped.
        ///
        /// It is the difference between `pane.switch` and
        /// `layout.focus-next`: the first is "the other pane" of any
        /// orthodox manager and the second is the whole screen's traversal.
        /// A single ring for both forced five presses to get back to the
        /// listing next door with the places bar, the tree and the viewer
        /// open.
        solo_listados: bool,
    },
    /// Designates ANOTHER slot as the destination of the next operation.
    Destino,
    /// Resizes the focused slot. Negative shrinks it.
    Tamano(i64),
    /// Equalizes the weight of the focused slot's siblings.
    Igualar,
    /// Rotates the focused slot's split (ADR 0138).
    Girar,
    /// Opens the layout picker.
    Disposiciones,
    /// Opens the column picker.
    Columnas,
    /// Opens the command palette.
    Paleta,
    /// Opens "go to anywhere" (#357).
    IrA,
    /// Opens settings: they are read, cycled and written.
    Ajustes,
    /// Opens the extension manager, read-only.
    Extensiones,
    /// The agent sessions viewed, and undoing a whole one.
    Agentes,
    /// Opens another TAB next to the focused slot.
    PestanaNueva,
    /// Closes the focused tab. Without a group, does nothing.
    CerrarPestana,
    /// Moves to the next — or previous — tab, cycling.
    CiclarPestana {
        /// Backward.
        atras: bool,
    },
    /// Moves the focused tab within its group.
    MoverPestana {
        /// To the right.
        derecha: bool,
    },
    /// Goes to tab `n` (1-based) of the focused group.
    IrAPestana {
        /// Which one, starting at 1.
        n: usize,
    },
    /// Splits the focused slot and puts another LISTING next to it.
    Partir {
        /// One above the other instead of one beside the other.
        vertical: bool,
    },
    /// Closes the focused slot.
    CerrarHueco,
    /// Opens the TERMINAL panel, or brings it to front and gives it focus
    /// (#362).
    ///
    /// **It never closes it**, and that is a deliberate divergence from
    /// [`Self::AlternarHueco`]: inside there is a shell belonging to the
    /// reader, with whatever it had half-done. Closing it would kill it, and
    /// that cannot be what the same key that enters it does. To close it
    /// there is `layout.close-slot`, named for what it does.
    AbrirTerminal,
    /// Opens — or closes — the auxiliary slot of this kind.
    AlternarHueco {
        /// `places`, `processes`, `metadata` or `tree`: the ones this window
        /// knows how to PAINT. Opening one that would only paint gray is not
        /// opening it.
        kind: &'static str,
    },
    /// Moves the BOARD's selected row, without needing to focus it.
    TaskVecina {
        /// Upward.
        atras: bool,
    },
    /// Removes the selected row from the board, if it already finished.
    DescartarTask,
    /// Marks ALL rows of the active pane.
    MarcarTodo,
    /// Inverts the active pane's marks.
    InvertirMarcas,
    /// Marks — or unmarks — the ones with the SAME extension as the cursor's
    /// (#313).
    MarcarExtension {
        /// `true` adds marks, `false` removes them.
        marcar: bool,
    },
    /// Marks entries of a CLASS: directories or files (#313).
    MarcarClase {
        /// `true` marks directories, `false` files.
        dirs: bool,
    },
    /// Restores the selection prior to the last block gesture (#313).
    RestaurarMarcas,
    /// Changes the POSIX PERMISSIONS of what is marked (#314): asks for the
    /// mode in octal.
    Permisos,
    /// Computes checksums of what is marked, or VERIFIES the checksum file
    /// under the cursor (#311).
    Sumas {
        /// `true` verifies against a checksum file; `false` computes.
        verificar: bool,
    },
    /// Marks — or unmarks — by PATTERN: opens the glob prompt.
    MarcarPatron {
        /// `true` adds marks, `false` removes them.
        marcar: bool,
    },
    /// Copies the paths of what is marked (or of what is selected) to the
    /// clipboard.
    CopiarRuta,
    /// Opens what is selected with the application the desktop picks.
    AbrirExterno,
    /// Edits what is selected with the editor `[ui] editor` names; without
    /// one, the same as [`Efecto::AbrirExterno`].
    EditarExterno,
    /// Compares TWO files (#312) with the program from `[ui] diff` — or
    /// `diff -u` — launched by the host process: detached if it opens a
    /// window, waited on and its output captured if not.
    CompararFicheros,
    /// Opens a terminal sitting in the active pane's directory.
    Terminal,
    /// Hands the screen off to the TERMINAL and closes this window (phase
    /// 9).
    ///
    /// Not inert (ADR 0126) and does not write a file: what it writes is the
    /// SESSION, and it also releases it and closes the window. A look-only
    /// window does none of the three.
    Relevo,
    /// Shows the active theme from the inside.
    Tema,
    /// Unfolds the menu bar. Neither adds capabilities nor removes them: it
    /// offers the same catalogue commands, sorted by topic, for whoever
    /// does not know the name of what they are looking for.
    Menu,
    /// Asks to close the window: the same path as the close button, with
    /// the same `[ui] confirm_quit` question.
    Salir,
    /// Opens the PROFILE picker (ADR 0079).
    PerfilElegir,
    /// Saves the CURRENT workspace as a profile (#318).
    PerfilGuardarComo,
    /// Jumps to the next or previous profile, without opening anything.
    PerfilVecino {
        /// Toward the previous one.
        atras: bool,
    },
    /// Opens the host's volume picker.
    Volumenes,
    /// Opens help. On the page for the CONTEXT the reader is in — an open
    /// dialog, the viewer, the listing — and not always on the index:
    /// whoever presses F1 while looking at a question wants that answer.
    Ayuda,
    /// Opens the viewer on the entry under the cursor.
    Ver,
    /// Opens the listing's incremental search.
    BuscarRapido,
    /// Asks for the PLAN to sync the active pane onto the destination.
    ///
    /// The plan does NOT write: it says what it would do. Still not inert,
    /// because it is the door to a write and a window that declares itself
    /// look-only does not open it.
    Sincronizar,
    /// Compares the two panes and opens the differences pane.
    ///
    /// Does NOT mutate: it walks both trees and answers. It is a long,
    /// cancellable task, and cancelling it is its only brake.
    Comparar,
    /// Packs what is MARKED into a new container (#132).
    ///
    /// Not inert (ADR 0126): it writes a file. The name is typed, and the
    /// FORMAT comes from it — a name with no known extension is refused
    /// instead of packing into something nobody asked for.
    Empaquetar,
    /// Copies the container's INSIDE under the cursor to the destination
    /// pane (#132).
    ///
    /// It carries no method of its own and needs none: the copy engine
    /// accepts an archive's inside as a source, so unpacking is the copy the
    /// reader could have done by hand — with its journal, its undo, its
    /// collision policy and its cancellation.
    Desempaquetar,
    /// Checks the container under the cursor (#132).
    ///
    /// Inert (ADR 0126): it reads the whole archive and answers whether it
    /// is sound, without writing anything. Same category as comparing.
    ComprobarArchivo,
    /// The configured-connections picker (#264).
    ///
    /// Inert (ADR 0126): listing opens nothing. Choosing one NAVIGATES, and
    /// navigating is what establishes the session — with the same gate as
    /// any other listing, and its TOFU if needed.
    Conexiones,
    /// Closes the active pane's session and takes it out of there (#140).
    ///
    /// Inert (ADR 0126): releasing a session does not write a single byte
    /// anywhere. What it does do is leave the pane looking at something it
    /// can no longer read, which is why it navigates afterward.
    Desconectar,
    /// Splits the file under the cursor into chunks of the typed size
    /// (#132). Not inert (ADR 0126): it writes the chunks.
    ///
    /// `PartirFichero` and not plain `Partir`: [`Efecto::Partir`] is
    /// splitting a layout SLOT, which has nothing to do with this.
    PartirFichero,
    /// Joins the chunks starting from the `.001` under the cursor (#132).
    /// Also writes, so it is not inert either.
    Juntar,
    /// Counts how much space what is MARKED — or what is under the cursor —
    /// takes up (#139).
    ///
    /// Inert for the same reason as [`Efecto::Comparar`]: it walks a tree
    /// and answers, without writing or sending out of the process anything
    /// listing did not already send. It is long and cancellable, and the
    /// board shows it as `dir-size`.
    TamanoDeDirectorio,
    /// Asks for a SEMANTIC search against the index: opens the query prompt.
    ///
    /// Not inert (ADR 0126) and does not write a byte: the query LEAVES the
    /// process toward the configured AI provider, same as a directory's
    /// contents in [`Efecto::RenameIa`].
    BuscarSemantica,
    /// Opens the subtree search prompt.
    Buscar,
    /// Opens the create-directory prompt.
    CrearDirectorio,
    /// Opens the prompt to create an EMPTY file and edit it (#290).
    ///
    /// Not inert (ADR 0126): it creates a node on disk, with its journal
    /// entry and its undo, exactly like creating a directory.
    CrearFichero,
    /// Asks to delete what is marked (or whatever is under the cursor). Does
    /// NOT delete: it opens the confirmation, which is where ALL paths pass
    /// through — key, menu, gesture — because a destructive operation with
    /// two doors ends up with one unlocked.
    Borrar {
        /// Permanent, no trash.
        permanente: bool,
    },
    /// Asks to copy or move what is marked (or whatever is under the
    /// cursor) to the DESTINATION slot. Does NOT transfer: it opens the
    /// confirmation, for the same reason as [`Efecto::Borrar`] — and here
    /// the confirmation is also the only thing that shows WHERE it goes,
    /// which in a window with three listings is not obvious.
    Transferir {
        /// `true` = move; `false` = copy. On the wire these are two
        /// different methods, so this does not pick an option: it picks the
        /// verb.
        mover: bool,
    },
    /// Asks for a rename plan for the WHOLE directory. Opens the
    /// instruction prompt; the plan arrives afterward and is reviewed
    /// before anything else.
    RenameIa,
    /// Asks for an ORGANIZE plan for the whole directory (phase 8). Opens
    /// no prompt: what is asked is "look at this directory and propose a
    /// shape", so the plan arrives on its own and is reviewed as a tree
    /// before anything else.
    Organizar,
    /// Batch rename by TEMPLATE (#310): opens the template prompt, and the
    /// plan — deterministic, no model — goes through the SAME review as the
    /// AI's.
    RenameLote,
    /// Asks to stop a task on the board.
    ///
    /// Survives [`Efectos::SoloLectura`] **only for its own tasks**, and the
    /// distinction is not formalism: stopping a copy DOES touch disk — the
    /// destination gets cleaned up or a `.norte-partial` is left, which is
    /// the project's rule — so a window mounted with no effects cannot abort
    /// ANOTHER client's transfer and leave it a partial. Its own tasks are a
    /// different matter: if it could launch them, it can stop them.
    CancelarTask,
    /// Pauses the selected task, or resumes it if already paused (ADR
    /// 0147). The same choice as [`Self::CancelarTask`], and the same rule
    /// for ANOTHER client's tasks in a window with no effects.
    PausarTask,
    /// Retries the most recent failed transfer with the same options (ADR
    /// 0148). Mutates, so a window with no effects refuses it.
    ReintentarTask,
    /// Sends transfers launched from now on to the serial queue, or stops
    /// doing so (ADR 0149). Does not touch what is already queued.
    AlternarCola,
    /// Moves the selected task up (`arriba`) or down the queue, if it has
    /// not started yet.
    MoverEnCola {
        /// Toward the front of the queue.
        arriba: bool,
    },
    /// Sorts the focused listing by this column.
    ///
    /// The same semantics as a click on the header: the active column
    /// reverses, a new one sorts ascending. `SortSpec::after_click` is what
    /// decides it, not a second table here.
    ///
    /// The KEY of a sort column, not the column itself: only sort keys
    /// arrive here (`pane.sort-name`, `-size`…), which are always built-ins.
    /// Sorting by an attribute enters through a click on its header
    /// (`ordenar_por`), so putting the whole `SortColumn` here — which
    /// stopped being `Copy` once it could carry an id (ADR 0144) — would
    /// strip `Copy` from all of `Efecto` for a case that never arrives
    /// through this path.
    Ordenar(norte_config::SortColumnKey),
    /// Asks again for the listing of the slots that are visible.
    ///
    /// For ALL of them, not just the focused one: an external change rarely
    /// respects focus, which is why the TUI refreshes both panes.
    Refrescar,
    /// Hides or restores the active pane's hidden files.
    ///
    /// Presentation-only (#107): the provider does not re-list.
    AlternarOcultos,
    /// Cycles the reinterpretation of names that are not UTF-8 (#57).
    ///
    /// Display-only, rule 1: the bytes are not touched.
    CiclarEncoding,
    /// The ACTIVE slot's location travels to the DESTINATION slot.
    Espejo,
    /// Turns SYNCED navigation on or off: while it is on, every navigation
    /// of the active slot is repeated by the destination.
    ///
    /// Does not navigate on its own, which is why it is not in the pane
    /// gesture group next to it: all it does is flip a switch.
    EspejoPermanente,
    /// Like [`Efecto::Espejo`], but what travels is the CURSOR'S TARGET: the
    /// folder under it if it is one, and if not the active slot's location
    /// (Krusader's `Ctrl+←`/`Ctrl+→`). Which directory that is is decided by
    /// `PaneState::target_dir`, one shared by both frontends (ADR 0077).
    EspejoObjetivo,
    /// The DESTINATION slot's location travels to the ACTIVE one: the
    /// mirror in reverse.
    Traer,
    /// The two slots — active and destination — swap places.
    ///
    /// Does not touch disk: both listings already existed.
    Intercambiar,
    /// Opens the active slot's navigation-trail list.
    Historial,
    /// Opens the configuration's favorites list.
    Hotlist,
    /// Opens the session's POPULAR directories (spec 2026-09-15 D6).
    Populares,
    /// Opens the history of one SIDE of the screen (D7), resolved by the
    /// split's geometry as in [`Efecto::VolumenesDeLado`].
    HistorialDeLado {
        /// The rightmost one instead of the leftmost.
        derecha: bool,
    },
    /// Returns to the active slot's jump point (D5).
    SaltoAtras,
    /// Sets the jump point at the active slot's directory (D5).
    FijarSalto,
    /// Opens the volume picker for one SIDE of the screen.
    ///
    /// A side, not the focus: it is what Total Commander's
    /// `Alt+F1`/`Alt+F2` do, and what the TUI does with its
    /// `panes[0]`/`panes[1]`. Here the side is decided by the split's
    /// GEOMETRY, the only thing that means "left" in a tree of slots.
    VolumenesDeLado {
        /// The rightmost one instead of the leftmost.
        derecha: bool,
    },
    /// Asks to rename the entry under the cursor. Does NOT rename: it opens
    /// the name for editing.
    ///
    /// On the WIRE it is a move to the same directory, and it is still its
    /// own effect: what it asks is a different thing (a name, not a place),
    /// what it refuses is a different thing (a multi-selection, not a
    /// missing destination) and what seeds the field has a rule no other
    /// surface has — the UNTOUCHED name travels as bytes.
    Renombrar,
}

/// Translates a catalogue command into the effect the host applies.
///
/// `None` = the host does not implement it. Not a silent discard: the
/// caller turns it into an `Unavailable` the user sees.
#[must_use]
// A TABLE: one arm per catalogue command, and each arm is a name. Long
// because of the number of commands, not logic — splitting it into two
// arbitrary halves would only hide the other half, and what makes a table
// readable is seeing it whole. Same criterion as the actor's message
// dispatch.
#[expect(
    clippy::too_many_lines,
    reason = "command→effect table: readable whole, like the actor's message dispatch"
)]
pub fn efecto_de(command: &str, veces: u32) -> Option<Efecto> {
    let n = i64::from(veces.max(1).min(u32::from(u16::MAX)));
    Some(match command {
        "cursor.up" => Efecto::Cursor(-n),
        "cursor.down" => Efecto::Cursor(n),
        // A page is the VISIBLE rows, and how many that is is known by the
        // slot (the renderer told it via `SetVisibleRange`): that is why it
        // travels as pages and not as rows.
        "cursor.page-up" => Efecto::Pagina(-n),
        "cursor.page-down" => Efecto::Pagina(n),
        "cursor.top" => Efecto::Extremo { al_final: false },
        "cursor.bottom" => Efecto::Extremo { al_final: true },
        "nav.enter" => Efecto::Entrar,
        "nav.parent" => Efecto::Subir,
        "nav.back" => Efecto::Rastro { atras: true },
        "nav.forward" => Efecto::Rastro { atras: false },
        "nav.jump-back" => Efecto::SaltoAtras,
        "nav.set-jump-point" => Efecto::FijarSalto,
        "mark.toggle" => Efecto::Marcar,
        "mark.clear" => Efecto::DesmarcarTodo,
        "mark.all" => Efecto::MarcarTodo,
        "mark.invert" => Efecto::InvertirMarcas,
        "mark.pattern-add" => Efecto::MarcarPatron { marcar: true },
        "mark.pattern-remove" => Efecto::MarcarPatron { marcar: false },
        "mark.extension-add" => Efecto::MarcarExtension { marcar: true },
        "mark.extension-remove" => Efecto::MarcarExtension { marcar: false },
        "mark.files" => Efecto::MarcarClase { dirs: false },
        "mark.dirs" => Efecto::MarcarClase { dirs: true },
        "mark.restore" => Efecto::RestaurarMarcas,
        "mark.toggle-up" => Efecto::MarcarSubiendo,
        "mark.toggle-page-down" => Efecto::MarcarPagina { abajo: true },
        "mark.toggle-page-up" => Efecto::MarcarPagina { abajo: false },
        "mark.to-top" => Efecto::MarcarHastaElBorde { arriba: true },
        "mark.to-bottom" => Efecto::MarcarHastaElBorde { arriba: false },
        // `pane.switch` is "the other pane": it cycles the LISTINGS, all of
        // them there are, and nothing else. `layout.focus-*` is the whole
        // screen's traversal, side panels included. They used to share an
        // arm, and that meant that with the tree and the viewer open, tab
        // took five stops to get back to the listing next door.
        "pane.switch" => Efecto::Foco {
            atras: false,
            solo_listados: true,
        },
        "layout.focus-next" => Efecto::Foco {
            atras: false,
            solo_listados: false,
        },
        "layout.focus-prev" => Efecto::Foco {
            atras: true,
            solo_listados: false,
        },
        "layout.set-target" => Efecto::Destino,
        "layout.grow" => Efecto::Tamano(n),
        "layout.shrink" => Efecto::Tamano(-n),
        "layout.equalize" => Efecto::Igualar,
        "layout.flip" => Efecto::Girar,
        "layout.pick" => Efecto::Disposiciones,
        "layout.split-h" => Efecto::Partir { vertical: false },
        "layout.split-v" => Efecto::Partir { vertical: true },
        "layout.close-slot" => Efecto::CerrarHueco,
        "layout.places" => Efecto::AlternarHueco { kind: "places" },
        "layout.processes" => Efecto::AlternarHueco { kind: "processes" },
        "layout.log" => Efecto::AlternarHueco { kind: "log" },
        "layout.terminal" => Efecto::AbrirTerminal,
        "layout.disk-map" => Efecto::AlternarHueco { kind: "disk-map" },
        "layout.timeline" => Efecto::AlternarHueco { kind: "timeline" },
        // The last of the seven from ADR 0058 (#291): the docked viewer.
        "layout.preview" => Efecto::AlternarHueco { kind: "viewer" },
        "pane.tree" => Efecto::AlternarHueco { kind: "tree" },
        // `pane.properties` falls here on purpose: this window's properties
        // ARE the attribute sheet, which already shows name, class, size and
        // date of what is selected. It does it a different way, same as
        // sorting by clicking the header.
        "layout.metadata" | "pane.properties" => Efecto::AlternarHueco { kind: "metadata" },
        "pane.tab-new" => Efecto::PestanaNueva,
        "pane.tab-close" => Efecto::CerrarPestana,
        "pane.tab-next" => Efecto::CiclarPestana { atras: false },
        "pane.tab-prev" => Efecto::CiclarPestana { atras: true },
        "pane.tab-move-left" => Efecto::MoverPestana { derecha: false },
        "pane.tab-move-right" => Efecto::MoverPestana { derecha: true },
        "pane.tab-goto-1" => Efecto::IrAPestana { n: 1 },
        "pane.tab-goto-2" => Efecto::IrAPestana { n: 2 },
        "pane.tab-goto-3" => Efecto::IrAPestana { n: 3 },
        "pane.tab-goto-4" => Efecto::IrAPestana { n: 4 },
        "pane.tab-goto-5" => Efecto::IrAPestana { n: 5 },
        "pane.tab-goto-6" => Efecto::IrAPestana { n: 6 },
        "pane.tab-goto-7" => Efecto::IrAPestana { n: 7 },
        "pane.tab-goto-8" => Efecto::IrAPestana { n: 8 },
        "pane.tab-goto-9" => Efecto::IrAPestana { n: 9 },
        // The "sort menu" IS the columns dialog: the column, the direction
        // and `dirs_first` are all there. A second screen for the same
        // thing would be one more to maintain and one more to learn, and it
        // is the same decision the TUI made.
        "pane.columns" | "pane.sort-menu" => Efecto::Columnas,
        "app.palette" => Efecto::Paleta,
        "app.goto" => Efecto::IrA,
        "app.help" => Efecto::Ayuda,
        "app.settings" => Efecto::Ajustes,
        "app.quit" => Efecto::Salir,
        "app.extensions" => Efecto::Extensiones,
        "app.agents" => Efecto::Agentes,
        "pane.copy-path" => Efecto::CopiarRuta,
        // F4 launches the editor `[ui] editor` names, and if there is none
        // it falls back to `pane.open` — i.e. to `openers.toml` and,
        // ultimately, to the DESKTOP's application.
        //
        // What stays out is `$EDITOR` (#290), and that remains deliberate:
        // the TUI launches it because it is already inside a terminal, and
        // this window has none to put it in. `[ui] editor` is a different
        // thing — a program the reader names, which can be graphical — and
        // this window already honors its sibling key `[ui] diff`.
        "pane.open" => Efecto::AbrirExterno,
        "pane.edit" => Efecto::EditarExterno,
        "pane.compare-files" => Efecto::CompararFicheros,
        "app.terminal" => Efecto::Terminal,
        "app.handoff" => Efecto::Relevo,
        "app.theme" => Efecto::Tema,
        "app.menu" => Efecto::Menu,
        "profile.pick" => Efecto::PerfilElegir,
        "profile.save-as" => Efecto::PerfilGuardarComo,
        "profile.next" => Efecto::PerfilVecino { atras: false },
        "profile.prev" => Efecto::PerfilVecino { atras: true },
        "pane.select-drive" => Efecto::Volumenes,
        "pane.connect" => Efecto::Conexiones,
        "pane.disconnect" => Efecto::Desconectar,
        "pane.view" => Efecto::Ver,
        "pane.quick-search" => Efecto::BuscarRapido,
        "pane.search" => Efecto::Buscar,
        "pane.mkdir" => Efecto::CrearDirectorio,
        "pane.edit-new" => Efecto::CrearFichero,
        "pane.delete" => Efecto::Borrar { permanente: false },
        "pane.delete-permanent" => Efecto::Borrar { permanente: true },
        "pane.copy" => Efecto::Transferir { mover: false },
        "pane.move" => Efecto::Transferir { mover: true },
        "pane.rename" => Efecto::Renombrar,
        "pane.chmod" => Efecto::Permisos,
        "pane.checksum" => Efecto::Sumas { verificar: false },
        "pane.checksum-verify" => Efecto::Sumas { verificar: true },
        "pane.ai-rename" => Efecto::RenameIa,
        "pane.organize" => Efecto::Organizar,
        "pane.rename-batch" => Efecto::RenameLote,
        "pane.semantic-search" => Efecto::BuscarSemantica,
        "pane.compare-dirs" => Efecto::Comparar,
        "pane.dir-size" => Efecto::TamanoDeDirectorio,
        "pane.pack" => Efecto::Empaquetar,
        "pane.unpack" => Efecto::Desempaquetar,
        "pane.test-archive" => Efecto::ComprobarArchivo,
        "pane.split-file" => Efecto::PartirFichero,
        "pane.combine-files" => Efecto::Juntar,
        "pane.sync-dirs" => Efecto::Sincronizar,
        // #138: the same semantics as a click on the header, and on the
        // FOCUSED slot — the sort belongs to a listing, like the cursor.
        "pane.sort-name" => Efecto::Ordenar(norte_config::SortColumnKey::Name),
        "pane.sort-ext" => Efecto::Ordenar(norte_config::SortColumnKey::Extension),
        "pane.sort-size" => Efecto::Ordenar(norte_config::SortColumnKey::Size),
        "pane.sort-time" => Efecto::Ordenar(norte_config::SortColumnKey::Mtime),
        "pane.refresh" => Efecto::Refrescar,
        "pane.toggle-hidden" => Efecto::AlternarOcultos,
        "pane.names-encoding" => Efecto::CiclarEncoding,
        "pane.mirror" => Efecto::Espejo,
        "pane.sync-nav" => Efecto::EspejoPermanente,
        "pane.mirror-target" => Efecto::EspejoObjetivo,
        "pane.pull" => Efecto::Traer,
        "pane.swap" => Efecto::Intercambiar,
        "pane.history" => Efecto::Historial,
        "pane.hotlist" => Efecto::Hotlist,
        "pane.popular" => Efecto::Populares,
        "pane.history-left" => Efecto::HistorialDeLado { derecha: false },
        "pane.history-right" => Efecto::HistorialDeLado { derecha: true },
        "pane.select-drive-left" => Efecto::VolumenesDeLado { derecha: false },
        "pane.select-drive-right" => Efecto::VolumenesDeLado { derecha: true },
        otro => return board_effect(otro),
    })
}

/// The tail of [`efecto_de`]: what acts on the task BOARD.
///
/// Lives apart because a single function's `match` goes over the line cap,
/// and this is the natural cut: everything above acts on a listing or on
/// what is shown above it; this, on tasks in progress, which are neither one
/// nor the other.
fn board_effect(command: &str) -> Option<Efecto> {
    Some(match command {
        "task.next" => Efecto::TaskVecina { atras: false },
        "task.prev" => Efecto::TaskVecina { atras: true },
        "task.dismiss" => Efecto::DescartarTask,
        "task.cancel" => Efecto::CancelarTask,
        "task.pause" => Efecto::PausarTask,
        "task.retry" => Efecto::ReintentarTask,
        "task.queue" => Efecto::AlternarCola,
        "task.up" => Efecto::MoverEnCola { arriba: true },
        "task.down" => Efecto::MoverEnCola { arriba: false },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything declared implemented has an effect, and vice versa.
    /// Without this, the list and the `match` drift apart and `Availability`
    /// starts lying.
    #[test]
    fn the_list_and_the_effects_cannot_come_apart() {
        for c in IMPLEMENTADOS {
            assert!(
                efecto_de(c, 1).is_some(),
                "{c} is in the list and has no effect"
            );
        }
    }

    /// Same for the viewer screen.
    #[test]
    fn the_viewer_list_and_its_effects_cannot_come_apart() {
        for c in IMPLEMENTADOS_VISOR {
            assert!(
                efecto_visor_de(c, 1).is_some(),
                "{c} is in the viewer list and has no effect"
            );
        }
    }

    /// And the two lists are disjoint: a command in both would mean a key
    /// does two different things depending on the screen with nobody
    /// declaring it.
    #[test]
    fn the_two_screens_share_no_commands() {
        for c in IMPLEMENTADOS_VISOR {
            assert!(
                !IMPLEMENTADOS.contains(c),
                "{c} is declared on both screens"
            );
        }
    }

    /// And everything declared exists in the shared catalogue: a command
    /// invented here would not be bound by any preset.
    #[test]
    fn everything_declared_is_in_the_catalogue() {
        for c in todos() {
            assert!(
                norte_frontend::keymap::CATALOGUE
                    .iter()
                    .any(|d| d.name == c),
                "{c} is not in the shared catalogue"
            );
        }
    }

    /// Read-only removes EXACTLY what the catalogue does not call inert,
    /// and those are the twenty-four the `MUTAN` list used to enumerate by
    /// hand before ADR 0126: deriving them could not change what the window
    /// does.
    #[test]
    fn read_only_removes_what_is_not_inert() {
        let solo_lectura = implementados(Efectos::SoloLectura);
        let mut quitados: Vec<&str> = IMPLEMENTADOS
            .iter()
            .copied()
            .filter(|c| !solo_lectura.contains(c))
            .collect();
        quitados.sort_unstable();
        assert_eq!(
            quitados,
            [
                "app.handoff",
                "app.terminal",
                // #362: opens a SHELL, so in read-only it goes the way of
                // its two neighbors above. A terminal panel in a window
                // that promises not to write would be the widest possible
                // back door: anything can be typed inside it.
                "layout.terminal",
                "pane.ai-rename",
                "pane.checksum",
                "pane.checksum-verify",
                "pane.chmod",
                "pane.combine-files",
                "pane.compare-files",
                "pane.copy",
                "pane.delete",
                "pane.delete-permanent",
                "pane.edit",
                "pane.edit-new",
                "pane.mkdir",
                "pane.move",
                "pane.open",
                "pane.organize",
                "pane.pack",
                "pane.rename",
                "pane.rename-batch",
                "pane.semantic-search",
                "pane.split-file",
                "pane.sync-dirs",
                "pane.unpack",
            ]
        );
        for c in &solo_lectura {
            assert!(inerte(c), "{c} survives read-only without being inert");
        }
    }

    /// The viewer and the dialogs are not filtered in read-only: their lists
    /// are served whole. That is only correct as long as EVERYTHING they
    /// have is inert, and this test requires it — a `viewer.edit` that
    /// launched an editor would otherwise slip into the window that
    /// promised to only look (ADR 0126).
    #[test]
    fn the_viewer_and_the_dialogs_only_have_inert_commands() {
        for c in IMPLEMENTADOS_VISOR.iter().chain(IMPLEMENTADOS_DIALOGO) {
            assert!(
                inerte(c),
                "{c} is not inert and its list is not filtered in read-only"
            );
        }
    }

    /// A name the catalogue does not know is not inert.
    #[test]
    fn the_unknown_is_not_inert() {
        assert!(!inerte("pane.does-not-exist-ever"));
        assert!(inerte("cursor.down"));
    }

    /// The counter multiplies whatever can be repeated.
    #[test]
    fn the_counter_multiplies() {
        assert_eq!(efecto_de("cursor.down", 3), Some(Efecto::Cursor(3)));
        assert_eq!(efecto_de("cursor.up", 3), Some(Efecto::Cursor(-3)));
        assert_eq!(efecto_de("cursor.page-down", 2), Some(Efecto::Pagina(2)));
    }
}
