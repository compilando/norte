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
/// assert_eq!(effect("pane.no-existe-jamas"), None);
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

/// Un comando que un preset puede nombrar honestamente y que norte todavía
/// no hace.
///
/// **Ahora mismo no hay ninguno**, y el constructor se queda por lo que
/// costó descubrirlo: con #132 la tabla se quedó sin `Planned`, y luego la
/// matriz de paridad de la fase 6 destapó tres (`task.next`/`prev`/
/// `dismiss`) declarados VIVOS sin que los implementara ningún frontend —
/// que es peor, porque una tecla así no hace nada y tampoco dice por qué.
/// Esta maquinaria (fila atenuada, motivo traducido, número de issue) es la
/// respuesta a eso, y reconstruirla costaría más que dejarla.
#[allow(dead_code, reason = "el vocabulario está entero: ver el doc de arriba")]
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
    // Las sesiones de AGENTE que este cliente ha visto pedir permiso, y el
    // deshacer de una entera (#276). Vive en el catálogo compartido —y no
    // solo en el host gráfico— porque el vocabulario de comandos es UNO: un
    // preset puede atarlo, la ayuda lo documenta, y el frontend que aún no lo
    // implementa lo dice con la misma frase que cualquier otro que no tenga.
    live("app.agents", false, Inert),
    live("app.palette", false, Inert),
    // «Ir a cualquier sitio» (fase 6 del programa WOW): una pantalla sobre
    // lo que ya había repartido en cinco —historia, populares, favoritos,
    // conexiones y la paleta— más una ruta tecleada y lo que encuentre el
    // índice. No sustituye a ninguna: cada una sigue teniendo su tecla, y
    // ésta es la que sirve cuando no sabes en cuál de las cinco está.
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
    // Fase 9: el RELEVO entre frontends. Vive aquí y no en un preset porque
    // lo que decide si se puede hacer es estado de ejecución —hay daemon,
    // hay a dónde abrir una ventana— que un fichero de keymap no sabe
    // expresar; la disponibilidad lo dice con su motivo.
    live("app.handoff", false, Launches),
    // --- pane ---
    live("pane.command-line", false, Launches),
    live("pane.switch", false, Inert),
    // --- pestañas (L1b, cierra #137) ---
    //
    // Los cuatro primeros estaban RESERVADOS aquí como `planned` desde K2, y
    // varios presets ya los ataban: `total-commander` y `krusader` los tenían
    // escritos y en gris. Construirlos con otro nombre habría dejado dos
    // vocabularios para lo mismo y esas teclas muertas para siempre.
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
    // ADR 0138: girar el reparto del panel con foco. Sin tecla en ningún
    // preset a propósito: vive en el botón de disposición, el menú y la
    // paleta, y cada preset lo dice en su cabecera.
    live("layout.flip", false, Inert),
    live("layout.set-target", false, Inert),
    // L3: el sidebar de sitios. En la familia `layout.*` y no en `pane.*`
    // porque lo que hace es REORGANIZAR la pantalla —acopla un panel nuevo—,
    // no operar sobre un listado.
    live("layout.places", false, Inert),
    // L3: el visor acoplado. Mismo motivo para estar en `layout.*`: acopla un
    // panel. Lo que hay DENTRO es el kind `viewer` de siempre.
    live("layout.preview", false, Inert),
    // Fase A: el panel de procesos y la hoja de atributos. Sin acorde en
    // ningún preset: quince `layout.*` por siete presets es #228, y ligar dos
    // aquí dejaría la familia a medias sin regla que diga qué mitad. Se
    // alcanzan desde la paleta y desde el menú.
    live("layout.processes", false, Inert),
    live("layout.metadata", false, Inert),
    // El registro (#323). Este SÍ lleva acorde en los siete, a diferencia de
    // sus vecinos: lo que se abre aquí es lo que explica por qué acaba de
    // fallar algo, y buscarlo en la paleta justo cuando algo va mal es pedirle
    // al lector el paso de más en el peor momento. `alt+l` estaba libre en los
    // siete presets.
    live("layout.log", false, Inert),
    // El mapa de disco (fase 4). Lleva acorde en los siete por lo mismo que el
    // registro: es un panel que se queda el TECLADO —se anda por los
    // rectángulos y se entra en uno—, y a un panel así hay que poder entrar y
    // salir sin ratón. Eso lo exige a máquina el test
    // `los_paneles_con_teclado_se_abren_y_se_recorren_en_los_siete_presets`.
    //
    // `alt+z`, y el mnemónico es malo a propósito: era la ÚNICA letra libre en
    // los siete. De la `a` a la `y` no queda ninguna sin atar en algún preset
    // —`alt+d` es `pane.disconnect` en orthodox y krusader, `alt+m` el menú,
    // `alt+j` procesos, `alt+l` el registro—, así que o era esta o era un
    // acorde que ya significa otra cosa en el gestor que alguien viene
    // imitando. Una tecla rara se aprende; una que hace dos cosas, no.
    live("layout.disk-map", false, Inert),
    // La línea de tiempo del journal (fase 7): qué se ha hecho y hasta dónde
    // se puede volver. SIN tecla en ningún preset, y es deliberado: el
    // espacio de `alt+<letra>` para paneles está agotado —b, j, l, t, z ya
    // son otros— y no queda ninguna libre en los siete a la vez. Atarla en
    // tres y en cuatro no es peor que no atarla en ninguno: es una capacidad
    // que la mitad de los lectores no tendría y que nadie les diría por qué.
    // Se llega por la barra de paneles, que se genera del registro de kinds y
    // por tanto la tiene en LOS SIETE, y por el menú Ver.
    live("layout.timeline", false, Inert),
    // El selector de disposición. Sin acorde por el mismo #228, y además
    // porque el nombre de una disposición NO es el de un preset de teclas
    // aunque coincida: el diálogo lo dice en su pie.
    live("layout.pick", false, Inert),
    // Los perfiles (ADR 0079). Sin acorde por el mismo #228 —atar cuatro
    // teclas nuevas en siete presets sin que nadie lo haya pedido es el error
    // contrario al que #228 arregló— y llegando por la paleta y el menú.
    //
    // El selector avisa además de que un nombre de perfil que coincide con
    // una disposición o con un preset de teclas NO es esa otra cosa: son tres
    // ajustes distintos que pueden compartir nombre.
    live("profile.pick", false, Inert),
    live("profile.next", false, Inert),
    live("profile.prev", false, Inert),
    live("profile.save-as", false, Inert),
    live("pane.mirror", false, Inert),
    live("pane.mirror-target", false, Inert),
    // El espejo PERMANENTE: mientras está puesto, cada navegación del panel
    // con foco la repite el otro. Es un interruptor, no un gesto — por eso no
    // se llama `pane.mirror-mode`: lo que se enciende no es un espejo, es que
    // los dos paneles andan juntos. Y nada que ver con `pane.sync-dirs`, que
    // ESCRIBE ficheros.
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
    // La historia entera (spec 2026-09-15, fase 1). `pane.popular` es la lista
    // de la sesión ordenada por visitas (Krusader `Ctrl+Z`); `-left`/`-right`
    // nombran un LADO, como los volúmenes. Ninguno lleva contador: son listas.
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
    // #312: la PAREJA, que es otra pregunta que comparar dos árboles. Se
    // delega en el programa de `[ui] diff`, así que lo que norte decide es el
    // operando —dos ficheros, o lo dice— y no el formato de la diferencia.
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
    // El punto de salto de Krusader (`Ctrl+J`). Sin contador: hay UN punto, y
    // saltar a él cinco veces es saltar a él.
    live("nav.jump-back", false, Inert),
    live("nav.set-jump-point", false, Inert),
    // --- mark ---
    live("mark.toggle", false, Inert),
    live("mark.all", false, Inert),
    live("mark.invert", false, Inert),
    live("mark.clear", false, Inert),
    live("mark.pattern-add", false, Inert),
    live("mark.pattern-remove", false, Inert),
    // #313: los tres huecos que Total Commander tiene en su familia Gray y
    // norte no tenía. `extension-*` actúa sobre la extensión de la entrada
    // BAJO EL CURSOR; `files`/`dirs` son aditivos como `pattern-add`; y
    // `restore` devuelve la selección de antes del último gesto en bloque,
    // que es la red del que pulsó «desmarcar todo» sin querer.
    live("mark.extension-add", false, Inert),
    live("mark.extension-remove", false, Inert),
    live("mark.files", false, Inert),
    live("mark.dirs", false, Inert),
    live("mark.restore", false, Inert),
    // Marcar MOVIÉNDOSE, que es la mitad de la familia que faltaba: `space` e
    // `insert` marcan bajando y no había nada para subir ni para un tramo.
    // `toggle-up` es el espejo exacto de `mark.toggle`; los dos `page-*`
    // aplican a todo el tramo lo contrario de lo que tenga la fila del cursor,
    // que es lo que hace el gesto reversible; y `to-top`/`to-bottom` son el
    // `Shift+Home`/`Shift+End` de Krusader, que además DESMARCAN el otro lado
    // — eso es literal de su documentación y es lo que los distingue de
    // «añade un tramo».
    live("mark.toggle-up", false, Inert),
    live("mark.toggle-page-down", false, Inert),
    live("mark.toggle-page-up", false, Inert),
    live("mark.to-top", false, Inert),
    live("mark.to-bottom", false, Inert),
    // --- task ---
    live("task.cancel", false, Inert),
    // Pausa y reanuda la misma tarea que cancelaría (ADR 0147).
    live("task.pause", false, Inert),
    // Relanza la transferencia fallida (ADR 0148).
    live("task.retry", false, Inert),
    // La cola en serie (ADR 0149): el interruptor y el reordenado.
    live("task.queue", false, Inert),
    live("task.up", false, Inert),
    live("task.down", false, Inert),
    // Los tres de RECORRER el tablero estuvieron un rato en `Planned`: la
    // matriz de paridad de la fase 6 destapó que la tabla los declaraba vivos
    // sin que los implementara NINGÚN frontend. Vuelven a vivos porque la
    // ventana ya los hace (#292); el TUI sigue atando solo `task.cancel`, que
    // es una asimetría normal —lo que no es normal es que la tabla prometa lo
    // que no hace nadie.
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
    // A lo ANCHO. Con cuenta, como sus gemelas verticales: el visor no
    // envuelve, así que una línea larga se recorre igual que un fichero alto.
    live("viewer.left", true, Inert),
    live("viewer.right", true, Inert),
    live("viewer.encoding", false, Inert),
    live("viewer.encoding-auto", false, Inert),
    live("viewer.hex", false, Inert),
    // El ZOOM de una imagen (spec 2026-09-20). Sin cuenta: `3` delante de
    // «acercar» leería como «tres peldaños», y el peldaño ya es la unidad —
    // pulsar tres veces es exactamente eso y se ve mientras ocurre.
    live("viewer.zoom-in", false, Inert),
    live("viewer.zoom-out", false, Inert),
    live("viewer.zoom-fit", false, Inert),
    // Las HERMANAS del listado: pasar a la foto siguiente sin salir del visor.
    // `Inert` como `pane.view`, que es lo mismo que hacen —abrir para LEER—, y
    // sin cuenta por el mismo motivo que el zoom: el peldaño ya es la unidad y
    // pulsar tres veces se ve mientras ocurre.
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
    // Saltar de sección en una página de texto (la ayuda).
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
    // Las listas de historia (spec 2026-09-15 D2): abrir lo elegido en el
    // OTRO panel sin mover el foco, y vaciar la lista entera.
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
    // #132: los cinco, construidos. Ninguno escribe DENTRO de un contenedor
    // —el provider de archivos sigue read-only, ADR 0018—: empaquetar, partir
    // y juntar fabrican ficheros nuevos, comprobar solo lee, y desempaquetar
    // es la copia de siempre desde el interior del archivo.
    live("pane.pack", false, Writes),
    live("pane.unpack", false, Writes),
    live("pane.test-archive", false, Inert),
    live("pane.split-file", false, Writes),
    live("pane.combine-files", false, Writes),
    // #133: norte no trae editor —lo suyo es el gestor— y F4 abre el TUYO,
    // que es lo que hacen los cuatro presets al atarlo.
    live("pane.edit", false, Launches),
    live("pane.edit-new", false, Writes),
    // #134's second half (`pane.sync-dirs`) left this block and is `live`
    // above; `keymap-reason-sync` went with it, out of both locales, because
    // nothing else claimed it — same disposal as `keymap-reason-shell` when
    // S4 shipped, recorded below.
    // The three of issue #135 left this block in S4 and are `live` above; the
    // family's reason id (`keymap-reason-shell`) went with them, out of both
    // locales, because nothing else claimed it.
    // #136: el árbol de directorios, acoplado a la izquierda del listado.
    live("pane.tree", false, Inert),
    // Orden por tecla (#138). `sort-menu` no abre un menú propio: abre el
    // diálogo de columnas, que es donde vive el orden desde #108 —tiene la
    // columna, la dirección y `dirs_first` en un sitio— y así no hay dos
    // pantallas que digan lo mismo con distinta letra.
    live("pane.sort-name", false, Inert),
    live("pane.sort-ext", false, Inert),
    live("pane.sort-size", false, Inert),
    live("pane.sort-time", false, Inert),
    live("pane.sort-menu", false, Inert),
    // #139: las propiedades salen del listado; el tamaño de una carpeta se
    // CUENTA, y por eso es una Task cancelable y no un campo del diálogo.
    live("pane.properties", false, Inert),
    // #314: la única categoría en la que los tres gestores de referencia TOCAN
    // y norte solo miraba. Es una mutación entera —journal con reversa,
    // política— y por eso vive aquí y no dentro del cuadro de propiedades.
    live("pane.chmod", false, Writes),
    live("pane.dir-size", false, Inert),
    live("pane.checksum", false, ReadsContent),
    live("pane.checksum-verify", false, ReadsContent),
    // #140: abrir es elegir de `connections.toml`; desconectar SUELTA la
    // sesión de verdad, no solo se va del panel.
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
/// // #132 construyó el último `Planned` del vocabulario: hoy no queda
/// // ninguno, y `pane.pack` es `Live` como todo lo demás que un preset ata.
/// assert_eq!(lookup("pane.pack").map(|d| d.status), Some(Status::Live));
/// assert_eq!(lookup("pane.select-drive").map(|d| d.status), Some(Status::Live));
/// assert_eq!(lookup("pane.compare-dirs").map(|d| d.status), Some(Status::Live));
/// assert_eq!(lookup("pane.sync-dirs").map(|d| d.status), Some(Status::Live));
/// assert!(lookup("pane.no-existe-jamas").is_none());
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
    fn no_hay_nombres_duplicados() {
        let mut names: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "nombre duplicado en CATALOGUE");
    }

    /// A `Planned` entry with an empty reason or a zero issue is a promise
    /// nobody can chase — the exact failure this state exists to prevent.
    #[test]
    fn todo_planned_tiene_motivo_e_issue() {
        for d in CATALOGUE {
            if let Status::Planned { reason, issue } = d.status {
                assert!(!reason.is_empty(), "{} sin motivo", d.name);
                assert!(issue > 0, "{} sin issue", d.name);
            }
        }
    }

    /// A reason id with no Fluent message renders as the raw id — an unbuilt
    /// key would then explain itself with `keymap-reason-...`, which is worse
    /// than saying nothing. Pin both locales.
    #[test]
    fn todo_motivo_planned_esta_traducido_en_ambos_locales() {
        for d in CATALOGUE {
            if let Status::Planned { reason, .. } = d.status {
                for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                    let s = norte_i18n::t_in(lang, reason);
                    assert_ne!(s, reason, "{} sin traducir en {lang:?}", d.name);
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
    fn el_conjunto_con_contador_es_exactamente_este() {
        let mut con_contador: Vec<&str> = CATALOGUE
            .iter()
            .filter(|d| d.counts)
            .map(|d| d.name)
            .collect();
        con_contador.sort_unstable();
        assert_eq!(
            con_contador,
            [
                "cursor.down",
                "cursor.page-down",
                "cursor.page-up",
                "cursor.up",
                // Un contador REPITE el despacho (ADR 0044), así que «3
                // agrandar» agranda tres veces. Es la misma lectura que
                // `cursor.down`, no una excepción.
                "layout.grow",
                "layout.shrink",
                "nav.back",
                "nav.forward",
                "viewer.down",
                // Los dos del eje horizontal, por lo mismo que sus gemelas
                // verticales: acotados a la línea más larga, en memoria, sin
                // task ni pantalla nueva.
                "viewer.left",
                "viewer.page-down",
                "viewer.page-up",
                "viewer.right",
                "viewer.up",
            ],
            "un comando ganó o perdió `counts`: ver ADR 0044 antes de tocar esta lista"
        );
    }

    /// A reason id is the name of ONE capability, so it must name ONE issue.
    /// K2b adds twenty-six Planned entries in nine families, transcribed by
    /// hand: a family whose issue number drifts on one line would send a user
    /// to the wrong tracker and nothing else would notice.
    #[test]
    fn cada_motivo_apunta_a_un_solo_issue() {
        let mut vistos: Vec<(&str, u32)> = Vec::new();
        for d in CATALOGUE {
            if let Status::Planned { reason, issue } = d.status {
                if let Some(&(_, otro)) = vistos.iter().find(|(r, _)| *r == reason) {
                    assert_eq!(
                        otro, issue,
                        "{reason} apunta a #{otro} y a #{issue} ({})",
                        d.name
                    );
                } else {
                    vistos.push((reason, issue));
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
    fn el_conjunto_que_no_es_inerte_es_exactamente_este() {
        use super::Effect::{Destroys, Launches, ReadsContent, SendsOut, Writes};
        let mut actuan: Vec<(&str, super::Effect)> = CATALOGUE
            .iter()
            .filter(|d| !d.effect.is_inert())
            .map(|d| (d.name, d.effect))
            .collect();
        actuan.sort_unstable_by_key(|(n, _)| *n);
        assert_eq!(
            actuan,
            [
                ("app.handoff", Launches),
                ("app.terminal", Launches),
                ("app.toggle-panels", Launches),
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
            "un comando cambió de efecto: ver ADR 0126 antes de tocar esta lista"
        );
    }

    #[test]
    fn lookup_encuentra_y_falla_bien() {
        assert!(lookup("pane.copy").is_some());
        assert!(lookup("pane.no-existe-jamas").is_none());
    }
}
