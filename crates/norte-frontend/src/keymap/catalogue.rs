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

const fn live(name: &'static str, counts: bool) -> CommandDef {
    CommandDef {
        name,
        counts,
        status: Status::Live,
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
const fn planned(name: &'static str, reason: &'static str, issue: u32) -> CommandDef {
    CommandDef {
        name,
        counts: false,
        status: Status::Planned { reason, issue },
    }
}

/// Every command either frontend knows, plus the ones a preset may honestly
/// bind before norte builds them.
pub const CATALOGUE: &[CommandDef] = &[
    // --- app ---
    live("app.quit", false),
    live("app.help", false),
    live("app.theme", false),
    live("app.settings", false),
    live("app.extensions", false),
    // Las sesiones de AGENTE que este cliente ha visto pedir permiso, y el
    // deshacer de una entera (#276). Vive en el catálogo compartido —y no
    // solo en el host gráfico— porque el vocabulario de comandos es UNO: un
    // preset puede atarlo, la ayuda lo documenta, y el frontend que aún no lo
    // implementa lo dice con la misma frase que cualquier otro que no tenga.
    live("app.agents", false),
    live("app.palette", false),
    live("app.menu", false),
    // `--pick` (S2): being in this table only means the NAME is known to the
    // vocabulary (help, palette, rebind checks). No preset binds it — the
    // TUI's run loop decides, per keystroke, whether Enter/Ctrl+Enter means
    // this or `nav.enter`, because that decision needs runtime state
    // (`--pick` was passed, the cursor is on a directory) that a keymap file
    // cannot express. A preset that bound it directly would fire outside
    // `--pick` too, which is exactly what keeping it out of every preset
    // prevents.
    live("app.pick-accept", false),
    // #135 (S4): the TUI builds these by SUSPENDING itself — it hands the
    // whole terminal over and takes it back. `app.terminal` is also live in
    // the GUI, which cannot suspend and launches the desktop's terminal
    // emulator instead; the other two are TUI-only and resolve to
    // `Availability::NotHere` there, which is the truth and is what the
    // reference sheet greys out.
    live("app.terminal", false),
    live("app.toggle-panels", false),
    // --- pane ---
    live("pane.command-line", false),
    live("pane.switch", false),
    // --- pestañas (L1b, cierra #137) ---
    //
    // Los cuatro primeros estaban RESERVADOS aquí como `planned` desde K2, y
    // varios presets ya los ataban: `total-commander` y `krusader` los tenían
    // escritos y en gris. Construirlos con otro nombre habría dejado dos
    // vocabularios para lo mismo y esas teclas muertas para siempre.
    live("pane.tab-new", false),
    live("pane.tab-close", false),
    live("pane.tab-next", false),
    live("pane.tab-prev", false),
    live("pane.tab-move-left", false),
    live("pane.tab-move-right", false),
    live("pane.tab-goto-1", false),
    live("pane.tab-goto-2", false),
    live("pane.tab-goto-3", false),
    live("pane.tab-goto-4", false),
    live("pane.tab-goto-5", false),
    live("pane.tab-goto-6", false),
    live("pane.tab-goto-7", false),
    live("pane.tab-goto-8", false),
    live("pane.tab-goto-9", false),
    // --- layout (L1b) ---
    live("layout.split-h", false),
    live("layout.split-v", false),
    live("layout.focus-next", false),
    live("layout.focus-prev", false),
    live("layout.close-slot", false),
    live("layout.grow", true),
    live("layout.shrink", true),
    live("layout.equalize", false),
    live("layout.set-target", false),
    // L3: el sidebar de sitios. En la familia `layout.*` y no en `pane.*`
    // porque lo que hace es REORGANIZAR la pantalla —acopla un panel nuevo—,
    // no operar sobre un listado.
    live("layout.places", false),
    // L3: el visor acoplado. Mismo motivo para estar en `layout.*`: acopla un
    // panel. Lo que hay DENTRO es el kind `viewer` de siempre.
    live("layout.preview", false),
    // Fase A: el panel de procesos y la hoja de atributos. Sin acorde en
    // ningún preset: quince `layout.*` por siete presets es #228, y ligar dos
    // aquí dejaría la familia a medias sin regla que diga qué mitad. Se
    // alcanzan desde la paleta y desde el menú.
    live("layout.processes", false),
    live("layout.metadata", false),
    // El selector de disposición. Sin acorde por el mismo #228, y además
    // porque el nombre de una disposición NO es el de un preset de teclas
    // aunque coincida: el diálogo lo dice en su pie.
    live("layout.pick", false),
    // Los perfiles (ADR 0079). Sin acorde por el mismo #228 —atar cuatro
    // teclas nuevas en siete presets sin que nadie lo haya pedido es el error
    // contrario al que #228 arregló— y llegando por la paleta y el menú.
    //
    // El selector avisa además de que un nombre de perfil que coincide con
    // una disposición o con un preset de teclas NO es esa otra cosa: son tres
    // ajustes distintos que pueden compartir nombre.
    live("profile.pick", false),
    live("profile.next", false),
    live("profile.prev", false),
    planned("profile.save-as", "cmd-planned-profile-save-as", 306),
    live("pane.mirror", false),
    live("pane.mirror-target", false),
    live("pane.pull", false),
    live("pane.swap", false),
    live("pane.copy", false),
    live("pane.move", false),
    live("pane.delete", false),
    live("pane.delete-permanent", false),
    live("pane.mkdir", false),
    live("pane.rename", false),
    live("pane.refresh", false),
    live("pane.view", false),
    live("pane.open", false),
    live("pane.quick-search", false),
    live("pane.history", false),
    live("pane.hotlist", false),
    // `pane.select-drive*` (2026-08-10-volumes.md, closes #131): the focused
    // pane and the two sides Total Commander's `Alt+F1`/`Alt+F2` name. None
    // is a clamped mover — a drive picker has no count to take.
    live("pane.select-drive", false),
    live("pane.select-drive-left", false),
    live("pane.select-drive-right", false),
    // `pane.compare-dirs` (2026-08-11-directory-comparison.md, roadmap item 1
    // spec 1): compares the two panes and opens the diff pane. `pane.sync-dirs`
    // (2026-08-11-directory-sync.md, spec 2) is the half that WRITES, and it
    // joined it here rather than staying Planned: it is built, and what it
    // needs that comparing does not — a journal, therefore the daemon — is a
    // fact about the SESSION, not about norte. That is `Facts::journalled` in
    // `crate::availability`, which dims it with a reason the reader can act on
    // instead of an issue number nobody can close. Neither takes a count: two
    // whole trees are a task, not a clamped mover (ADR 0044).
    live("pane.compare-dirs", false),
    live("pane.sync-dirs", false),
    live("pane.search", false),
    live("pane.names-encoding", false),
    live("pane.toggle-hidden", false),
    live("pane.columns", false),
    live("pane.ai-rename", false),
    live("pane.semantic-search", false),
    live("pane.copy-path", false),
    // --- cursor (the count-aware family) ---
    live("cursor.up", true),
    live("cursor.down", true),
    live("cursor.page-up", true),
    live("cursor.page-down", true),
    live("cursor.top", false),
    live("cursor.bottom", false),
    // --- nav ---
    live("nav.enter", false),
    live("nav.parent", false),
    live("nav.back", true),
    live("nav.forward", true),
    // --- mark ---
    live("mark.toggle", false),
    live("mark.all", false),
    live("mark.invert", false),
    live("mark.clear", false),
    live("mark.pattern-add", false),
    live("mark.pattern-remove", false),
    // --- task ---
    live("task.cancel", false),
    // Los tres de RECORRER el tablero estuvieron un rato en `Planned`: la
    // matriz de paridad de la fase 6 destapó que la tabla los declaraba vivos
    // sin que los implementara NINGÚN frontend. Vuelven a vivos porque la
    // ventana ya los hace (#292); el TUI sigue atando solo `task.cancel`, que
    // es una asimetría normal —lo que no es normal es que la tabla prometa lo
    // que no hace nadie.
    live("task.next", false),
    live("task.prev", false),
    live("task.dismiss", false),
    // --- viewer ---
    live("viewer.close", false),
    live("viewer.up", true),
    live("viewer.down", true),
    live("viewer.page-up", true),
    live("viewer.page-down", true),
    live("viewer.top", false),
    live("viewer.bottom", false),
    live("viewer.encoding", false),
    live("viewer.encoding-auto", false),
    live("viewer.hex", false),
    // --- dialog ---
    live("dialog.confirm", false),
    live("dialog.cancel", false),
    live("dialog.approve", false),
    live("dialog.deny", false),
    live("dialog.overwrite", false),
    live("dialog.skip", false),
    live("dialog.rename", false),
    live("dialog.newer", false),
    // The four dialog movers are the shape that WOULD take a count, and they
    // declare `false` anyway (ADR 0044, rust-reviewer MAJOR-2): no overlay
    // dispatcher honours one. Every overlay handler resolves against this
    // screen and then resets the resolver on `Resolution::Counting` — the
    // same decision that gives overlays no multi-key sequences — so a count
    // typed over a dialog is destroyed at the digit and can never reach the
    // command. `true` here would be the catalogue claiming a capability
    // nothing implements, which is precisely the drift the shared catalogue
    // exists to end. Flip them the day an overlay learns to repeat.
    live("dialog.up", false),
    live("dialog.down", false),
    live("dialog.page-up", false),
    live("dialog.page-down", false),
    live("dialog.add", false),
    live("dialog.toggle-enabled", false),
    live("dialog.remove", false),
    live("dialog.move-up", false),
    live("dialog.move-down", false),
    live("dialog.sort", false),
    live("dialog.cycle-format", false),
    live("dialog.pane", false),
    live("dialog.back", false),
    live("dialog.filter", false),
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
    live("pane.pack", false),
    live("pane.unpack", false),
    live("pane.test-archive", false),
    live("pane.split-file", false),
    live("pane.combine-files", false),
    // #133: norte no trae editor —lo suyo es el gestor— y F4 abre el TUYO,
    // que es lo que hacen los cuatro presets al atarlo.
    live("pane.edit", false),
    live("pane.edit-new", false),
    // #134's second half (`pane.sync-dirs`) left this block and is `live`
    // above; `keymap-reason-sync` went with it, out of both locales, because
    // nothing else claimed it — same disposal as `keymap-reason-shell` when
    // S4 shipped, recorded below.
    // The three of issue #135 left this block in S4 and are `live` above; the
    // family's reason id (`keymap-reason-shell`) went with them, out of both
    // locales, because nothing else claimed it.
    // #136: el árbol de directorios, acoplado a la izquierda del listado.
    live("pane.tree", false),
    // Orden por tecla (#138). `sort-menu` no abre un menú propio: abre el
    // diálogo de columnas, que es donde vive el orden desde #108 —tiene la
    // columna, la dirección y `dirs_first` en un sitio— y así no hay dos
    // pantallas que digan lo mismo con distinta letra.
    live("pane.sort-name", false),
    live("pane.sort-ext", false),
    live("pane.sort-size", false),
    live("pane.sort-time", false),
    live("pane.sort-menu", false),
    // #139: las propiedades salen del listado; el tamaño de una carpeta se
    // CUENTA, y por eso es una Task cancelable y no un campo del diálogo.
    live("pane.properties", false),
    live("pane.dir-size", false),
    // #140: abrir es elegir de `connections.toml`; desconectar SUELTA la
    // sesión de verdad, no solo se va del panel.
    live("pane.connect", false),
    live("pane.disconnect", false),
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
    /// are here on a second bound: `nav::HISTORY_MAX` caps the trail at 30
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
                "viewer.page-down",
                "viewer.page-up",
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

    #[test]
    fn lookup_encuentra_y_falla_bien() {
        assert!(lookup("pane.copy").is_some());
        assert!(lookup("pane.no-existe-jamas").is_none());
    }
}
