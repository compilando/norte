//! Estado puro del TUI (panes, cursor, presentación de nombres) y la
//! presentación de ERRORES para la barra (#73): categorías Fluent
//! ([`error_key`]/[`error_category`] y compañía) + saneado de detalle
//! ([`detail_for_bar`]). Máquina testeable sin terminal — el render (`ui`)
//! y el I/O (`main`) viven aparte; los scripts Lua (M4) consumen de aquí la
//! clave ESTABLE de [`error_key`].

use norte_i18n::{t, ta};
use norte_proto::{Entry, EntryKind, Error, VPath};

/// Un panel: directorio actual y sus entradas YA ordenadas.
///
/// La mecánica PURA de un pane —directorio, entradas, cursor, `loading` y
/// quick search— vive UNA sola vez en [`norte_frontend::PaneState`],
/// compartida con la GUI (#82): el `Pane` de la TUI la EMBEBE en su campo
/// `state` (privado) y delega en ella
/// (`dir`/`entries`/`cursor`/`selected`/`move_*`/`quick_*`…). Lo propio de la
/// TUI —la búsqueda viva (`virtual_search`, `search_state`, `search_error`) y
/// el fill paginado (ADR 0017, [`Pane::extend_listing`] y compañía)— se queda
/// aquí, encima de ese estado.
#[derive(Debug)]
pub struct Pane {
    /// Estado no-render compartido (cursor + quick + listado). Privado: se
    /// accede por los delegados ([`Pane::dir`], [`Pane::entries`], …) para que
    /// el invariante del cursor lo custodie `PaneState`.
    state: norte_frontend::PaneState,
    /// El pane muestra los HITS de una búsqueda viva (`Alt+F7`, liveSearch),
    /// no un listado de directorio real: `dir` es la RAÍZ del walk y las
    /// `entries` son los resultados que van llegando por streaming
    /// ([`Pane::extend_listing`], reusando el molde de paginación). Con él la
    /// barra de estado pinta `search-status-*` en vez del `pos/total` normal;
    /// cualquier `cd`/refresh normal lo apaga (los listados reales lo ponen a
    /// `false`). `F5`/`F8`/`F3` operan sobre el hit bajo el cursor SOLOS
    /// ([`Pane::selected`] da la `Entry` con su `VPath` completo).
    pub virtual_search: bool,
    /// Estado de presentación de la búsqueda viva (solo significativo con
    /// [`Pane::virtual_search`]): decide qué variante `search-status-*` pinta
    /// la barra. El run loop lo actualiza al llegar el estado terminal.
    pub search_state: SearchState,
    /// Categoría del error de una búsqueda que FALLÓ (`SearchState::Failed`),
    /// ya localizada y saneada: la barra la pinta de forma PERSISTENTE
    /// (`search-status-failed`) tras limpiarse `App::message` — un fallo no
    /// puede degradar a «done» en la siguiente tecla (review MINOR-2).
    pub search_error: Option<String>,
    /// Contexto del match de contenido por hit de la búsqueda viva (#81):
    /// `path → (línea, preview YA saneado en origen)`. Solo significativo con
    /// [`Pane::virtual_search`]; la barra lo pinta para el hit bajo el
    /// cursor. Se limpia al salir del modo virtual (cd/listado real).
    pub search_matches: std::collections::HashMap<VPath, norte_proto::methods::MatchInfo>,
    /// Preferencia de ocultos del USUARIO (#107): el pane virtual de
    /// búsqueda SUSPENDE el filtro (un hit es una petición EXPLÍCITA — un
    /// `.env` buscado que desapareciera en silencio bajo `[ui] show_hidden
    /// = false` es el MAJOR-1 del review), y al volver a un listado real se
    /// restaura esto. Un Ctrl+H DENTRO del pane virtual actúa sobre los
    /// resultados pero no toca la preferencia.
    show_hidden_pref: bool,
}

/// Estado de presentación de una búsqueda viva (`Alt+F7`, liveSearch T6): el
/// run loop lo refleja en [`Pane::search_state`] para que la barra elija la
/// variante `search-status-*`. `Failed` no se pinta en la barra del pane (el
/// error concreto viaja por [`App::message`] vía `error_message`); se
/// conserva la variante por completitud del estado del run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchState {
    /// El walker sigue emitiendo hits.
    #[default]
    Running,
    /// Terminó y no se alcanzó el tope de hits.
    Done,
    /// Terminó por alcanzar `max_hits` (resultados posiblemente incompletos).
    Truncated,
    /// El usuario canceló (los hits ya llegados se conservan).
    Cancelled,
    /// La Task de búsqueda falló (el error va por la barra de mensajes).
    Failed,
}

impl Pane {
    /// Pane sobre `dir` con `entries`: #54, ya no hace falta ordenarlas antes
    /// — [`norte_frontend::PaneState::new`] normaliza internamente (dirs
    /// primero, NFC, empate por bytes, ver [`sort_entries`]).
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        Self {
            state: norte_frontend::PaneState::new(dir, entries),
            virtual_search: false,
            search_state: SearchState::Running,
            search_error: None,
            search_matches: std::collections::HashMap::new(),
            show_hidden_pref: true,
        }
    }

    /// Cicla la reinterpretación de nombres (#57): delegado puro — la
    /// mecánica (sugerencia, vuelta completa del ciclo, re-pliegue del quick
    /// vivo) vive en [`norte_frontend::PaneState::cycle_name_encoding`]
    /// (#98/m2: la GUI la reusa tal cual).
    pub fn cycle_name_encoding(&mut self) -> Option<&'static str> {
        self.state.cycle_name_encoding()
    }

    /// Reinterpretación de nombres activa (#57), para render.
    #[must_use]
    pub fn name_encoding(&self) -> Option<norte_encoding::NameEncoding> {
        self.state.name_encoding()
    }

    // --- Delegados de solo-lectura sobre el estado compartido (#82) ---

    /// Directorio listado.
    #[must_use]
    pub fn dir(&self) -> &VPath {
        self.state.dir()
    }

    /// Entradas ordenadas ([`sort_entries`]).
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        self.state.entries()
    }

    /// Índice bajo el cursor real (0 incluso con lista vacía).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.state.cursor()
    }

    /// El listado se está RELLENANDO en background (paginación, ADR 0017): la
    /// primera página ya se pintó y llegan más entradas. La UI lo marca — un
    /// listado incompleto JAMÁS es silencioso.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.state.loading()
    }

    /// Quick search vivo (`/`, spec 2026-07-18) para el render; `None` =
    /// navegación normal.
    #[must_use]
    pub fn quick(&self) -> Option<&crate::nav::QuickSearch> {
        self.state.quick()
    }

    /// La entrada seleccionada: con quick search en modo Filter, la
    /// selección DENTRO del filtro (así F5/F8/F3… operan sobre lo filtrado
    /// sin que cada comando sepa del quick search — feed-to-listbox); si el
    /// filtro no tiene matches, `None` (las ops no-opean, jamás actúan
    /// sobre una entrada que el usuario no ve). Sin filtro (o en Jump, que
    /// mueve el cursor real), la entrada bajo el cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        self.state.selected()
    }

    /// Índices REALES visibles bajo el filtro; `None` = sin filtro (quick
    /// inactivo, o modo Jump: el listado se pinta entero).
    #[must_use]
    pub fn quick_visible(&self) -> Option<&[usize]> {
        self.state.quick_visible()
    }

    // --- Delegados de mutación de cursor + quick (#82) ---

    /// Arranca el quick search (`/`) en `mode` sobre las entries actuales.
    pub fn quick_start(&mut self, mode: crate::nav::Mode) {
        self.state.quick_start(mode);
    }

    /// Un carácter tecleado con el quick search activo.
    pub fn quick_char(&mut self, c: char) {
        self.state.quick_char(c);
    }

    /// Backspace con el quick search activo.
    pub fn quick_backspace(&mut self) {
        self.state.quick_backspace();
    }

    /// Selección del quick search una posición abajo.
    pub fn quick_down(&mut self) {
        self.state.quick_down();
    }

    /// Selección del quick search una posición arriba.
    pub fn quick_up(&mut self) {
        self.state.quick_up();
    }

    /// Siguiente match con wrap (Tab en modo Jump).
    pub fn quick_next(&mut self) {
        self.state.quick_next();
    }

    /// Cierra el quick search SIN tocar el cursor real: en Filter el listado
    /// completo vuelve con el cursor donde estaba (el filtro nunca lo movió
    /// — test del plan); en Jump el cursor se queda donde saltó.
    pub fn quick_cancel(&mut self) {
        self.state.quick_cancel();
    }

    /// Cierra el quick search fijando el cursor REAL a la selección (Enter:
    /// la op siguiente parte de ahí). Devuelve `true` si el cursor apunta a
    /// una entrada que el usuario VEÍA: en Filter sin matches devuelve
    /// `false` (la lista pintada estaba vacía — jamás despachar sobre una
    /// entrada invisible); en Jump sin matches devuelve `true` si hay
    /// entradas (el listado se pinta ENTERO: el cursor real es visible por
    /// definición — edge de T4, observación del reviewer).
    pub fn quick_confirm(&mut self) -> bool {
        self.state.quick_confirm()
    }

    /// Sube el cursor `n` posiciones (con tope en 0).
    pub fn move_up(&mut self, n: usize) {
        self.state.page_up(n);
    }

    /// Baja el cursor `n` posiciones (con tope en la última entrada).
    pub fn move_down(&mut self, n: usize) {
        self.state.page_down(n);
    }

    /// Cursor a la primera entrada.
    pub fn move_to_start(&mut self) {
        self.state.home();
    }

    /// Cursor a la última entrada.
    pub fn move_to_end(&mut self) {
        self.state.end();
    }

    /// Fija el cursor REAL a `i` (con tope en la última entrada): re-anclar
    /// tras localizar un índice concreto, p. ej. un hit de búsqueda.
    pub fn set_cursor(&mut self, i: usize) {
        self.state.set_cursor(i);
    }

    /// Foco pendiente (spec 2026-07-24 §S1, `nav.parent`): el próximo
    /// [`Pane::begin_listing`] selecciona `child` si aparece en el listado
    /// nuevo, por delante de la memoria de cursor. Ver
    /// [`norte_frontend::PaneState::set_pending_focus`].
    pub fn set_pending_focus(&mut self, child: VPath) {
        self.state.set_pending_focus(child);
    }

    /// Descarta un foco pendiente sin consumirlo (revisión S, M2). Ver
    /// [`norte_frontend::PaneState::clear_pending_focus`].
    pub fn clear_pending_focus(&mut self) {
        self.state.clear_pending_focus();
    }

    // --- Listado + búsqueda viva (propio de la TUI, encima del estado) ---

    /// Marca (o desmarca) el flag de carga de un fill paginado (ADR 0017).
    pub fn set_loading(&mut self, loading: bool) {
        self.state.set_loading(loading);
    }

    /// Arranca el pane virtual de una búsqueda viva (`Alt+F7`, liveSearch T6):
    /// `root` es la raíz del walk, las entries empiezan vacías y los hits
    /// entran por [`Pane::extend_listing`] como un listado paginado. Marca el
    /// pane como virtual (la barra pinta `search-status-running`) y mata
    /// cualquier quick search vivo (filtraba OTRA cosa).
    pub fn begin_search(&mut self, root: VPath) {
        // #107: los hits son EXPLÍCITOS — el filtro de ocultos se suspende
        // en el pane virtual (la preferencia queda en `show_hidden_pref`).
        self.state.set_show_hidden(true);
        self.state.set_listing(root, Vec::new());
        self.virtual_search = true;
        self.search_state = SearchState::Running;
        self.search_error = None;
        // #81 (MAJOR-4 del review): re-lanzar Alt+F7 sin cd de por medio no
        // debe arrastrar previews de la búsqueda ANTERIOR (un hit de la
        // query B solo-nombre pintaría el :línea de la query A) ni crecer
        // el mapa sin límite entre búsquedas.
        self.search_matches.clear();
    }

    /// Reemplaza el contenido tras un cd/refresh, reseteando el cursor.
    /// Un quick search vivo muere: filtraba OTRO listado.
    pub fn set_listing(&mut self, dir: VPath, entries: Vec<Entry>) {
        // #107: al volver a un listado real, la preferencia de ocultos del
        // usuario vuelve a mandar ANTES de ingerir (el filtro se aplica al
        // entrar el listado).
        self.state.set_show_hidden(self.show_hidden_pref);
        self.state.set_listing(dir, entries);
        self.virtual_search = false;
        self.search_matches.clear();
        self.state.set_skipped(None);
    }

    /// Primera página de un listado paginado: reemplaza el contenido y MARCA
    /// que faltan entradas por llegar (ADR 0017). El drenador irá llamando a
    /// [`Pane::extend_listing`] y, al terminar, [`Pane::finish_listing`].
    /// `skipped` = omitidas del contenedor (#93), del open del listado.
    ///
    /// Punto de captura de la memoria de cursor (spec §S1) para la TUI: a
    /// diferencia de la GUI (que tiene una fase `begin_loading` optimista
    /// ANTES del fetch async), la TUI espera el listado ENTERO antes de
    /// tocar el pane (`cd` en `main.rs` no llama a
    /// [`norte_frontend::PaneState::begin_loading`] — este método es el
    /// único punto donde `self.state` todavía refleja el dir VIEJO). Grabar
    /// aquí, antes de `set_listing`, es el equivalente exacto.
    pub fn begin_listing(
        &mut self,
        dir: VPath,
        first_page: Vec<Entry>,
        more: bool,
        skipped: Option<u64>,
    ) {
        self.state.remember_cursor();
        // #107: mismo restablecimiento que `set_listing` — este es el cd
        // real paginado de la TUI.
        self.state.set_show_hidden(self.show_hidden_pref);
        self.state.set_listing(dir, first_page);
        self.state.set_loading(more);
        self.virtual_search = false;
        self.search_matches.clear();
        self.state.set_skipped(skipped);
    }

    /// Añade un lote del drenador: re-ordena TODO y re-ancla el cursor al path
    /// que estaba seleccionado (si desapareció del re-orden, clamp por índice)
    /// para que rellenar no mueva la selección del usuario bajo sus pies. Un
    /// quick search vivo se RE-APLICA sobre el listado nuevo (spec: el filtro
    /// no se congela mientras el fill sigue), conservando su selección por
    /// path. La mecánica pura vive en [`norte_frontend::PaneState::extend`].
    pub fn extend_listing(&mut self, batch: Vec<Entry>) {
        self.state.extend(batch);
    }

    /// Hidrata size/mtime de la entrada `path` con el resultado de una sonda
    /// de stat on-focus (#52, listado lazy). No reordena; no-op si la entrada
    /// ya no está. Delegado puro a [`norte_frontend::PaneState::hydrate`].
    pub fn hydrate(&mut self, path: &VPath, size: Option<u64>, mtime_ms: Option<i64>) {
        self.state.hydrate(path, size, mtime_ms);
    }

    /// El drenador terminó: el listado ya está completo. El quick search se
    /// re-aplica por contrato (hoy no muta entries: refresh barato; si algún
    /// día el cierre re-sortea, el filtro no se queda con índices muertos).
    pub fn finish_listing(&mut self) {
        self.state.set_loading(false);
        self.state.refresh_quick();
    }

    /// Listado COMPLETO nuevo del MISMO dir (refresh tras una mutación):
    /// cursor conservado por ÍNDICE con clamp (tras un delete queda en la
    /// siguiente entrada — semántica ortodoxa) y quick search re-aplicado
    /// por path (los índices del listado viejo no identifican nada).
    pub fn refresh_listing(&mut self, entries: Vec<Entry>) {
        self.state.refill(entries);
        self.state.set_loading(false);
        self.virtual_search = false;
        self.search_matches.clear();
    }

    /// Omitidas del contenedor del listado actual (#93/#96): delegado puro a
    /// [`norte_frontend::PaneState::skipped`]. La barra pinta `Some(n)`, n>0.
    #[must_use]
    pub fn skipped(&self) -> Option<u64> {
        self.state.skipped()
    }

    /// Fija las omitidas frescas (#96) — ver `PaneState::set_skipped`.
    pub fn set_skipped(&mut self, skipped: Option<u64>) {
        self.state.set_skipped(skipped);
    }

    /// Decoración de plugin de `path` (G3b, ADR 0037) — delegado puro a
    /// [`norte_frontend::PaneState::decoration_for`]. El render la pinta
    /// como badge tras el hueco del badge hostil.
    #[must_use]
    pub fn decoration_for(&self, path: &VPath) -> Option<&norte_frontend::Decoration> {
        self.state.decoration_for(path)
    }

    /// ¿Está marcada esta entrada? (#103) — delegado puro a
    /// [`norte_frontend::PaneState::is_marked`]. El render pinta un canalón
    /// textual (`*`) al inicio de la fila.
    #[must_use]
    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.state.is_marked(entry)
    }

    /// Togglea la marca de la entrada seleccionada. Delegado puro (#103).
    pub fn toggle_mark(&mut self) {
        self.state.toggle_mark();
    }

    /// mc/Total Commander: togglea la marca de la selección VISIBLE y avanza
    /// (dentro del filtro si hay uno activo, si no el cursor real; sin
    /// envolver en la última fila). Delegado puro a
    /// [`norte_frontend::PaneState::toggle_mark_and_advance`] (#103, review:
    /// la mecánica de "sobre qué avanza" no puede reimplementarse aquí ni en
    /// el dispatch — vive una sola vez en el modelo compartido).
    pub fn toggle_mark_and_advance(&mut self) {
        self.state.toggle_mark_and_advance();
    }

    /// Marca todas las entradas visibles. Delegado puro (#103).
    pub fn mark_all(&mut self) {
        self.state.mark_all();
    }

    /// Invierte las marcas de las entradas visibles. Delegado puro (#103).
    pub fn invert_marks(&mut self) {
        self.state.invert_marks();
    }

    /// Quita todas las marcas. Delegado puro (#103).
    pub fn clear_marks(&mut self) {
        self.state.clear_marks();
    }

    /// Marca/desmarca por glob; devuelve cuántas marcas cambió (#103).
    ///
    /// # Errors
    /// Si el patrón no compila.
    pub fn mark_glob(
        &mut self,
        pattern: &str,
        mark: bool,
    ) -> Result<usize, norte_frontend::PatternError> {
        self.state.mark_glob(pattern, mark)
    }

    /// Cuántas entradas marcadas. Delegado puro (#103).
    #[must_use]
    pub fn marks_len(&self) -> usize {
        self.state.marks_len()
    }

    /// Tamaño total de los FICHEROS marcados. Delegado puro (#103).
    #[must_use]
    pub fn marked_bytes(&self) -> u64 {
        self.state.marked_bytes()
    }

    /// Cuántas entradas marcadas son directorios. Delegado puro (#103) —
    /// ver [`norte_frontend::PaneState::marked_dirs`].
    #[must_use]
    pub fn marked_dirs(&self) -> usize {
        self.state.marked_dirs()
    }

    /// Marcas que el último refresh en el mismo directorio descartó porque su
    /// entrada desapareció. Delegado puro (#103) — ver
    /// [`norte_frontend::PaneState::pruned_marks`].
    #[must_use]
    pub fn pruned_marks(&self) -> usize {
        self.state.pruned_marks()
    }

    /// Sobre qué opera la acción: marcas, o cursor si no hay. Delegado puro (#103).
    #[must_use]
    pub fn marked_paths(&self) -> Vec<VPath> {
        self.state.marked_paths()
    }

    /// Toggle de ocultos (#107); devuelve el estado nuevo. En el pane
    /// virtual actúa sobre los RESULTADOS sin tocar la preferencia — al
    /// volver a un listado real manda `show_hidden_pref`.
    pub fn toggle_hidden(&mut self) -> bool {
        let now = self.state.toggle_hidden();
        if !self.virtual_search {
            self.show_hidden_pref = now;
        }
        now
    }

    /// Siembra la visibilidad de ocultos desde `[ui] show_hidden` (#107):
    /// fija la preferencia Y el estado actual.
    pub fn set_show_hidden(&mut self, show: bool) {
        self.show_hidden_pref = show;
        self.state.set_show_hidden(show);
    }

    /// Entradas apartadas por la ocultación (#107). Delegado puro.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.state.hidden_count()
    }

    /// Instala el lote de decoraciones resuelto (G3b) — ver
    /// `PaneState::set_decorations`.
    pub fn set_decorations(
        &mut self,
        decorations: std::collections::HashMap<VPath, norte_frontend::Decoration>,
    ) {
        self.state.set_decorations(decorations);
    }
}

/// Campo de texto activo del diálogo de búsqueda (`Alt+F7`, liveSearch T6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    /// Patrón sobre el NOMBRE (glob o regex).
    Name,
    /// Texto/regex sobre el CONTENIDO.
    Content,
}

/// Diálogo de búsqueda viva (`Alt+F7`, liveSearch T6): dos campos de texto
/// (nombre y contenido) y dos toggles (regex, case). El `regex` decide, por
/// eje, glob-vs-regex (nombre) y literal-vs-regex (contenido) al construir los
/// [`FsSearchParams`](norte_proto::methods::FsSearchParams) en el run loop.
/// La raíz del walk es el `cwd` del pane con foco (no editable, se muestra en
/// el modal). Sus teclas van hardcodeadas como los demás overlays (#24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchDialog {
    /// Patrón de nombre (glob o, con `regex`, regex).
    pub name: String,
    /// Texto de contenido (literal o, con `regex`, regex).
    pub content: String,
    /// Campo que recibe los imprimibles/backspace (Tab alterna).
    pub field: SearchField,
    /// `F2`: interpreta ambos patrones como regex en vez de glob/literal.
    pub regex: bool,
    /// `F3`: matching sensible a mayúsculas.
    pub case: bool,
}

impl Default for SearchDialog {
    fn default() -> Self {
        Self {
            name: String::new(),
            content: String::new(),
            field: SearchField::Name,
            regex: false,
            case: false,
        }
    }
}

impl SearchDialog {
    /// Diálogo vacío con el foco en el campo de nombre.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// El campo de texto activo, mutable.
    fn active_mut(&mut self) -> &mut String {
        match self.field {
            SearchField::Name => &mut self.name,
            SearchField::Content => &mut self.content,
        }
    }

    /// Un carácter imprimible al campo activo.
    pub fn push_char(&mut self, c: char) {
        self.active_mut().push(c);
    }

    /// Backspace en el campo activo.
    pub fn backspace(&mut self) {
        self.active_mut().pop();
    }

    /// Tab: alterna el campo activo Name ⇄ Content.
    pub fn toggle_field(&mut self) {
        self.field = match self.field {
            SearchField::Name => SearchField::Content,
            SearchField::Content => SearchField::Name,
        };
    }

    /// F2: alterna glob/literal ⇄ regex (aplica a AMBOS ejes).
    pub fn toggle_regex(&mut self) {
        self.regex = !self.regex;
    }

    /// F3: alterna sensibilidad a mayúsculas.
    pub fn toggle_case(&mut self) {
        self.case = !self.case;
    }

    /// ¿Hay algún criterio no vacío? Enter no lanza si ambos campos están
    /// vacíos (una búsqueda sin criterio es un no-op con aviso).
    #[must_use]
    pub fn has_criteria(&self) -> bool {
        !self.name.is_empty() || !self.content.is_empty()
    }
}

// El saneado de nombres ([`display_name`]/[`path_display`]/`must_mask`) y el
// orden del listado ([`sort_entries`] + `nfc_key`/`name_bytes`) viven ahora en
// `norte-frontend` (lógica de presentación PURA compartida con la GUI). Se
// re-exportan aquí para que los call-sites `crate::app::…`/`app::…` (main, ui,
// viewer) sigan resolviendo sin cambios.
pub use norte_frontend::{display_name, path_display, sort_entries};

/// Estado completo del TUI: dos panes y el foco.
pub struct App {
    /// Los dos paneles (izquierda, derecha).
    pub panes: [Pane; 2],
    /// Índice del pane con foco (invariante 0|1: privado, ver [`Self::focus`]).
    focus: usize,
    /// `true` cuando el usuario pidió salir.
    pub quit: bool,
    /// Secuencia de teclas pendiente, ya formateada (status bar).
    pub pending: String,
    /// Diálogo modal activo (bloquea el keymap hasta resolverse).
    pub modal: Option<Modal>,
    /// Último mensaje para la barra (error por categoría o resultado).
    pub message: Option<String>,
    /// Panel de tasks vivo.
    pub board: crate::tasks::TaskBoard,
    /// Viewer abierto (F3); None = navegando.
    pub viewer: Option<crate::viewer::Viewer>,
    /// Ayuda abierta (F1): líneas ya construidas + scroll. Se construye
    /// del keymap EFECTIVO al abrir (extensible: preset y capas del
    /// usuario incluidos, jamás una lista a mano).
    pub help: Option<Help>,
    /// Colisiones a la espera de diálogo: JAMÁS se pisa un modal abierto
    /// (una tecla en vuelo respondería a la pregunta equivocada); se
    /// atienden en orden al cerrarse el modal actual.
    pub pending_collisions: std::collections::VecDeque<crate::tasks::RetrySpec>,
    /// Aprobaciones de policy a la espera de diálogo (M3-3b T5): misma
    /// disciplina que las colisiones (jamás pisar un modal abierto), pero con
    /// PRIORIDAD sobre ellas — una aprobación tiene TTL en el daemon y una
    /// colisión espera lo que haga falta.
    pub pending_approvals: std::collections::VecDeque<norte_proto::methods::PolicyApprovalRequired>,
    /// Tema resuelto + profundidad de color (ADR 0020). El render lee de aquí;
    /// el hot-reload lo reemplaza. Default = preset `default`.
    pub theme: crate::theme::TuiTheme,
    /// Selector de tema abierto (popup): None = cerrado.
    pub theme_picker: Option<ThemePicker>,
    /// Gestor de extensiones abierto (overlay del catálogo, M4-P3): None =
    /// cerrado.
    pub extensions: Option<ExtensionManager>,
    /// TOFU Lua pendiente (M4, [`Modal::TrustLuaInit`]): path CANÓNICO del
    /// `init.lua` de proyecto + los BYTES leídos una sola vez. Al resolver
    /// el modal se registra la decisión y, si se aprueba, se evalúan ESTOS
    /// bytes — jamás se relee el disco entre el check y el eval
    /// (anti-TOCTOU).
    pub lua_pending_trust: Option<(std::path::PathBuf, Vec<u8>)>,
    /// Salida del hook `norte.ui.statusbar` del `init.lua` activo (M4 Lua),
    /// YA saneada por el host (`detail_for_bar`). `Some` sustituye la línea
    /// default de la barra del pane con foco; `None` = barra normal.
    pub lua_status: Option<String>,
    /// #44: sesión remota degradada a texto plano; indicador PERSISTENTE en la
    /// status bar (a diferencia de `message`, que es transitorio).
    pub connection_warning: Option<String>,
    /// Historial de directorios por pane (spec 2026-07-18, `Alt+↓`): mismo
    /// índice que `panes`. Vive en `App` y no en `Pane` (el historial no es
    /// estado de render): cada cd EXITOSO empuja el dir anterior (main.rs).
    pub history: [crate::nav::History; 2],
    /// Copia de la hotlist de `LoadedConfig` (clonada en arranque y en cada
    /// hot-reload OK): la fuente para el popup `Ctrl+D`. Los adds/removes
    /// SOLO la tocan tras persistir con éxito (consistencia con disco).
    pub hotlist: Vec<crate::config::HotlistItem>,
    /// Popup de navegación abierto (historial/hotlist): None = cerrado.
    pub nav_popup: Option<NavPopup>,
    /// Diálogo de búsqueda viva abierto (`Alt+F7`, liveSearch T6): None =
    /// cerrado. Captura imprimibles como el `name_input` del popup de nav.
    pub search_dialog: Option<SearchDialog>,
    /// Openers declarativos fusionados (#28): clonados en arranque y en cada
    /// hot-reload OK. Fuente de `pane.open` (F4). Vacío = sin openers.
    pub openers: norte_frontend::openers::OpenersConfig,
    /// Comando externo resuelto por `pane.open` y pendiente de lanzar (#28):
    /// `(programa, argv)`. `dispatch` lo fija tras validar; el run loop —
    /// dueño de la terminal — suspende el TUI, lo ejecuta y restaura.
    pub pending_open: Option<(String, Vec<std::ffi::OsString>)>,
    /// Hints de pie de página de los overlays de diálogo (H1 T3, #24),
    /// PRECOMPUTADOS del efectivo `dialog` vigente — igual que `help_lines`
    /// en `main.rs`, se reconstruyen en el arranque y en cada hot-reload OK
    /// (`main::build_keymaps` + `DialogHints::build`), ANTES de que el
    /// efectivo se mueva al `Resolver` compartido. `ui::draw_*` los lee en
    /// vez de una clave Fluent estática.
    pub dialog_hints: crate::hints::DialogHints,
    /// Command palette abierta (`Ctrl+P`/vim `:`, H1 T4): `None` = cerrada.
    pub palette: Option<Palette>,
    /// Filas de la palette PRECOMPUTADAS del keymap vigente
    /// ([`crate::palette::build_rows`]) — igual criterio que `help_lines`/
    /// `dialog_hints`: se reconstruyen en el arranque y en cada hot-reload
    /// OK, ANTES de que los efectivos se muevan al `Resolver`. Abrir la
    /// palette (`dispatch`, brazo `app.palette`) solo clona esta snapshot.
    pub palette_rows: Vec<crate::palette::Row>,
    /// Overlay de ajustes abierto (`app.settings`, S3): `None` = cerrado.
    /// Sus filas se reconstruyen del `cfg` VIGENTE en cada hot-reload OK
    /// (`main::reload_config`, `Settings::refresh`) — a diferencia de
    /// `palette`/`help`, que se CIERRAN, este overlay se queda abierto y se
    /// refresca en su sitio (ver el doc de `Settings::refresh`).
    pub settings: Option<Settings>,
}

/// Qué popup de navegación está abierto (spec 2026-07-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavPopupKind {
    /// Historial de directorios del pane con foco (sesión, no persistido).
    History,
    /// Favoritos persistidos en el `norte.toml` del USUARIO.
    Hotlist,
}

/// Un item del popup de navegación, CONGELADO al construirse en
/// [`App::open_nav_popup`]: display ya saneado, destino ya parseado y (en
/// hotlist) la clave cruda del favorito. El popup es una snapshot a
/// propósito — todo lo que una tecla necesita viaja dentro del item, nada
/// se re-resuelve contra un estado que pudo cambiar debajo.
#[derive(Debug, Clone)]
pub struct NavItem {
    /// Display YA saneado, listo para pintar.
    pub display: String,
    /// Destino parseado; `None` = entrada de hotlist inválida (se muestra
    /// con su aviso, no navega).
    pub target: Option<VPath>,
    /// `name` CRUDO del favorito — la clave del borrado con `d`
    /// ([`App::nav_popup_selected_hotlist_name`]), congelada al abrir: un
    /// hot-reload puede mutar `App::hotlist` bajo el popup y el borrado
    /// debe caer sobre lo MOSTRADO, jamás sobre lo que ahora ocupe ese
    /// índice en la lista nueva (review MAJOR T5). `None` en historial.
    pub hotlist_name: Option<String>,
}

/// Popup de navegación (`Alt+↓` historial / `Ctrl+D` hotlist). Los `items`
/// se construyen YA saneados en [`App::open_nav_popup`] (ver [`NavItem`]):
/// el render no re-decide nada y Enter no re-parsea nada.
#[derive(Debug, Clone)]
pub struct NavPopup {
    /// Historial u hotlist (decide título, footer y las teclas `a`/`d`).
    pub kind: NavPopupKind,
    /// Items congelados al abrir.
    items: Vec<NavItem>,
    /// Índice resaltado.
    cursor: usize,
    /// Input de nombre abierto (`a` en hotlist): captura imprimibles antes
    /// que nada (main.rs); `None` = navegación normal del popup.
    pub name_input: Option<String>,
}

impl NavPopup {
    /// Sube el cursor (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja el cursor (tope en el último item).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.items.len() {
            self.cursor += 1;
        }
    }

    /// Items congelados para el render.
    #[must_use]
    pub fn items(&self) -> &[NavItem] {
        &self.items
    }

    /// Índice resaltado.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// El item resaltado, si lo hay.
    #[must_use]
    pub fn selected(&self) -> Option<&NavItem> {
        self.items.get(self.cursor)
    }
}

/// Display de un item del popup de navegación: `[name — ]path` con el badge
/// hostil como PREFIJO si cualquier parte saldría alterada (mismo criterio
/// que los panes: lossy y MARCADO, spec §6).
fn nav_item_display(
    name: Option<&str>,
    path: &VPath,
    enc: Option<norte_encoding::NameEncoding>,
) -> String {
    // #98/F4: los popups son superficie de DECISIÓN (elegir destino de
    // salto) — siguen la reinterpretación del pane con foco, como la barra.
    let (texto, path_hostil) = norte_frontend::path_display_with(path, enc);
    let (prefix, name_hostil) = match name {
        Some(n) => {
            let (nt, nh) = display_name(n.as_bytes());
            (format!("{nt} — "), nh)
        }
        None => (String::new(), false),
    };
    if path_hostil || name_hostil {
        format!("{} {prefix}{texto}", crate::ui::HOSTILE_BADGE)
    } else {
        format!("{prefix}{texto}")
    }
}

/// Tope defensivo sobre `PluginInfo.description` en el wire (P1 encoding
/// audit F1): el manifiesto YA limita a 280 chars al PARSEAR
/// (`norte-plugin-host` manifest.rs, `ManifestError::DescriptionTooLong`) —
/// pero eso solo protege el camino honesto (un plugin bien formado, un
/// daemon fiel al server que lo cargó). Un daemon hostil o comprometido
/// podría mandar CUALQUIER longitud por el wire — el cliente no debe
/// confiar en que el server respetó su propio límite. Mismo valor que el
/// tope del manifiesto: mirror deliberado, no coincidencia.
pub const PLUGIN_DESCRIPTION_WIRE_CAP: usize = 280;

/// Clampa ([`PLUGIN_DESCRIPTION_WIRE_CAP`]) y enmascara ([`display_name`])
/// la `description` de CADA plugin de `plugins`, IN PLACE — en el único
/// punto donde un `PluginListResult` recién llegado del `Backend` entra al
/// estado del TUI (`main::dispatch`, brazos `app.extensions`/
/// `app.palette`). El trabajo se hace UNA vez por plugin aquí, no por fila
/// ni por frame: ambos consumidores ([`ExtensionManager`],
/// [`crate::palette::plugin_rows`]) comparten el resultado ya seguro para
/// pintar — `ExtensionManager` la repinta cada frame
/// (`ui::plugin_description_line`), y antes de este fix recalculaba el
/// enmascarado del String crudo (sin tope) en CADA uno.
pub fn clamp_plugin_descriptions(plugins: &mut [norte_proto::methods::PluginInfo]) {
    for p in plugins {
        if let Some(raw) = &p.description {
            let clamped: String = raw.chars().take(PLUGIN_DESCRIPTION_WIRE_CAP).collect();
            let (masked, _) = display_name(clamped.as_bytes());
            p.description = Some(masked);
        }
    }
}

#[cfg(test)]
mod clamp_plugin_descriptions_tests {
    use super::clamp_plugin_descriptions;
    use norte_proto::methods::PluginInfo;

    fn plugin(description: Option<&str>) -> PluginInfo {
        PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: description.map(str::to_owned),
            commands: Vec::new(),
            columns: Vec::new(),
        }
    }

    /// P1 encoding audit F1 (MEDIUM): `PluginInfo.description` no tiene tope
    /// en el wire (el manifiesto solo lo limita al PARSEAR, en el camino
    /// honesto) — un daemon hostil/comprometido podría mandar cualquier
    /// longitud. `clamp_plugin_descriptions` es el único punto donde
    /// `plugins_list` entra al estado del TUI (`main::dispatch`); debe
    /// recortarla ahí, de una vez, para ambos consumidores.
    #[test]
    fn clampa_al_tope_del_wire() {
        let mut plugins = vec![plugin(Some(&"a".repeat(50_000)))];
        clamp_plugin_descriptions(&mut plugins);
        assert_eq!(
            plugins[0].description.as_deref().unwrap().chars().count(),
            super::PLUGIN_DESCRIPTION_WIRE_CAP
        );
    }

    #[test]
    fn none_se_queda_none() {
        let mut plugins = vec![plugin(None)];
        clamp_plugin_descriptions(&mut plugins);
        assert_eq!(plugins[0].description, None);
    }

    #[test]
    fn corta_bajo_el_tope_no_se_toca() {
        let mut plugins = vec![plugin(Some("una description corta"))];
        clamp_plugin_descriptions(&mut plugins);
        assert_eq!(
            plugins[0].description.as_deref(),
            Some("una description corta")
        );
    }

    /// El override RTL nunca sobrevive crudo al clamp — se enmascara aquí,
    /// no en cada frame del gestor de extensiones.
    #[test]
    fn enmascara_override_rtl() {
        let mut plugins = vec![plugin(Some("abc\u{202E}gpj.exe"))];
        clamp_plugin_descriptions(&mut plugins);
        let d = plugins[0].description.as_deref().unwrap();
        assert!(!d.contains('\u{202E}'));
        assert!(d.contains('\u{FFFD}'));
    }
}

/// Overlay del catálogo de extensiones (M4-P3): la lista de plugins descubierta
/// por el core (YA ordenada por categoría e id) más los directorios que
/// fallaron al cargar, con un cursor de selección. Regla 7: el TUI no decide
/// nada — aprobar/activar viaja al core por el `Backend`; aquí solo se navega y
/// se refleja el estado. El `name`/`publisher` de cada plugin son texto LIBRE
/// de un tercero: se enmascaran con [`display_name`] al pintar (superficie de
/// decisión de seguridad).
#[derive(Debug, Clone)]
pub struct ExtensionManager {
    /// Plugins descubiertos, en el orden del core (categoría, luego id).
    pub plugins: Vec<norte_proto::methods::PluginInfo>,
    /// Directorios que no cargaron (diagnóstico), se pintan al final.
    pub errors: Vec<norte_proto::methods::PluginLoadError>,
    /// Índice del plugin resaltado.
    pub cursor: usize,
    /// Drill-down editor over the SELECTED plugin's `[config]` (G3c):
    /// `Some` while open — `dialog.confirm` on the plugin list opens it
    /// (fetches `plugin.get_config`), `dialog.cancel` inside it closes
    /// back to the plugin list (never the whole overlay).
    pub config: Option<PluginConfigPanel>,
}

/// The extension manager's config drill-down (G3c): which plugin, its
/// masked name (for the header — `Row`'s `name`/`desc` inside `state` are
/// ALREADY masked by `norte_frontend::plugin_config::sanitize_config_keys`,
/// this is just the plugin's own display name), and the pure editor state.
#[derive(Debug, Clone)]
pub struct PluginConfigPanel {
    /// Id of the plugin being configured — needed to call
    /// `Backend::plugin_set_config(id, key, value)` on commit.
    pub plugin_id: String,
    /// Masked plugin name, for the panel header.
    pub plugin_name: String,
    /// The pure cursor+edit widget over this plugin's `[config]` keys.
    pub state: norte_frontend::plugin_config::PluginConfigState,
}

impl ExtensionManager {
    /// Sube el cursor (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja el cursor (tope al último plugin).
    pub fn down(&mut self) {
        let max = self.plugins.len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(max);
    }

    /// El plugin bajo el cursor, si lo hay.
    #[must_use]
    pub fn selected(&self) -> Option<&norte_proto::methods::PluginInfo> {
        self.plugins.get(self.cursor)
    }

    /// Togglea el bool LOCAL de aprobación del plugin bajo el cursor, para
    /// feedback inmediato tras un `plugins_set_approval` OK en el Backend (la
    /// verdad vive en el core; esto solo evita un relistado para repintar).
    pub fn set_local_approved(&mut self, approved: bool) {
        if let Some(p) = self.plugins.get_mut(self.cursor) {
            p.approved = approved;
        }
    }

    /// Análogo a [`Self::set_local_approved`] para el estado de activación.
    pub fn set_local_enabled(&mut self, enabled: bool) {
        if let Some(p) = self.plugins.get_mut(self.cursor) {
            p.enabled = enabled;
        }
    }
}

/// Acción del usuario sobre el overlay de extensiones (el frontend traduce las
/// teclas; el efecto —llamar al `Backend`— vive en `main`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtAction {
    /// Resalta el anterior.
    Up,
    /// Resalta el siguiente.
    Down,
    /// Togglea la aprobación del plugin resaltado.
    ToggleApprove,
    /// Togglea la activación del plugin resaltado.
    ToggleEnable,
    /// Cierra el overlay.
    Close,
}

/// Popup de selección de tema: lista de presets con preview EN VIVO (mover el
/// cursor aplica el tema al vuelo; Esc revierte al que había, Enter lo fija).
#[derive(Debug, Clone)]
pub struct ThemePicker {
    /// Nombres de preset a elegir.
    pub names: Vec<String>,
    /// Índice resaltado.
    pub cursor: usize,
    /// Tema que había ANTES de abrir, para revertir al cancelar.
    pub original: crate::theme::TuiTheme,
}

impl ThemePicker {
    /// Sube el cursor (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja el cursor (tope al último).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.names.len() {
            self.cursor += 1;
        }
    }

    /// El nombre resaltado.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.names.get(self.cursor).map(String::as_str)
    }
}

/// Acción del usuario sobre el popup de tema (el frontend traduce las teclas).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    /// Resalta el anterior (con preview).
    Up,
    /// Resalta el siguiente (con preview).
    Down,
    /// Fija el tema resaltado y cierra.
    Confirm,
    /// Revierte al tema previo y cierra.
    Cancel,
}

/// Command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised): filtro libre
/// sobre TODOS los comandos de [`crate::keymap::COMMANDS`]. A diferencia de
/// los overlays de H1 T2 (modal/theme-picker/extensions/nav-popup), sus
/// teclas NO resuelven contra el contexto `dialog` — es un editor de texto
/// libre como el diálogo de búsqueda (`SearchDialog`, decisión 8 del plan
/// H1): no hay vocabulario `dialog.*` para "teclear un carácter" o "correr
/// la selección", así que el run loop las trata como fijas, hardcodeadas.
///
/// Las `rows` llegan YA construidas ([`crate::palette::build_rows`],
/// `App::palette_rows`, precomputadas como `help_lines`/`dialog_hints` —
/// mismo criterio: reconstruidas en el arranque y en cada hot-reload OK,
/// ANTES de que los efectivos se muevan al `Resolver`); `Palette::new` solo
/// pliega el haystack de cada fila. Mismo patrón de cache que
/// [`crate::nav::QuickSearch`] (#77): el fold por fila se computa UNA vez
/// aquí, no por keystroke — los keystrokes solo pliegan la query.
#[derive(Debug, Clone)]
pub struct Palette {
    /// `(comando, descripción, chord-o-guion)` — snapshot congelado al abrir.
    rows: Vec<crate::palette::Row>,
    /// Haystack plegado por fila (nombre + descripción, [`crate::nav::fold`]),
    /// índice-paralelo a `rows`.
    folds: Vec<String>,
    /// Bytes tecleados tal cual (matching SIN sanear; el saneado es solo al
    /// pintar, [`Self::query_display`] — mismo contrato que
    /// [`crate::nav::QuickSearch::query_display`]).
    query: Vec<u8>,
    /// Índices REALES en `rows` que casan (query vacía = todas).
    visible: Vec<usize>,
    /// Posición de la selección DENTRO de `visible`.
    cursor: usize,
}

impl Palette {
    /// Abre la palette sobre `rows` (la snapshot precomputada de `App`):
    /// pliega el haystack de cada fila y arranca con la query vacía (todo
    /// visible). El fold es sobre `text`+`desc` (lo PINTADO, ya enmascarado
    /// para una fila de plugin) — jamás sobre `key` (P1: podría llevar el
    /// `command_id` crudo del manifiesto, sin charset validado).
    #[must_use]
    pub fn new(rows: Vec<crate::palette::Row>) -> Self {
        let folds = rows
            .iter()
            .map(|row| crate::nav::fold(format!("{} {}", row.text, row.desc).as_bytes()))
            .collect();
        let mut p = Self {
            rows,
            folds,
            query: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
        };
        p.recompute();
        p
    }

    /// Recalcula `visible` a partir de la query actual sobre `self.folds`
    /// (el cache YA vigente) y clampa el cursor.
    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            let q = crate::nav::fold(&self.query);
            self.folds
                .iter()
                .enumerate()
                .filter(|(_, f)| f.contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Añade un carácter tecleado a la query y recalcula (mismo contrato que
    /// [`crate::nav::QuickSearch::push_char`]).
    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
    }

    /// Retira el último char UTF-8 completo tecleado y recalcula.
    pub fn backspace(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
    }

    /// Sube la selección (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja la selección (tope al final).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    /// Sube `n` posiciones (pgup).
    pub fn page_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// Baja `n` posiciones, tope al final (pgdn).
    pub fn page_down(&mut self, n: usize) {
        self.cursor = (self.cursor + n).min(self.visible.len().saturating_sub(1));
    }

    /// Índices REALES en `rows()` visibles con la query actual.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// Todas las filas ([`crate::palette::Row`]) — `rows()[visible()[i]]`
    /// para pintar la fila `i`-ésima de la lista filtrada. Solo `text`/
    /// `desc`/`chord` se pintan; `key` es de despacho interno (ver doc de
    /// [`crate::palette::Row`]).
    #[must_use]
    pub fn rows(&self) -> &[crate::palette::Row] {
        &self.rows
    }

    /// Posición de la selección DENTRO de `visible()` (para `ListState`).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// La CLAVE de despacho bajo el cursor, si hay alguna visible (P1: ya no
    /// es `&'static str` — una fila de plugin trae una `key` construida en
    /// tiempo de ejecución, `plugin:{id}:{command}`; se clona porque
    /// `main::dispatch` la usa DESPUÉS de cerrar la palette, `app.palette =
    /// None`, que dropea `rows`).
    #[must_use]
    pub fn selected(&self) -> Option<String> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.rows[i].key.clone())
    }

    /// Query para pintar (lossy, enmascarada — mismo contrato que
    /// [`crate::nav::QuickSearch::query_display`]: sin bracketed paste un
    /// paste hostil llega como stream de `push_char` y pintaría bidi/
    /// invisibles crudos en el borde).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod palette_tests {
    use super::Palette;

    fn row(key: &str, desc: &str, chord: &str) -> crate::palette::Row {
        crate::palette::Row {
            key: key.to_owned(),
            text: key.to_owned(),
            desc: desc.to_owned(),
            chord: chord.to_owned(),
        }
    }

    fn rows() -> Vec<crate::palette::Row> {
        vec![
            row("app.quit", "quit norte", "q"),
            row("app.help", "this help", "f1"),
        ]
    }

    #[test]
    fn palette_filtra_y_selecciona() {
        let mut p = Palette::new(rows());
        for c in "quit".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible().len(), 1, "solo app.quit casa con 'quit'");
        assert_eq!(p.selected().as_deref(), Some("app.quit"));
    }

    #[test]
    fn palette_query_hostil_se_enmascara() {
        let mut p = Palette::new(rows());
        for c in "a\u{202E}b".chars() {
            p.push_char(c);
        }
        let display = p.query_display();
        assert!(
            !display.chars().any(norte_encoding::is_terminal_hazard),
            "query_display dejó un hazard crudo: {display:?}"
        );
    }

    #[test]
    fn palette_filtro_vacio_muestra_todo() {
        let p = Palette::new(rows());
        assert_eq!(p.visible().len(), 2, "query vacía = todas las filas");
        assert_eq!(
            p.selected().as_deref(),
            Some("app.quit"),
            "cursor arranca en la primera"
        );
    }

    /// Filtro que NO casa con ninguna fila: `selected()` devuelve `None`
    /// (jamás un índice fantasma) y `up`/`down`/páginas no panican sobre
    /// `visible` vacío.
    #[test]
    fn palette_sin_matches_selected_es_none_y_no_panica() {
        let mut p = Palette::new(rows());
        for c in "zzz".chars() {
            p.push_char(c);
        }
        assert!(p.visible().is_empty());
        assert_eq!(p.selected(), None);
        p.up();
        p.down();
        p.page_up(3);
        p.page_down(3);
        assert_eq!(p.selected(), None);
    }

    /// (P1) Filas de plugin ([`crate::palette::plugin_rows`]) mezcladas con
    /// las built-in: el filtro de texto libre casa contra el TÍTULO YA
    /// enmascarado (`text`), y Enter (`selected()`) devuelve la `key` de
    /// despacho `plugin:{id}:{command}` — jamás el texto pintado.
    #[test]
    fn palette_filas_de_plugin_se_filtran_por_titulo_y_despachan_por_key() {
        let plugin = norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".into(),
                title: "Greet loudly".into(),
            }],
            columns: Vec::new(),
        };
        let mut all = rows();
        all.extend(crate::palette::plugin_rows(&[plugin]));
        let mut p = Palette::new(all);
        for c in "loudly".chars() {
            p.push_char(c);
        }
        assert_eq!(
            p.visible().len(),
            1,
            "solo la fila de plugin casa con 'loudly' (el título)"
        );
        assert_eq!(p.selected().as_deref(), Some("plugin:org.norte.demo:greet"));
    }
}

// `PendingWrite`/`SettingsEditError`/`Settings`/`cycle` (S3 overlay editor)
// hoisted to `norte_frontend::settings` in S4 (GUI settings view): the code
// had ZERO TUI-specific coupling (no ratatui/crossterm, pure state +
// `norte_frontend::nav::fold`) — re-exported here under their historical
// names so the rest of this crate (and integration tests referencing
// `norte_tui::app::{Settings, PendingWrite, SettingsEditError}`) keep
// resolving unchanged. See `norte_frontend::settings` module doc.
pub use norte_frontend::settings::{PendingWrite, SettingsEditError, SettingsState as Settings};

impl App {
    /// App con foco en el pane izquierdo.
    #[must_use]
    pub fn new(left: Pane, right: Pane) -> Self {
        Self {
            panes: [left, right],
            focus: 0,
            quit: false,
            pending: String::new(),
            modal: None,
            message: None,
            board: crate::tasks::TaskBoard::default(),
            viewer: None,
            help: None,
            pending_collisions: std::collections::VecDeque::new(),
            pending_approvals: std::collections::VecDeque::new(),
            theme: crate::theme::TuiTheme::default(),
            theme_picker: None,
            extensions: None,
            lua_pending_trust: None,
            lua_status: None,
            connection_warning: None,
            history: [
                crate::nav::History::default(),
                crate::nav::History::default(),
            ],
            hotlist: Vec::new(),
            nav_popup: None,
            search_dialog: None,
            openers: norte_frontend::openers::OpenersConfig::empty(),
            pending_open: None,
            dialog_hints: crate::hints::DialogHints::default(),
            palette: None,
            palette_rows: Vec::new(),
            settings: None,
        }
    }

    /// Abre el diálogo de búsqueda viva (`Alt+F7`, liveSearch T6) vacío. La
    /// raíz del walk se resuelve al lanzar (cwd del pane con foco).
    pub fn open_search_dialog(&mut self) {
        self.search_dialog = Some(SearchDialog::new());
    }

    /// Índice del pane con foco (0 = izquierda, 1 = derecha).
    #[must_use]
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// El pane con foco.
    #[must_use]
    pub fn focused(&self) -> &Pane {
        &self.panes[self.focus]
    }

    /// (índice, path) de la entrada File enfocada sin `size`: candidata a la
    /// sonda de stat on-focus (#52, listado lazy).
    #[must_use]
    pub fn focused_needs_stat(&self) -> Option<(usize, VPath)> {
        let e = self.focused().selected()?;
        (e.kind == EntryKind::File && e.size.is_none()).then(|| (self.focus(), e.path.clone()))
    }

    /// El pane con foco, mutable.
    pub fn focused_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focus]
    }

    /// Alterna el foco entre los dos panes (Tab, keymap mc).
    pub fn switch_focus(&mut self) {
        self.focus ^= 1;
    }

    /// Abre el popup selector de tema (ADR 0020): lista de presets, cursor en el
    /// tema vigente, con preview EN VIVO desde ya.
    pub fn open_theme_picker(&mut self) {
        let names: Vec<String> = norte_theme::preset_names()
            .into_iter()
            .map(String::from)
            .collect();
        let current = self.theme.name().map(String::from);
        let cursor = current
            .as_ref()
            .and_then(|c| names.iter().position(|n| n == c))
            .unwrap_or(0);
        self.theme_picker = Some(ThemePicker {
            names,
            cursor,
            original: self.theme.clone(),
        });
        self.preview_theme();
    }

    /// Aplica al vuelo el tema resaltado en el popup (preview en vivo).
    fn preview_theme(&mut self) {
        let Some(name) = self
            .theme_picker
            .as_ref()
            .and_then(|p| p.selected().map(String::from))
        else {
            return;
        };
        let depth = crate::theme::detect_depth();
        if let Ok(theme) = crate::theme::resolve(Some(&name), depth) {
            self.theme = theme;
        }
    }

    /// Procesa una acción del usuario sobre el popup de tema. `Confirm` fija el
    /// tema previsualizado; `Cancel` revierte al que había al abrir.
    pub fn theme_picker_input(&mut self, action: PickerAction) {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.theme_picker {
                    p.up();
                }
                self.preview_theme();
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.theme_picker {
                    p.down();
                }
                self.preview_theme();
            }
            PickerAction::Confirm => {
                let name = self
                    .theme_picker
                    .as_ref()
                    .and_then(|p| p.selected().map(String::from));
                self.theme_picker = None;
                if let Some(n) = name {
                    self.message = Some(norte_i18n::ta("msg-theme-applied", &[("name", &n)]));
                }
            }
            PickerAction::Cancel => {
                if let Some(p) = self.theme_picker.take() {
                    self.theme = p.original;
                }
                self.message = Some(norte_i18n::t("msg-theme-reverted"));
            }
        }
    }

    /// Si no hay modal abierto, abre el diálogo de la siguiente colisión
    /// encolada. Llamar tras cerrar un modal y en cada tick.
    pub fn open_next_collision(&mut self) {
        if self.modal.is_none()
            && let Some(retry) = self.pending_collisions.pop_front()
        {
            self.modal = Some(Modal::Collision { retry });
        }
    }

    /// Si no hay modal abierto, abre el siguiente diálogo pendiente:
    /// aprobaciones de policy PRIMERO (tienen TTL en el daemon), colisiones
    /// después. Llamar tras cerrar un modal y al llegar una aprobación.
    pub fn open_next_pending(&mut self) {
        if self.modal.is_none()
            && let Some(req) = self.pending_approvals.pop_front()
        {
            self.modal = Some(Modal::ApproveAgentOp { req });
            return;
        }
        self.open_next_collision();
    }

    /// Cancela `Modal::MarkPattern` SIN marcar nada — el equivalente de un
    /// `DialogOutcome::Cancelled` para ESTE modal de texto libre (#103 T9),
    /// que no pasa por el ALLOWLIST de [`dialog_action`] y por tanto no
    /// tiene su propio Esc en `on_dialog_key`. Abre la siguiente pendiente
    /// en cola, misma disciplina que cerrar cualquier otro modal (jamás
    /// pisar una aprobación/colisión que llegó mientras este estaba
    /// abierto).
    ///
    /// Deliberadamente NO genérico sobre `self.modal` (review rust MAJOR
    /// M1): para `Modal::ApproveAgentOp` cerrar sin más deja al agente sin
    /// respuesta hasta el TTL del daemon — el cierre real de ESE modal
    /// (`on_dialog_key`, `DialogOutcome::Cancelled`) empareja el cierre con
    /// un `policy.decide(approve: false)` async, algo que un método
    /// síncrono no puede hacer. El guard estructural (`debug_assert!`) hace
    /// del allowlist "solo modales de texto libre" algo que el compilador
    /// de tests, no la disciplina del caller, hace cumplir.
    pub fn cancel_mark_pattern(&mut self) {
        if !matches!(self.modal, Some(Modal::MarkPattern { .. })) {
            debug_assert!(
                false,
                "solo los modales de texto libre se cierran sin decisión; \
                 un modal de DECISIÓN debe denegar por on_dialog_key"
            );
            return;
        }
        self.modal = None;
        self.open_next_pending();
    }

    /// Abre el modal de copia/movimiento (F5/F6, #103 T10): orígenes = las
    /// MARCAS del pane con foco (o el cursor si no hay ninguna), destino =
    /// el DIRECTORIO del pane inactivo. No-op si no hay nada que transferir
    /// (pane vacío): jamás un diálogo sobre un lote vacío.
    pub fn open_transfer_modal(&mut self, kind: TransferKind) {
        let items = self.focused().marked_paths();
        if items.is_empty() {
            return;
        }
        let to = self.panes[1 - self.focus].dir().clone();
        self.modal = Some(Modal::ConfirmTransfer { kind, items, to });
    }

    /// Abre el modal de borrado (F8, #103 T10) sobre las MARCAS del pane con
    /// foco (o el cursor si no hay ninguna). `permanent` lo decide el caller:
    /// es `shift+F8`, o la ausencia de papelera en el provider — que se
    /// sondea UNA vez por lote, no una por ítem (serían N round-trips de red
    /// para responder siempre lo mismo). No-op si no hay nada que borrar.
    pub fn open_delete_modal(&mut self, permanent: bool) {
        let items = self.focused().marked_paths();
        if items.is_empty() {
            return;
        }
        self.modal = Some(Modal::ConfirmDelete { items, permanent });
    }

    /// Las marcas las CONSUME la operación (mc/Total Commander): se limpian
    /// al ENVIAR el lote, no al completarse, para que jamás exista una
    /// selección a medio consumir cuyo significado dependa de qué task
    /// terminó (#103).
    pub fn consume_marks(&mut self) {
        self.focused_mut().clear_marks();
    }

    /// Abre el modal de marcado por patrón (#103).
    pub fn open_mark_pattern(&mut self, mark: bool) {
        self.modal = Some(Modal::MarkPattern {
            mark,
            pattern: String::new(),
            error: None,
        });
    }

    /// Añade un carácter al patrón en curso. No-op sin modal de patrón.
    pub fn mark_pattern_push(&mut self, c: char) {
        if let Some(Modal::MarkPattern { pattern, error, .. }) = &mut self.modal {
            // #103 T9 review MINOR: un patrón pegado por accidente (varios
            // KB de portapapeles) desbordaría el ancho del modal y recortaría
            // el hint — tope silencioso, como el resto de campos de texto de
            // este overlay no tienen un límite de terminal que los frene.
            if pattern.chars().count() >= MARK_PATTERN_MAX_CHARS {
                return;
            }
            pattern.push(c);
            *error = None;
        }
    }

    /// Borra el último carácter del patrón. No-op sin modal de patrón.
    pub fn mark_pattern_pop(&mut self) {
        if let Some(Modal::MarkPattern { pattern, error, .. }) = &mut self.modal {
            pattern.pop();
            *error = None;
        }
    }

    /// Aplica el patrón: cierra el modal y devuelve cuántas marcas cambió.
    /// Un patrón inválido DEJA el modal abierto con el diagnóstico — el
    /// usuario conserva lo tecleado para corregirlo.
    ///
    /// # Errors
    /// Si el glob no compila.
    pub fn mark_pattern_confirm(&mut self) -> Result<usize, norte_frontend::PatternError> {
        let Some(Modal::MarkPattern { mark, pattern, .. }) = &self.modal else {
            return Ok(0);
        };
        let (mark, pattern) = (*mark, pattern.clone());
        match self.focused_mut().mark_glob(&pattern, mark) {
            Ok(changed) => {
                self.modal = None;
                // Misma disciplina que CUALQUIER otro cierre de modal
                // (`on_dialog_key`, `cancel_mark_pattern`): jamás dejar una
                // aprobación/colisión encolada esperando a la próxima tecla.
                self.open_next_pending();
                Ok(changed)
            }
            Err(e) => {
                let msg = e.to_string();
                if let Some(Modal::MarkPattern { error, .. }) = &mut self.modal {
                    *error = Some(msg);
                }
                Err(e)
            }
        }
    }

    /// Abre el modal de crear directorio (F7, #104).
    pub fn open_mkdir(&mut self) {
        self.modal = Some(Modal::Mkdir {
            name: String::new(),
            error: None,
        });
    }

    /// Añade un carácter al nombre en curso. No-op sin modal de mkdir.
    /// Tope en `chars` como el patrón (#103): un paste accidental no
    /// desborda el modal; el límite REAL del nombre lo pone el provider.
    pub fn mkdir_push(&mut self, c: char) {
        if let Some(Modal::Mkdir { name, error }) = &mut self.modal {
            if name.chars().count() >= MARK_PATTERN_MAX_CHARS {
                return;
            }
            name.push(c);
            *error = None;
        }
    }

    /// Borra el último carácter del nombre. No-op sin modal de mkdir.
    pub fn mkdir_pop(&mut self) {
        if let Some(Modal::Mkdir { name, error }) = &mut self.modal {
            name.pop();
            *error = None;
        }
    }

    /// Cancela `Modal::Mkdir` sin crear nada — el Esc de ESTE modal de
    /// texto libre (mismo contrato y guard que [`Self::cancel_mark_pattern`]:
    /// un modal de DECISIÓN jamás se cierra por aquí).
    pub fn cancel_mkdir(&mut self) {
        if !matches!(self.modal, Some(Modal::Mkdir { .. })) {
            debug_assert!(
                false,
                "solo los modales de texto libre se cierran sin decisión; \
                 un modal de DECISIÓN debe denegar por on_dialog_key"
            );
            return;
        }
        self.modal = None;
        self.open_next_pending();
    }

    /// Valida el nombre y devuelve el DESTINO completo (dir del pane con
    /// foco + nombre como [`norte_proto::Segment`] — la validación es la
    /// del `VPath`: ni vacío, ni `/`, ni NUL, ni `.`/`..`). NO cierra el
    /// modal (#104 review MINOR-1): el caller lo cierra con
    /// [`Self::mkdir_submitted`] SOLO tras encolar la task — un submit que
    /// falla (policy, conexión) deja el diagnóstico con
    /// [`Self::mkdir_set_error`] y el usuario CONSERVA lo tecleado. Un
    /// nombre inválido deja su diagnóstico aquí mismo y devuelve `None`.
    pub fn mkdir_confirm(&mut self) -> Option<VPath> {
        let Some(Modal::Mkdir { name, .. }) = &self.modal else {
            return None;
        };
        match norte_proto::Segment::new(name.as_bytes().to_vec()) {
            Ok(seg) => Some(self.focused().dir().join(seg)),
            Err(e) => {
                let msg = e.to_string();
                self.mkdir_set_error(msg);
                None
            }
        }
    }

    /// Cierra el modal tras un submit que SÍ encoló (#104): misma
    /// disciplina de cierre que el resto (jamás dejar una pendiente
    /// esperando).
    pub fn mkdir_submitted(&mut self) {
        if matches!(self.modal, Some(Modal::Mkdir { .. })) {
            self.modal = None;
            self.open_next_pending();
        }
    }

    /// Deja el diagnóstico de un submit fallido en el modal (#104): el
    /// nombre tecleado sobrevive para corregir y reintentar.
    pub fn mkdir_set_error(&mut self, msg: String) {
        if let Some(Modal::Mkdir { error, .. }) = &mut self.modal {
            *error = Some(msg);
        }
    }

    /// Abre el popup de navegación (spec 2026-07-18): historial del pane
    /// con foco (más reciente primero) o la copia de hotlist. Los items se
    /// construyen YA saneados aquí (`nav_item_display`); una entrada de
    /// hotlist inválida se muestra con su aviso y destino `None`.
    pub fn open_nav_popup(&mut self, kind: NavPopupKind) {
        let enc = self.focused().name_encoding();
        let items: Vec<NavItem> = match kind {
            NavPopupKind::History => self.history[self.focus]
                .entries()
                .iter()
                .map(|p| NavItem {
                    display: nav_item_display(None, p, enc),
                    target: Some(p.clone()),
                    hotlist_name: None,
                })
                .collect(),
            NavPopupKind::Hotlist => self
                .hotlist
                .iter()
                .map(|h| {
                    let (display, target) = if let Ok(p) = &h.target {
                        (nav_item_display(Some(&h.name), p, enc), Some(p.clone()))
                    } else {
                        // review MINOR T5: el flag hostil del name NO se
                        // descarta — una inválida con name bidi también
                        // lleva el badge (mismo criterio que el resto).
                        let (name, hostil) = display_name(h.name.as_bytes());
                        let aviso = t("hotlist-invalid");
                        let display = if hostil {
                            format!("{} {name} {aviso}", crate::ui::HOSTILE_BADGE)
                        } else {
                            format!("{name} {aviso}")
                        };
                        (display, None)
                    };
                    NavItem {
                        display,
                        target,
                        hotlist_name: Some(h.name.clone()),
                    }
                })
                .collect(),
        };
        self.nav_popup = Some(NavPopup {
            kind,
            items,
            cursor: 0,
            name_input: None,
        });
    }

    /// Procesa una acción sobre el popup de navegación. `Confirm` con un
    /// item VÁLIDO cierra el popup y devuelve su destino (el caller navega
    /// por el flujo de cd normal); sobre un item inválido (o sin items) es
    /// no-op — el popup sigue abierto. `Cancel` cierra el `name_input` si
    /// está activo, y si no, el popup. El caller no debe llamar a `Confirm`
    /// con `name_input` activo (Enter ahí confirma el ADD, main.rs).
    pub fn nav_popup_input(&mut self, action: PickerAction) -> Option<VPath> {
        match action {
            PickerAction::Up => {
                if let Some(p) = &mut self.nav_popup {
                    p.up();
                }
                None
            }
            PickerAction::Down => {
                if let Some(p) = &mut self.nav_popup {
                    p.down();
                }
                None
            }
            PickerAction::Confirm => {
                let target = self
                    .nav_popup
                    .as_ref()
                    .and_then(NavPopup::selected)
                    .and_then(|it| it.target.clone());
                if target.is_some() {
                    self.nav_popup = None;
                }
                target
            }
            PickerAction::Cancel => {
                if let Some(p) = &mut self.nav_popup {
                    if p.name_input.is_some() {
                        p.name_input = None;
                    } else {
                        self.nav_popup = None;
                    }
                }
                None
            }
        }
    }

    /// Abre el input de nombre del popup de hotlist (`a`), prellenado
    /// vacío. En el popup de historial es no-op (no hay nada que nombrar).
    pub fn nav_popup_open_name_input(&mut self) {
        if let Some(p) = &mut self.nav_popup
            && p.kind == NavPopupKind::Hotlist
        {
            p.name_input = Some(String::new());
        }
    }

    /// El `name` CRUDO del favorito seleccionado (la clave que necesita
    /// `persist_hotlist_remove` — el display del item va saneado y NO sirve
    /// como clave). Sale de la clave CONGELADA en el propio item
    /// ([`NavItem::hotlist_name`]): jamás se indexa `App::hotlist`, que un
    /// hot-reload pudo mutar bajo el popup (review MAJOR T5 — borraría
    /// otro favorito). `None` en historial o sin items.
    #[must_use]
    pub fn nav_popup_selected_hotlist_name(&self) -> Option<String> {
        self.nav_popup.as_ref()?.selected()?.hotlist_name.clone()
    }

    /// Refleja en la copia local un favorito YA persistido con éxito
    /// (reemplaza por `name` conservando posición, o añade al final — la
    /// MISMA semántica que `config::persist_hotlist_add`/`load`) y refresca
    /// el popup si está abierto.
    pub fn hotlist_apply_saved(&mut self, name: &str, target: VPath) {
        if let Some(item) = self.hotlist.iter_mut().find(|h| h.name == name) {
            item.target = Ok(target);
        } else {
            self.hotlist.push(crate::config::HotlistItem {
                name: name.to_owned(),
                target: Ok(target),
            });
        }
        self.rebuild_hotlist_popup();
    }

    /// Refleja en la copia local un favorito YA borrado del disco y
    /// refresca el popup si está abierto.
    pub fn hotlist_apply_removed(&mut self, name: &str) {
        self.hotlist.retain(|h| h.name != name);
        self.rebuild_hotlist_popup();
    }

    /// Reconstruye los items del popup de hotlist tras un add/remove,
    /// conservando el cursor (con clamp): la lista pintada nunca queda
    /// desincronizada de la copia en `App` (el invariante 1:1 de índices).
    fn rebuild_hotlist_popup(&mut self) {
        if let Some(p) = &self.nav_popup
            && p.kind == NavPopupKind::Hotlist
        {
            let cursor = p.cursor;
            self.open_nav_popup(NavPopupKind::Hotlist);
            if let Some(p) = &mut self.nav_popup {
                p.cursor = cursor.min(p.items.len().saturating_sub(1));
            }
        }
    }
}

/// Estado de la ayuda (F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    /// Contenido ya renderizable (secciones y bindings formateados).
    pub lines: Vec<String>,
    /// Primera línea visible.
    pub scroll: usize,
}

impl Help {
    /// Baja `n` líneas (tope al final).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.lines.len().saturating_sub(1));
    }

    /// Sube `n` líneas.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }
}

/// Tipo de transferencia pendiente de confirmación/colisión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Copia (F5).
    Copy,
    /// Movimiento (F6).
    Move,
}

/// Diálogo modal activo. Sus teclas resuelven contra el contexto `dialog`
/// del keymap (H1, issue #24 — CERRADO): el run loop pasa la tecla por el
/// [`Resolver`](crate::keymap::Resolver) del efectivo `dialog` y el comando
/// resultante se filtra por el ALLOWLIST del modal concreto
/// ([`dialog_action`]) — la semántica de SEGURIDAD (qué confirma, qué
/// deniega, qué es inerte) vive en código, jamás en el keymap; solo la
/// ASIGNACIÓN de tecla→comando es rebindeable. Única excepción:
/// `Modal::TrustLuaInit`, que el run loop intercepta ANTES (necesita el
/// `LuaHost`) y resuelve con [`trust_lua_key`] — decisión 8 del plan H1, no
/// migrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Confirmación de borrado (F8) sobre las MARCAS. `permanent = false` →
    /// papelera.
    ConfirmDelete {
        /// Los ítems a borrar, en orden de listado.
        items: Vec<VPath>,
        /// Permanente (shift+F8, o sin papelera en el provider): el diálogo
        /// AVISA (ADR 0009).
        permanent: bool,
    },
    /// Confirmación de copia/movimiento sobre las MARCAS (#103). `to` es el
    /// DIRECTORIO destino (el del otro pane): con varios ítems no hay un
    /// nombre único que editar. El destino editable de un solo ítem, y el
    /// rename que trae, viven en #105.
    ConfirmTransfer {
        /// Copy o Move.
        kind: TransferKind,
        /// Los orígenes, en orden de listado.
        items: Vec<VPath>,
        /// Directorio destino.
        to: VPath,
    },
    /// Colisión: elegir política y REENVIAR la operación entera (ADR 0005:
    /// el engine trata Ask como Fail; el TUI pregunta a nivel de task).
    /// Porta el `RetrySpec` COMPLETO: el reintento conserva las opciones
    /// originales, solo cambia la política de colisión.
    Collision {
        /// La transferencia que colisionó, lista para reenviar.
        retry: crate::tasks::RetrySpec,
    },
    /// Aprobación de una op de AGENTE bajo regla `ask` (M3-3b T5): el daemon
    /// difundió `policy.approval_required` y espera `policy.decide`. Las
    /// rutas son SOLO display (redactadas server-side): jamás se reparsean.
    /// `y` aprueba, `n`/Esc deniegan; Enter NO aprueba (aprobar una mutación
    /// de agente no es una respuesta inocua que merezca dispararse sola —
    /// mismo principio que la colisión).
    ApproveAgentOp {
        /// La aprobación pendiente tal como llegó del daemon.
        req: norte_proto::methods::PolicyApprovalRequired,
    },
    /// Primer contacto TOFU con un host SSH desconocido (#45, ADR 0015 D):
    /// un `Error::HostKeyUnknown` al navegar a `dir`. Muestra host/algo/
    /// fingerprint para que el usuario los COMPARE fuera de banda; `y`
    /// confía (`connection.trust_host_key`) y reintenta la navegación,
    /// `n`/Esc cancelan. Enter NO confía (decisión de seguridad, mismo
    /// principio que la aprobación de agente). host/algo/fingerprint vienen
    /// del servidor remoto (no confiable): se enmascaran al pintar.
    TrustHostKey {
        /// Host desnudo al que se conecta (el del `HostKeyUnknown`).
        host: String,
        /// Puerto (ausente = default del scheme).
        port: Option<u16>,
        /// Algoritmo de la clave (p. ej. `ssh-ed25519`).
        algo: String,
        /// Fingerprint OpenSSH `SHA256:<base64>` — la MISMA cadena que va a
        /// `connection.trust_host_key`.
        fingerprint: String,
        /// La ruta remota a la que reintentar navegar tras confiar.
        dir: VPath,
    },
    /// TOFU del `./.norte/init.lua` de PROYECTO (M4 Lua, ADR 0026): un repo
    /// AJENO trae un script que correría con los permisos del usuario —
    /// primer contacto pregunta. `y` confía y evalúa, `n`/Esc deniegan
    /// (persistido por (path, hash) hasta que el fichero cambie); Enter NO
    /// aprueba (decisión de seguridad, mismo principio que
    /// [`Modal::ApproveAgentOp`]). Los BYTES aprobados viven en
    /// [`App::lua_pending_trust`] (anti-TOCTOU: lo aprobado = lo evaluado).
    TrustLuaInit {
        /// Path del script YA SANEADO por quien construye el modal
        /// (`detail_for_bar`): solo display, jamás se reparsea.
        path: String,
        /// sha256 abreviado (32 hex = 128 bits — forjar una colisión corta
        /// cuesta minutos; el humano compara lo que ve) del contenido, para
        /// correlar con el `lua-trust.toml` a ojo.
        hash_abbrev: String,
    },
    /// Confirmar `app.quit` (S2, `[ui] confirm_quit`): abierto por el brazo
    /// de despacho de `app.quit` en `main.rs` cuando [`quit_needs_confirm`]
    /// lo pide — SIN datos propios (a diferencia del equivalente de la GUI,
    /// que cuenta tasks/marcas para el título): sin riesgo de seguridad que
    /// enmascarar, así que reutiliza el ALLOWLIST/hint de
    /// [`ALLOW_CONFIRM`]/`DialogHints::confirm` sin necesitar los suyos
    /// propios. Los Ctrl+C hardcodeados del resto de `main.rs` NO pasan por
    /// aquí a propósito (ver el comentario junto al brazo de despacho): ese
    /// atajo de salida de emergencia se mantiene inmediato en todos los
    /// overlays, igual que antes de S2.
    ConfirmQuit,
    /// Marcar (`mark = true`) o desmarcar por patrón (`+`/`-`, #103). El
    /// texto es la query CRUDA del usuario; se enmascara al pintarla, igual
    /// que el quick search (un patrón puede llegar por PASTE con bidi o
    /// invisibles).
    MarkPattern {
        /// Marcar, o desmarcar.
        mark: bool,
        /// Lo tecleado hasta ahora.
        pattern: String,
        /// Diagnóstico del último intento fallido, para pintarlo bajo el
        /// campo. `None` = aún no se ha confirmado nada.
        error: Option<String>,
    },
    /// Crear directorio (F7, #104). Texto libre como [`Modal::MarkPattern`]:
    /// el nombre CRUDO del usuario, enmascarado al pintarlo (un nombre
    /// llega por paste con bidi/invisibles tan fácil como un patrón).
    Mkdir {
        /// Lo tecleado hasta ahora.
        name: String,
        /// Diagnóstico del último intento inválido (`VPath` o del engine),
        /// pintado bajo el campo.
        error: Option<String>,
    },
}

/// Tope de caracteres del patrón de [`Modal::MarkPattern`] (#103 T9 review
/// MINOR): en `chars()`, no bytes — igual criterio que [`DETAIL_MAX_CHARS`],
/// un carácter multibyte cuenta una vez.
pub const MARK_PATTERN_MAX_CHARS: usize = 256;

/// S2 (`[ui] confirm_quit`): si el brazo de despacho de `app.quit` debe abrir
/// [`Modal::ConfirmQuit`] en vez de cerrar de inmediato. Pura — el run loop
/// aporta `board_has_active` ([`crate::tasks::TaskBoard::has_active`]), así
/// que es testeable sin ratatui/tokio. `Auto` (por defecto) es el
/// comportamiento pre-S2: confirma solo si el panel de tasks tiene trabajo en
/// vuelo; `Always`/`Never` son incondicionales. Envoltorio fino (revisión S,
/// M6): la decisión de tres vías era byte-idéntica a la de la GUI
/// (`confirm_quit_should_open`) — hoisteada a
/// [`norte_frontend::settings::quit_needs_confirm`].
#[must_use]
pub fn quit_needs_confirm(mode: crate::config::ConfirmQuit, board_has_active: bool) -> bool {
    norte_frontend::settings::quit_needs_confirm(mode, board_has_active)
}

/// Resultado de una tecla sobre un modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Tecla irrelevante: el diálogo sigue abierto.
    Open,
    /// Cerrado sin hacer nada.
    Cancelled,
    /// Confirmado (Enter/y).
    Confirmed,
    /// Reintentar la transferencia con esta política.
    Retry(norte_proto::CollisionPolicy),
}

/// ALLOWLIST de `Modal::ConfirmDelete`/`Modal::ConfirmTransfer`/
/// `Modal::ConfirmQuit` (S2, `[ui] confirm_quit`): `approve` y `confirm`
/// ambos aceptan (Enter e `y` funcionan igual que antes de H1), `deny`/
/// `cancel` rechazan. Excluye deliberadamente los comandos de
/// colisión/aprobación — un rebind de `w`→`dialog.newer` no hace nada aquí.
pub const ALLOW_CONFIRM: &[&str] = &[
    "dialog.approve",
    "dialog.confirm",
    "dialog.deny",
    "dialog.cancel",
];

/// ALLOWLIST de `Modal::Collision`: overwrite/skip/rename/newer eligen
/// política y reintentan; `cancel` cierra. Excluye A PROPÓSITO
/// `dialog.confirm`/`dialog.approve` — no hay respuesta inocua que Enter
/// deba disparar sola (decisión 4 del plan H1, igual que antes de H1).
pub const ALLOW_COLLISION: &[&str] = &[
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    "dialog.cancel",
];

/// ALLOWLIST de `Modal::ApproveAgentOp`: SOLO `approve` confirma; `deny` y
/// `cancel` deniegan (cerrar ES denegar, fail-safe). Excluye A PROPÓSITO
/// `dialog.confirm` — aprobar una mutación de AGENTE no es una respuesta
/// inocua que Enter deba disparar sola (decisión 2 del plan H1).
pub const ALLOW_APPROVAL: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST de `Modal::TrustHostKey` (TOFU SSH, #45): mismo principio que
/// [`ALLOW_APPROVAL`] — SOLO `approve` confía, `dialog.confirm` excluido a
/// propósito (Enter jamás confía en una host key sin verificar).
pub const ALLOW_TRUST_HOST: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST del selector de tema (`on_theme_picker_key`, main.rs): sin
/// riesgo de seguridad (elegir tema no muta nada fuera del propio popup),
/// así que `confirm` SÍ dispara (a diferencia de los modales de arriba).
/// Única lista de este overlay — dispatch (main.rs) y el hint generado
/// (H1 T3, `hints::DialogHints`) la comparten, jamás una copia.
pub const ALLOW_PICKER: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST del gestor de extensiones (`on_extensions_key`, main.rs,
/// M4-P3): `approve` togglea la aprobación del plugin (decisión 3 del plan
/// H1 — "aprobar un plugin" reutiliza `dialog.approve`), `toggle-enabled`
/// lo activa/desactiva. `confirm` (G3c) abre la sección de `[config]` del
/// plugin resaltado, SI declara alguna clave — Enter jamás aprueba (pin del
/// P1), solo entra en un submenú. Compartida por dispatch y el hint
/// generado.
pub const ALLOW_EXTENSIONS: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.approve",
    "dialog.toggle-enabled",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST del panel de `[config]` de un plugin (G3c, `on_extensions_key`
/// cuando `mgr.config.is_some()` y NO se está editando un buffer — mientras
/// se edita, las teclas se capturan RAW, mismo criterio que
/// `on_nav_popup_key`'s `name_input`): `up`/`down` mueven el cursor sobre
/// las claves, `confirm` cicla `bool`/`enum` o abre edición de
/// `string`/`int`, `cancel` cierra el panel (vuelve a la lista de plugins,
/// NO cierra el overlay entero).
pub const ALLOW_PLUGIN_CONFIG: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST del popup de navegación en modo HOTLIST (`on_nav_popup_key`,
/// main.rs): `add`/`remove` los filtra el caller a `kind == Hotlist` (el
/// historial no tiene nada que nombrar ni borrar — mismo criterio que antes
/// de H1); el hint (H1 T3) solo se pinta para `NavPopupKind::Hotlist`,
/// igual que el footer estático que sustituye. Compartida por dispatch y
/// el hint generado.
pub const ALLOW_NAV_HOTLIST: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.add",
    "dialog.remove",
    "dialog.cancel",
];

/// Mapea un comando `dialog.*` YA RESUELTO (por el
/// [`Resolver`](crate::keymap::Resolver) del efectivo `dialog`, H1 #24) al
/// desenlace del modal activo, filtrando por el ALLOWLIST del modal
/// concreto: `None` = comando fuera de allowlist, la tecla es INERTE para
/// este modal (p. ej. Enter — `dialog.confirm` — sobre una aprobación de
/// agente). La semántica de seguridad vive aquí, en código, jamás en el
/// keymap: un rebind solo cambia qué TECLA dispara `dialog.approve`, nunca
/// qué modales aceptan `dialog.approve` como confirmación.
///
/// `Modal::TrustLuaInit` no tiene allowlist — decisión 8 del plan H1, se
/// resuelve aparte con [`trust_lua_key`] — y devuelve `None` aquí siempre.
#[must_use]
pub fn dialog_action(modal: &Modal, cmd: &str) -> Option<DialogOutcome> {
    use norte_proto::CollisionPolicy as P;
    match modal {
        Modal::ConfirmDelete { .. } | Modal::ConfirmTransfer { .. } | Modal::ConfirmQuit => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" | "dialog.confirm" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        Modal::Collision { .. } => {
            if !ALLOW_COLLISION.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.overwrite" => DialogOutcome::Retry(P::Overwrite),
                "dialog.skip" => DialogOutcome::Retry(P::Skip),
                "dialog.rename" => DialogOutcome::Retry(P::RenameAuto),
                "dialog.newer" => DialogOutcome::Retry(P::Newer),
                _ => DialogOutcome::Cancelled, // dialog.cancel
            })
        }
        Modal::ApproveAgentOp { .. } => {
            if !ALLOW_APPROVAL.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        Modal::TrustHostKey { .. } => {
            if !ALLOW_TRUST_HOST.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        // #103 T9: `MarkPattern` es texto libre, como el diálogo de
        // búsqueda — el run loop lo intercepta ANTES de llegar aquí (raw
        // chars, jamás el contexto `dialog`), igual que `TrustLuaInit`.
        // Ambos devuelven `None` siempre.
        Modal::TrustLuaInit { .. } | Modal::MarkPattern { .. } | Modal::Mkdir { .. } => None,
    }
}

/// Resuelve el modal [`Modal::TrustLuaInit`] (decisión 8 del plan H1: NO
/// migrado al contexto `dialog` — es una ruta de resolución ESPECIAL que el
/// run loop intercepta ANTES de consultar el keymap, porque necesita el
/// `LuaHost` que solo vive ahí). Mismo contrato de seguridad que el resto de
/// diálogos TOFU: `y` confía, `n`/Esc deniegan, Enter NO decide (sin default
/// peligroso que se dispare solo).
#[must_use]
pub fn trust_lua_key(code: crossterm::event::KeyCode) -> DialogOutcome {
    use crossterm::event::KeyCode as K;
    match code {
        K::Char('y') => DialogOutcome::Confirmed,
        K::Char('n') | K::Esc => DialogOutcome::Cancelled,
        _ => DialogOutcome::Open,
    }
}

/// Clave Fluent ESTABLE de la CATEGORÍA de un [`Error`] del protocolo (spec
/// §17.7, #20). Es la base de [`error_category`] y también el vocabulario que
/// ven los scripts Lua (`nil, clave` — M4 Lua): el script compara contra
/// claves estables, jamás contra texto localizado. Los campos con detalle
/// (host, `rule`, retryable…) se DESCARTAN por patrón: `PolicyDenied` no
/// expone la regla concreta (vocabulario cerrado); `HostKeyUnknown`/
/// `Mismatch` no filtran el host (además un `Display` con host arbitrario
/// sería un vector bidi/control en la barra). Una categoría futura
/// (`Unknown`, cliente N-1) cae a `err-unknown`.
#[must_use]
pub fn error_key(e: &Error) -> &'static str {
    use norte_proto::ConflictKind;
    match e {
        Error::NotFound => "err-not-found",
        Error::PermissionDenied => "err-permission-denied",
        Error::Conflict { conflict } => match conflict {
            ConflictKind::Exists => "err-conflict-exists",
            ConflictKind::CaseCollision => "err-conflict-case",
            ConflictKind::Normalization => "err-conflict-normalization",
            ConflictKind::TypeMismatch => "err-conflict-type",
            _ => "err-conflict",
        },
        Error::ProviderUnavailable { .. } => "err-provider-unavailable",
        Error::NoSpace => "err-no-space",
        Error::Io { .. } => "err-io",
        Error::Cancelled => "err-cancelled",
        Error::PolicyDenied { .. } => "err-policy-denied",
        Error::EncodingLoss => "err-encoding-loss",
        Error::Unsupported => "err-unsupported",
        Error::InvalidPath => "err-invalid-path",
        Error::Internal { .. } => "err-internal",
        Error::Loop => "err-loop",
        Error::Corrupt => "err-corrupt",
        // #95.3: límite local ≠ corrupción. El sub-vocabulario (`entries`/
        // `decompressed-bytes`) es diagnóstico, no UX: una sola clave.
        Error::LimitExceeded { .. } => "err-limit-exceeded",
        Error::HostKeyUnknown { .. } => "err-host-key-unknown",
        Error::HostKeyMismatch { .. } => "err-host-key-mismatch",
        Error::CursorExpired => "err-cursor-expired",
        _ => "err-unknown",
    }
}

/// Texto LOCALIZADO de la categoría de un [`Error`] del protocolo: la clave
/// estable de [`error_key`] pasada por Fluent — jamás el `Display` inglés
/// hardcodeado ni un string del OS.
#[must_use]
pub fn error_category(e: &Error) -> String {
    t(error_key(e))
}

/// Mensaje de barra `error: <categoría>` (envuelve [`error_category`]).
#[must_use]
pub fn error_message(e: &Error) -> String {
    ta("msg-error", &[("error", &error_category(e))])
}

/// Tope del detalle diagnóstico en la barra (una línea; un TOML hostil puede
/// citar valores kilométricos).
pub const DETAIL_MAX_CHARS: usize = 160;

/// Detalle diagnóstico listo para la barra (#73): enmascarado como un nombre
/// ([`display_name`]: lossy marcado, sin controles/bidi/invisibles) y con
/// tope [`DETAIL_MAX_CHARS`] (recorte marcado con `…`).
#[must_use]
pub fn detail_for_bar(detail: &str) -> String {
    let (masked, _) = display_name(detail.as_bytes());
    let mut out: String = masked.chars().take(DETAIL_MAX_CHARS).collect();
    if masked.chars().nth(DETAIL_MAX_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// Categoría LOCALIZADA de un error de io LOCAL (#73): `ErrorKind` → clave
/// Fluent — jamás el `Display` del OS, que el SO localiza a su antojo
/// («Permission denied (os error 13)»; regla 1).
#[must_use]
pub fn io_error_category(e: &std::io::Error) -> String {
    let key = match e.kind() {
        std::io::ErrorKind::NotFound => "err-not-found",
        std::io::ErrorKind::PermissionDenied => "err-permission-denied",
        std::io::ErrorKind::StorageFull => "err-no-space",
        _ => "err-io",
    };
    t(key)
}

/// Categoría LOCALIZADA de un [`crate::config::ConfigError`] (#73): path
/// propio (lossy explícito + mask) y, en el caso TOML, el diagnóstico del
/// parser saneado por [`detail_for_bar`] — la posición («at line N») es lo
/// accionable. El io subyacente va por [`io_error_category`].
#[must_use]
pub fn config_error_category(e: &crate::config::ConfigError) -> String {
    use crate::config::ConfigError;
    match e {
        ConfigError::Io { path, source } => ta(
            "err-config-io",
            &[
                ("path", &detail_for_bar(&path.display().to_string())),
                ("error", &io_error_category(source)),
            ],
        ),
        ConfigError::Toml { path, message } => ta(
            "err-config-parse",
            &[
                ("path", &detail_for_bar(&path.display().to_string())),
                ("detail", &detail_for_bar(message)),
            ],
        ),
    }
}

/// Categoría LOCALIZADA de un [`crate::theme::ResolveError`] (#73), espejo
/// de [`config_error_category`]. El `spec` puede venir de la capa `./.norte`
/// de un repo AJENO: siempre por [`detail_for_bar`].
#[must_use]
pub fn theme_error_category(e: &crate::theme::ResolveError) -> String {
    use crate::theme::ResolveError;
    match e {
        ResolveError::Io { spec, source } => ta(
            "err-config-io",
            &[
                ("path", &detail_for_bar(spec)),
                ("error", &io_error_category(source)),
            ],
        ),
        ResolveError::Parse { spec, detail } => ta(
            "err-config-parse",
            &[
                ("path", &detail_for_bar(spec)),
                ("detail", &detail_for_bar(detail)),
            ],
        ),
    }
}

/// Error tipado del montaje de keymaps (#73): cada variante mapea a una
/// clave Fluent en [`keymaps_error_category`] — nada de contextos anyhow
/// castellanos hardcodeados en la barra. El `Display` (thiserror) solo sale
/// por stderr en el arranque, antes de levantar la TUI.
#[derive(Debug, thiserror::Error)]
pub enum KeymapsError {
    /// El preset pedido (CLI o config) no existe.
    #[error("preset desconocido {name:?}; disponibles: {available}")]
    UnknownPreset {
        /// Lo pedido.
        name: String,
        /// Los que sí existen, ya unidos para display.
        available: String,
    },
    /// Una capa de keymap no valida contra los comandos.
    #[error("keymap inválido: {detail}")]
    Invalid {
        /// Diagnóstico del validador ([`crate::keymap::KeymapError`]).
        detail: String,
    },
}

/// Categoría LOCALIZADA de un [`KeymapsError`] (#73).
#[must_use]
pub fn keymaps_error_category(e: &KeymapsError) -> String {
    match e {
        KeymapsError::UnknownPreset { name, available } => ta(
            "err-keymap-preset-unknown",
            &[("name", &detail_for_bar(name)), ("available", available)],
        ),
        KeymapsError::Invalid { detail } => {
            ta("err-keymap-invalid", &[("detail", &detail_for_bar(detail))])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{EntryKind, Scheme};

    fn root() -> VPath {
        VPath::root(Scheme::new("mem").unwrap(), None)
    }

    fn file(name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: root().join(norte_proto::Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    fn names(p: &Pane) -> Vec<String> {
        p.entries()
            .iter()
            .map(|e| String::from_utf8_lossy(e.path.file_name().unwrap().as_bytes()).into_owned())
            .collect()
    }

    /// `extend_listing` re-ordena TODO el listado (primera página + lote).
    #[test]
    fn extend_reordena_todo() {
        let mut first = vec![file("b.txt"), file("d.txt")];
        sort_entries(&mut first);
        let mut p = Pane::new(root(), first);
        p.set_loading(true);
        p.extend_listing(vec![file("a.txt"), file("c.txt")]);
        assert_eq!(names(&p), vec!["a.txt", "b.txt", "c.txt", "d.txt"]);
    }

    /// El cursor se re-ancla al PATH seleccionado, no al índice: rellenar no
    /// mueve la selección del usuario bajo sus pies.
    #[test]
    fn extend_reancla_el_cursor_por_path() {
        let mut first = vec![file("m.txt"), file("z.txt")];
        sort_entries(&mut first);
        let mut p = Pane::new(root(), first);
        p.set_cursor(1); // "z.txt"
        // Llega un lote de nombres que ordenan ANTES: z.txt se desplaza.
        p.extend_listing(vec![file("a.txt"), file("b.txt")]);
        assert_eq!(names(&p), vec!["a.txt", "b.txt", "m.txt", "z.txt"]);
        assert_eq!(
            p.selected().unwrap().path.file_name().unwrap().as_bytes(),
            b"z.txt"
        );
    }

    /// Un lote vacío no altera nada (fin del drenado sin cola).
    #[test]
    fn extend_vacio_es_noop() {
        let mut p = Pane::new(root(), vec![file("a.txt")]);
        p.set_cursor(0);
        p.extend_listing(vec![]);
        assert_eq!(names(&p), vec!["a.txt"]);
        assert_eq!(p.cursor(), 0);
    }

    /// `finish_listing` limpia el flag de carga.
    #[test]
    fn finish_limpia_loading() {
        let mut p = Pane::new(root(), vec![]);
        p.set_loading(true);
        p.finish_listing();
        assert!(!p.loading());
    }

    /// #52: `focused_needs_stat` señala la entrada File enfocada SIN `size`
    /// (candidata a la sonda lazy). Ya hidratada o siendo un Dir, no aplica.
    #[test]
    fn focused_needs_stat_solo_file_lazy() {
        let mut lazy = file("a.txt");
        lazy.size = None;
        let mut app = App::new(
            Pane::new(root(), vec![lazy.clone()]),
            Pane::new(root(), vec![]),
        );
        assert_eq!(
            app.focused_needs_stat(),
            Some((0, lazy.path.clone())),
            "File sin size es candidato"
        );

        // Ya hidratada: deja de ser candidata.
        app.panes[0].hydrate(&lazy.path, Some(5), None);
        assert!(app.focused_needs_stat().is_none(), "ya tiene size");

        // Un Dir jamás se sondea, aunque venga sin size.
        let mut dir_lazy = file("b");
        dir_lazy.kind = EntryKind::Dir;
        dir_lazy.size = None;
        app.panes[0] = Pane::new(root(), vec![dir_lazy]);
        assert!(app.focused_needs_stat().is_none(), "un Dir no se sondea");
    }

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Pane sobre `mem://` con archivos nombrados como se pida: #54, `Pane`
    /// (vía `PaneState::new`) normaliza el orden internamente (dirs primero,
    /// NFC, empate por bytes) — los tests del quick search razonan sobre el
    /// índice real YA ORDENADO, no sobre el orden de llegada de `names`.
    fn pane_con(names: &[&str]) -> Pane {
        Pane::new(root(), names.iter().map(|n| file(n)).collect())
    }

    /// Filtro activo: `selected()` (la base de F5/F8/F3…) apunta a la
    /// selección DENTRO del filtro; cancelar restaura el listado completo
    /// con el cursor real donde estaba (el filtro jamás lo movió).
    #[test]
    fn quick_filter_redirige_seleccion_y_ops() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a1"),
            "selected respeta el filtro"
        );
        p.quick_down();
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
        p.quick_cancel();
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a1"),
            "restaurado: cursor al último real"
        );
    }

    /// Confirmar fija el cursor REAL a lo seleccionado en el filtro y cierra
    /// (Enter: la op siguiente —cd, view— parte de ese cursor).
    #[test]
    fn quick_confirm_fija_el_cursor_real() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.quick_down();
        p.quick_confirm();
        assert!(p.quick().is_none(), "confirmar cierra el quick search");
        // #54: normalizado, el orden real es [a1, a2, b] — a2 al índice 1.
        assert_eq!(p.cursor(), 1, "cursor real = índice real de a2");
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
    }

    /// Un lote nuevo del fill re-aplica el filtro (spec: al llegar lotes
    /// nuevos el filtro se re-aplica, no se congela).
    #[test]
    fn extend_listing_reaplica_el_filtro() {
        let mut p = pane_con(&["a1"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.extend_listing(vec![file("a2"), file("zz")]);
        assert_eq!(p.quick_visible().unwrap().len(), 2, "a2 entra, zz no");
    }

    /// review MAJOR T4: con el filtro SIN matches la pantalla lista vacío —
    /// Enter jamás debe actuar sobre la entrada del cursor real (invisible
    /// para el usuario). `quick_confirm` devuelve false y no toca el cursor.
    #[test]
    fn enter_sin_matches_no_actua_sobre_entrada_invisible() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.set_cursor(1);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('x'); // cero matches
        assert!(p.selected().is_none(), "sin matches no hay selección");
        assert!(
            !p.quick_confirm(),
            "confirmar sin matches NO fija selección"
        );
        assert!(p.quick().is_none(), "el quick search sí se cierra");
        assert_eq!(p.cursor(), 1, "el cursor real queda intacto");
    }

    /// Modo salto: el listado NO cambia; teclear mueve el cursor REAL al
    /// primer match y Tab (`quick_next`) al siguiente con wrap.
    #[test]
    fn quick_jump_mueve_el_cursor_real() {
        // #54: normalizado, el orden real es [ab, ac, zz] — ab y ac casan.
        let mut p = pane_con(&["ab", "zz", "ac"]);
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('a');
        assert_eq!(p.cursor(), 0, "salta al primer match");
        assert!(
            p.quick_visible().is_none(),
            "en salto el listado queda intacto"
        );
        p.quick_next();
        assert_eq!(p.cursor(), 1, "Tab: siguiente match");
        p.quick_next();
        assert_eq!(p.cursor(), 0, "wrap");
        assert_eq!(p.selected().unwrap().path, vp("mem:///ab"));
    }

    /// El contrato de `QuickSearch::refresh` (T1) de punta a punta:
    /// `extend_listing` RE-SORTEA el listado entero, así que la selección
    /// del filtro se conserva por PATH, jamás por índice.
    #[test]
    fn extend_con_resort_conserva_seleccion_por_path() {
        let mut p = pane_con(&["a1", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.quick_down(); // selecciona a2 (índice real 1)
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
        // "a0" ordena ANTES: a2 pasa del índice real 1 al 2 tras el sort.
        p.extend_listing(vec![file("a0")]);
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a2"),
            "la selección sigue en el MISMO path tras el resort"
        );
    }

    /// Edge de T4 (review): en Jump con query SIN matches el listado se
    /// pinta ENTERO — el cursor real es visible por definición, así que
    /// Enter SÍ puede operar sobre él (en Filter sigue siendo `false`).
    #[test]
    fn enter_en_jump_sin_matches_opera_sobre_el_cursor_visible() {
        let mut p = pane_con(&["a1", "b"]);
        p.set_cursor(1);
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('x'); // cero matches; el listado no cambió
        assert!(
            p.quick_confirm(),
            "en Jump el cursor real ES visible: Enter opera"
        );
        assert!(p.quick().is_none(), "el quick search se cierra");
        assert_eq!(p.cursor(), 1, "el cursor real queda donde estaba");

        // Con el pane VACÍO ni Jump confirma (no hay nada visible).
        let mut vacio = pane_con(&[]);
        vacio.quick_start(crate::nav::Mode::Jump);
        assert!(
            !vacio.quick_confirm(),
            "sin entradas no hay nada que operar"
        );
    }

    fn app_dos_panes() -> App {
        App::new(pane_con(&["a"]), pane_con(&["b"]))
    }

    /// Popup de historial (spec 2026-07-18): navegación con `PickerAction`,
    /// Confirm devuelve el destino y cierra, Cancel cierra.
    #[test]
    fn nav_popup_historial_navega_confirma_y_cancela() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///uno"));
        app.history[0].push(vp("mem:///dos"));
        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup.as_ref().unwrap().items().len(), 2);
        assert_eq!(
            app.nav_popup.as_ref().unwrap().selected().unwrap().target,
            Some(vp("mem:///dos")),
            "más reciente primero"
        );
        assert_eq!(app.nav_popup_input(PickerAction::Down), None);
        assert_eq!(
            app.nav_popup_input(PickerAction::Confirm),
            Some(vp("mem:///uno")),
            "Confirm devuelve el destino del item resaltado"
        );
        assert!(app.nav_popup.is_none(), "Confirm cierra el popup");

        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup_input(PickerAction::Cancel), None);
        assert!(app.nav_popup.is_none(), "Cancel cierra el popup");
    }

    /// Un favorito INVÁLIDO (path que no parsea) se muestra con su aviso y
    /// destino `None`: Confirm sobre él es no-op (el popup sigue abierto).
    #[test]
    fn nav_popup_hotlist_item_invalido_no_confirma() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "rota".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let item = app.nav_popup.as_ref().unwrap().selected().unwrap().clone();
        assert!(item.target.is_none(), "inválida no navega");
        assert!(
            item.display.contains(&norte_i18n::t("hotlist-invalid")),
            "el aviso de inválida se pinta: {}",
            item.display
        );
        assert_eq!(app.nav_popup_input(PickerAction::Confirm), None);
        assert!(app.nav_popup.is_some(), "el popup NO se cierra");
    }

    /// `a` abre el input de nombre SOLO en hotlist; Cancel con input activo
    /// cierra el input (no el popup). `d`: el name CRUDO seleccionado sirve
    /// de clave y el borrado local refresca los items.
    #[test]
    fn nav_popup_hotlist_input_y_borrado() {
        let mut app = app_dos_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "uno".into(),
                target: Ok(vp("mem:///uno")),
            },
            crate::config::HotlistItem {
                name: "dos".into(),
                target: Ok(vp("mem:///dos")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some(""),
            "`a` abre el input prellenado vacío"
        );
        app.nav_popup_input(PickerAction::Cancel);
        let p = app.nav_popup.as_ref().unwrap();
        assert!(p.name_input.is_none(), "Cancel cierra el input");
        assert!(app.nav_popup.is_some(), "…no el popup");

        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("uno"),
            "el name CRUDO del seleccionado (clave del persist)"
        );
        app.hotlist_apply_removed("uno");
        assert_eq!(app.hotlist.len(), 1);
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.items().len(), 1, "el popup se refresca tras borrar");
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///dos")));
    }

    /// review MAJOR T5: un hot-reload con el popup abierto muta
    /// `App.hotlist` mientras el usuario ve la snapshot VIEJA (items
    /// congelados a propósito) — `d` debe borrar lo MOSTRADO (clave
    /// congelada en el item), jamás lo que ahora ocupa ese índice en la
    /// lista nueva (borraría OTRO favorito: pérdida de config).
    #[test]
    fn d_con_popup_desincronizado_borra_el_mostrado() {
        let mut app = app_dos_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "uno".into(),
                target: Ok(vp("mem:///uno")),
            },
            crate::config::HotlistItem {
                name: "dos".into(),
                target: Ok(vp("mem:///dos")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        // Cursor en 0: el usuario VE "uno". Simula el hot-reload que quitó
        // "uno" de la config (la copia en App cambia, el popup no).
        app.hotlist.remove(0);
        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("uno"),
            "la clave es la CONGELADA del popup, no App.hotlist[cursor]"
        );
    }

    /// En el popup de HISTORIAL no hay input de nombre ni name de hotlist.
    #[test]
    fn nav_popup_historial_sin_input_ni_name() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///uno"));
        app.open_nav_popup(NavPopupKind::History);
        app.nav_popup_open_name_input();
        assert!(app.nav_popup.as_ref().unwrap().name_input.is_none());
        assert_eq!(app.nav_popup_selected_hotlist_name(), None);
    }

    /// `hotlist_apply_saved` reemplaza por name conservando posición o
    /// añade al final (misma semántica que persist/load) y refresca popup.
    #[test]
    fn hotlist_apply_saved_reemplaza_o_anade() {
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "uno".into(),
            target: Ok(vp("mem:///viejo")),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.hotlist_apply_saved("uno", vp("mem:///nuevo"));
        assert_eq!(app.hotlist.len(), 1, "reemplaza, no duplica");
        assert_eq!(app.hotlist[0].target.as_ref().unwrap(), &vp("mem:///nuevo"));
        app.hotlist_apply_saved("dos", vp("mem:///dos"));
        assert_eq!(app.hotlist.len(), 2, "name nuevo se añade al final");
        assert_eq!(
            app.nav_popup.as_ref().unwrap().items().len(),
            2,
            "el popup abierto refleja el alta"
        );
    }

    /// Un path HOSTIL en el historial sale enmascarado y con el badge como
    /// prefijo — jamás bidi/controles crudos en el popup (spec §6).
    #[test]
    fn nav_popup_sanea_paths_hostiles() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///evil%E2%80%AEdir"));
        app.open_nav_popup(NavPopupKind::History);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// review MINOR T5: una entrada INVÁLIDA con name hostil también lleva
    /// el badge (antes el flag de `display_name` se descartaba en ese brazo).
    #[test]
    fn hotlist_invalida_con_name_hostil_lleva_badge() {
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "evil\u{202E}name".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// TOFU (#45): confiar es decisión de seguridad — `dialog.approve`
    /// confía; `dialog.deny`/`dialog.cancel` cancelan; `dialog.confirm`
    /// (Enter) es INERTE (safety pin H1: sin default peligroso).
    #[test]
    fn trust_host_key_solo_approve_confia() {
        let m = Modal::TrustHostKey {
            host: "h".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            dir: root(),
        };
        assert_eq!(
            dialog_action(&m, "dialog.approve"),
            Some(DialogOutcome::Confirmed)
        );
        for cmd in ["dialog.deny", "dialog.cancel"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Cancelled));
        }
        assert_eq!(
            dialog_action(&m, "dialog.confirm"),
            None,
            "Enter (dialog.confirm) jamás confía en una host key"
        );
    }

    /// S2 (`[ui] confirm_quit`): `Modal::ConfirmQuit` reutiliza el ALLOWLIST
    /// de `ConfirmDelete`/`ConfirmTransfer` — `y`/Enter confirman (cierran),
    /// `n`/Esc cancelan, cualquier otro comando queda fuera (`None`).
    #[test]
    fn confirm_quit_reutiliza_allow_confirm() {
        let m = Modal::ConfirmQuit;
        for cmd in ["dialog.approve", "dialog.confirm"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Confirmed));
        }
        for cmd in ["dialog.deny", "dialog.cancel"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Cancelled));
        }
        assert_eq!(
            dialog_action(&m, "dialog.overwrite"),
            None,
            "fuera del allowlist de confirm: inerte"
        );
    }

    /// S2 (`[ui] confirm_quit`): las tres combinaciones modo × trabajo en
    /// vuelo, cada una por separado (mismo estilo que
    /// `has_pending_work_tasks_o_marcas_o_ninguno` de la GUI).
    #[test]
    fn quit_needs_confirm_los_tres_modos() {
        use crate::config::ConfirmQuit;
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, true),
            "never NUNCA confirma, ni con trabajo en vuelo"
        );
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, false),
            "never NUNCA confirma"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, false),
            "always SIEMPRE confirma, incluso sin trabajo pendiente"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, true),
            "always SIEMPRE confirma"
        );
        assert!(
            !quit_needs_confirm(ConfirmQuit::Auto, false),
            "auto sin trabajo pendiente: cierra directo"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Auto, true),
            "auto con trabajo pendiente: confirma (comportamiento pre-S2)"
        );
    }

    /// TOFU Lua (M4, decisión 8 del plan H1: NO migrado): mismo contrato de
    /// seguridad que el resto — solo `y` confía; `n` y Esc deniegan; Enter
    /// NO decide.
    #[test]
    fn trust_lua_init_solo_y_confia_y_enter_no_decide() {
        use crossterm::event::KeyCode as K;
        assert_eq!(trust_lua_key(K::Char('y')), DialogOutcome::Confirmed);
        assert_eq!(trust_lua_key(K::Char('n')), DialogOutcome::Cancelled);
        assert_eq!(trust_lua_key(K::Esc), DialogOutcome::Cancelled);
        assert_eq!(
            trust_lua_key(K::Enter),
            DialogOutcome::Open,
            "Enter jamás aprueba ejecutar un script ajeno"
        );
    }

    /// Diálogo de búsqueda (liveSearch T6): Tab alterna el campo activo y los
    /// imprimibles/backspace caen en el campo con foco.
    #[test]
    fn search_dialog_tab_y_edicion_por_campo() {
        let mut d = SearchDialog::new();
        assert_eq!(d.field, SearchField::Name);
        d.push_char('*');
        d.push_char('x');
        assert_eq!(d.name, "*x");
        assert_eq!(d.content, "");
        d.toggle_field();
        assert_eq!(d.field, SearchField::Content);
        d.push_char('a');
        d.push_char('b');
        d.backspace();
        assert_eq!(d.content, "a");
        assert_eq!(d.name, "*x", "backspace solo tocó el campo activo");
        d.toggle_field();
        assert_eq!(d.field, SearchField::Name);
    }

    /// Los toggles (F2 regex / F3 case) alternan sus flags de forma
    /// independiente.
    #[test]
    fn search_dialog_toggles_regex_y_case() {
        let mut d = SearchDialog::new();
        assert!(!d.regex && !d.case);
        d.toggle_regex();
        assert!(d.regex && !d.case);
        d.toggle_case();
        assert!(d.regex && d.case);
        d.toggle_regex();
        assert!(!d.regex && d.case);
    }

    /// Validación del criterio: sin ningún campo no hay búsqueda; basta con
    /// uno (nombre O contenido) para que la haya.
    #[test]
    fn search_dialog_criterio_no_vacio() {
        let mut d = SearchDialog::new();
        assert!(!d.has_criteria(), "ambos vacíos: no lanza");
        d.push_char('*');
        assert!(d.has_criteria(), "solo nombre basta");
        let mut d = SearchDialog::new();
        d.toggle_field();
        d.push_char('a');
        assert!(d.has_criteria(), "solo contenido basta");
    }

    /// `begin_search` marca el pane como virtual, vacía las entries y resetea
    /// el estado a `Running`; `extend_listing` alimenta los hits SIN apagar el
    /// modo virtual (los hits siguen siendo de una búsqueda).
    #[test]
    fn begin_search_marca_virtual_y_extend_conserva() {
        let mut p = pane_con(&["basura"]);
        p.begin_search(root());
        assert!(p.virtual_search);
        assert_eq!(p.search_state, SearchState::Running);
        assert!(p.entries().is_empty(), "los hits empiezan vacíos");
        p.extend_listing(vec![file("hit1"), file("hit2")]);
        assert!(p.virtual_search, "extend no apaga el modo virtual");
        assert_eq!(names(&p), vec!["hit1", "hit2"]);
    }

    /// Un listado NORMAL (cd/refresh) apaga el modo virtual de búsqueda.
    #[test]
    fn listados_normales_apagan_el_modo_virtual() {
        let mut p = pane_con(&[]);
        p.begin_search(root());
        assert!(p.virtual_search);
        p.begin_listing(root(), vec![file("a")], false, None);
        assert!(!p.virtual_search, "begin_listing apaga virtual");

        p.begin_search(root());
        p.set_listing(root(), vec![file("a")]);
        assert!(!p.virtual_search, "set_listing apaga virtual");

        p.begin_search(root());
        p.refresh_listing(vec![file("a")]);
        assert!(!p.virtual_search, "refresh_listing apaga virtual");
    }

    fn e(wire: &str, k: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }

    /// #104: el modal de F7 valida con las reglas del `VPath` y devuelve el
    /// destino completo; inválido = diagnóstico en el modal, jamás submit.
    #[test]
    fn el_modal_mkdir_valida_y_construye_el_destino() {
        let mut app = app_with_entries(&["a"]);
        app.open_mkdir();
        for c in "docs".chars() {
            app.mkdir_push(c);
        }
        let target = app.mkdir_confirm().expect("nombre válido");
        assert_eq!(target, VPath::parse("mem:///docs").unwrap());
        assert!(
            app.modal.is_some(),
            "confirmar NO cierra: cierra el submit que encoló (MINOR-1)"
        );
        // Un submit fallido deja el diagnóstico y conserva el nombre…
        app.mkdir_set_error("policy".into());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), name }) if name == "docs"
        ));
        // …y el que encoló, cierra.
        app.mkdir_submitted();
        assert!(app.modal.is_none(), "submitted cierra el modal");

        // Vacío: error, modal abierto.
        app.open_mkdir();
        assert!(app.mkdir_confirm().is_none());
        assert!(
            matches!(&app.modal, Some(Modal::Mkdir { error: Some(_), .. })),
            "el diagnóstico queda en el modal"
        );

        // `..` es DotSegment: jamás un destino.
        app.mkdir_push('.');
        app.mkdir_push('.');
        assert!(app.mkdir_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), .. })
        ));

        // `/` embebido: InvalidByte.
        app.cancel_mkdir();
        app.open_mkdir();
        for c in "a/b".chars() {
            app.mkdir_push(c);
        }
        assert!(app.mkdir_confirm().is_none());

        // Cancelar cierra sin nada.
        app.cancel_mkdir();
        assert!(app.modal.is_none());
    }

    /// #107 review MAJOR-1: los hits de una búsqueda son EXPLÍCITOS — el
    /// pane virtual suspende el filtro de ocultos. Con `[ui] show_hidden =
    /// false`, buscar "env" DEBE enseñar `.env`: tragárselo en silencio
    /// (mientras el contador de hits decía 1 sobre un pane vacío) era el
    /// bug. Al volver a un listado real, la preferencia vuelve a mandar.
    #[test]
    fn el_pane_virtual_de_busqueda_ensena_hits_ocultos() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///.env", EntryKind::File),
                e("mem:///a", EntryKind::File),
            ],
        );
        p.set_show_hidden(false); // seed de config: ocultar
        assert_eq!(p.entries().len(), 1, "el listado real filtra");

        p.begin_search(VPath::parse("mem:///").unwrap());
        p.extend_listing(vec![e("mem:///sub/.env", EntryKind::File)]);
        assert_eq!(
            p.entries().len(),
            1,
            "el hit oculto ES visible en el pane virtual"
        );

        // Ctrl+H dentro del pane virtual filtra los RESULTADOS…
        p.toggle_hidden();
        assert_eq!(p.entries().len(), 0);

        // …pero NO toca la preferencia: el listado real vuelve filtrando
        // (y un toggle en el real sí la cambia).
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///.env", EntryKind::File),
                e("mem:///a", EntryKind::File),
            ],
        );
        assert_eq!(p.entries().len(), 1, "la preferencia (ocultar) manda");
        p.toggle_hidden();
        assert_eq!(p.entries().len(), 2, "toggle real: mostrar");
        p.begin_listing(
            VPath::parse("mem:///sub").unwrap(),
            vec![
                e("mem:///sub/.git", EntryKind::Dir),
                e("mem:///sub/x", EntryKind::File),
            ],
            false,
            None,
        );
        assert_eq!(
            p.entries().len(),
            2,
            "begin_listing respeta la preferencia nueva (mostrar)"
        );
    }

    /// #103 review MAJOR-4: `Pane` delega la API de marcas en `PaneState`
    /// sin reimplementar nada — pero el set de partida debe ser ASIMÉTRICO
    /// en cada paso, o `mark_all`/`invert_marks`/`clear_marks` quedan
    /// indistinguibles entre sí (p. ej. sobre un set vacío, `mark_all` e
    /// `invert_marks` dan el mismo resultado). Cada aserción de abajo
    /// falsaría si esa llamada se sustituyera por CUALQUIER otra delegada.
    #[test]
    fn pane_delegates_the_mark_api() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        let a = p.entries()[0].clone();
        let b = p.entries()[1].clone();
        let c = p.entries()[2].clone();

        // toggle_mark: marca SOLO la entrada bajo el cursor ("a").
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&a) && !p.is_marked(&b) && !p.is_marked(&c));

        // mark_all desde {a}: las TRES, incluida "a" — si esto llamara a
        // invert_marks en su lugar, "a" se desmarcaría y el total sería 2.
        p.mark_all();
        assert_eq!(p.marks_len(), 3);
        assert!(p.is_marked(&a) && p.is_marked(&b) && p.is_marked(&c));

        // Reset a un set asimétrico de nuevo para poder distinguir invert.
        p.clear_marks();
        p.toggle_mark(); // {a}

        // invert_marks desde {a}: exactamente LAS OTRAS DOS — ni el set
        // vacío que daría clear_marks, ni las tres que daría mark_all.
        p.invert_marks();
        assert_eq!(p.marks_len(), 2);
        assert!(!p.is_marked(&a) && p.is_marked(&b) && p.is_marked(&c));

        // clear_marks desde {b, c}: vacío — invert_marks aquí daría {a}
        // (marks_len 1), mark_all daría 3.
        p.clear_marks();
        assert_eq!(p.marks_len(), 0);
    }

    /// El dispatch real de `mark.toggle` (main.rs) es una ÚNICA llamada a
    /// `toggle_mark_and_advance` (#103 review MAJOR-2: la composición
    /// "marca + avanza" ya no se parte en dos llamadas del dispatch —
    /// vive entera en el modelo compartido, que decide avanzar dentro del
    /// filtro o el cursor real; ver
    /// `norte_frontend::pane::tests::toggle_mark_and_advance_stays_inside_an_active_filter`
    /// para el caso con filtro). `dispatch` en sí no es testeable aquí sin
    /// un daemon real (pide `&Backend`/`&mut EventStream`), así que este
    /// test pinea la misma llamada al nivel de `Pane`: marca Y avanza, y en
    /// la última fila no envuelve.
    #[test]
    fn mark_toggle_advances_without_wrapping_at_the_end() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        let a = p.entries()[0].clone();
        let b = p.entries()[1].clone();
        assert_eq!(p.cursor(), 0);

        p.toggle_mark_and_advance();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&a), "la fila 0 quedó marcada");
        assert_eq!(p.cursor(), 1, "el cursor avanzó tras marcar");

        // Última fila: togglear + avanzar NO debe envolver a 0.
        p.toggle_mark_and_advance();
        assert_eq!(p.marks_len(), 2);
        assert!(p.is_marked(&b), "la fila 1 (última) también quedó marcada");
        assert_eq!(p.cursor(), 1, "clampado en la última fila, no envuelve");
    }

    /// App de un solo listado de nombres, sobre `mem://` (task 9, #103):
    /// vía `pane_con` — mismo orden real que pinta la UI — con foco en el
    /// pane lleno; el otro vacío. Nombre distinto de `app_with_sized_entries`
    /// (`tests/status_marks.rs`): esa lleva tamaño explícito, esta solo
    /// nombres.
    fn app_with_entries(names: &[&str]) -> App {
        App::new(pane_con(names), Pane::new(root(), Vec::new()))
    }

    /// Como [`app_with_entries`], con el pane INACTIVO plantado en `dst`
    /// (vacío): el destino ortodoxo de F5/F6 es el DIRECTORIO del otro pane
    /// (#103 T10), así que los tests del lote necesitan un destino distinto
    /// de la raíz de origen.
    fn app_with_two_panes(names: &[&str], dst: &str) -> App {
        App::new(
            pane_con(names),
            Pane::new(VPath::parse(dst).unwrap(), Vec::new()),
        )
    }

    /// #103 T10: F5/F6 construyen el modal desde TODAS las marcas, y el
    /// destino es el DIRECTORIO del otro pane (con varios ítems no hay un
    /// nombre único que editar — eso es #105).
    #[test]
    fn copy_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3, "las tres quedaron marcadas");
        app.open_transfer_modal(TransferKind::Copy);
        let Some(Modal::ConfirmTransfer { items, to, .. }) = &app.modal else {
            panic!("no transfer modal");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(to, &VPath::parse("mem:///dst").unwrap());
    }

    /// Sin ninguna marca, F5 sigue operando sobre el CURSOR (el gesto
    /// clásico no se pierde) — `marked_paths` cae al seleccionado.
    #[test]
    fn copy_without_marks_still_uses_the_cursor_entry() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        assert_eq!(app.focused().marks_len(), 0, "sin marcas de partida");
        app.open_transfer_modal(TransferKind::Copy);
        let Some(Modal::ConfirmTransfer { items, .. }) = &app.modal else {
            panic!("no transfer modal");
        };
        assert_eq!(items.len(), 1, "marked_paths falls back to the cursor");
        assert_eq!(items[0], VPath::parse("mem:///a").unwrap());
    }

    /// Las marcas las CONSUME el ENVÍO del lote (mc/Total Commander): tras
    /// `consume_marks` no queda una selección a medio consumir.
    #[test]
    fn submitting_a_bulk_operation_consumes_the_marks() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 2, "marcadas antes de enviar");
        app.open_transfer_modal(TransferKind::Copy);
        app.consume_marks();
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// F8 sobre las marcas: el modal lleva el lote entero y el modo
    /// (papelera/permanente) que decidió el caller tras sondear la
    /// capability UNA vez.
    #[test]
    fn delete_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_delete_modal(true);
        let Some(Modal::ConfirmDelete { items, permanent }) = &app.modal else {
            panic!("no delete modal");
        };
        assert_eq!(items.len(), 3);
        assert!(*permanent);
    }

    /// Un pane VACÍO no abre modal: no hay nada que copiar ni que borrar
    /// (ni marcas ni cursor) — jamás un diálogo sobre un lote vacío.
    #[test]
    fn an_empty_pane_opens_no_bulk_modal() {
        let mut app = app_with_two_panes(&[], "mem:///dst");
        app.open_transfer_modal(TransferKind::Copy);
        assert!(app.modal.is_none(), "sin ítems no hay modal de copia");
        app.open_delete_modal(false);
        assert!(app.modal.is_none(), "sin ítems no hay modal de borrado");
    }

    /// #103 T9: el modal de patrón marca/desmarca y reporta cuántas marcas
    /// cambió — camino feliz (glob válido, matches reales).
    #[test]
    fn the_pattern_modal_marks_and_reports_how_many() {
        let mut app = app_with_entries(&["a.rs", "b.rs", "c.txt"]);
        app.open_mark_pattern(true);
        assert!(matches!(
            app.modal,
            Some(Modal::MarkPattern { mark: true, .. })
        ));
        app.mark_pattern_push('*');
        app.mark_pattern_push('.');
        app.mark_pattern_push('r');
        app.mark_pattern_push('s');
        let changed = app.mark_pattern_confirm().expect("valid glob");
        assert_eq!(changed, 2);
        assert!(app.modal.is_none());
        assert_eq!(app.focused().marks_len(), 2);
    }

    /// Un patrón inválido (glob que no compila) deja el modal ABIERTO con el
    /// diagnóstico — el usuario conserva lo tecleado para corregirlo — y no
    /// marca nada.
    #[test]
    fn an_invalid_pattern_keeps_the_modal_open_and_marks_nothing() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('[');
        assert!(app.mark_pattern_confirm().is_err());
        assert!(app.modal.is_some(), "the user keeps their text to fix it");
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// #103 T9 review MINOR: `Modal::MarkPattern` no tiene ALLOWLIST — es
    /// texto libre, el run loop lo intercepta ANTES del contexto `dialog`
    /// (main.rs). Esto pinea la mitad de seguridad de esa afirmación:
    /// NINGÚN comando del vocabulario `dialog.*`, ni siquiera
    /// `dialog.confirm` (Enter), puede confirmarlo a través de
    /// `dialog_action` — si alguna vez este modal se colara al contexto
    /// `dialog` por un bug de enrutado, seguiría siendo inerte ahí.
    #[test]
    fn dialog_action_es_siempre_none_para_mark_pattern() {
        let m = Modal::MarkPattern {
            mark: true,
            pattern: String::new(),
            error: None,
        };
        for cmd in crate::keymap::DIALOG_COMMANDS {
            assert_eq!(
                dialog_action(&m, cmd),
                None,
                "{cmd} no debe confirmar/cancelar MarkPattern vía dialog_action"
            );
        }
    }

    /// Cancelar (`cancel_mark_pattern`, el Esc de este modal de texto libre)
    /// no marca nada, aunque el usuario ya hubiera tecleado un patrón — y
    /// cierra el modal, la propiedad real que este test debía pinear.
    #[test]
    fn the_pattern_modal_cancels_without_marking() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('*');
        app.cancel_mark_pattern();
        assert!(app.modal.is_none(), "cancel closes the modal");
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// Review rust MAJOR M1: `cancel_mark_pattern` NO es un cierre genérico
    /// — con un `Modal::ApproveAgentOp` abierto (llegado, p. ej., mientras
    /// el usuario tecleaba un patrón que luego se sustituyó), debe dejarlo
    /// INTACTO. Cerrarlo sin el `policy.decide(approve: false)` async que
    /// hace `on_dialog_key` dejaría al agente sin respuesta hasta el TTL
    /// del daemon, y al humano sin volver a ver la pregunta.
    ///
    /// El guard es un `debug_assert!`: en ESTE build (test = dev,
    /// `debug-assertions` activas) panica ANTES de tocar `self.modal` — se
    /// captura con `catch_unwind` para poder comprobar el estado posterior
    /// en la misma aserción; en release sería un no-op y la función
    /// devolvería temprano igual, mismo resultado sobre el modal.
    #[test]
    fn cancel_mark_pattern_leaves_an_approval_modal_untouched() {
        let mut app = app_with_entries(&["a.rs"]);
        let approval = Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 7,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///proj/a".into(), "mem:///proj/b".into()],
                ttl_ms: 60_000,
            },
        };
        app.modal = Some(approval.clone());
        // Silencia el hook de pánico por defecto: el panic se captura y se
        // espera, no debe ensuciar la salida de este test con un backtrace.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.cancel_mark_pattern();
        }));
        std::panic::set_hook(prev_hook);
        assert!(
            result.is_err(),
            "el guard debe panicar en debug ante el mal uso"
        );
        assert_eq!(
            app.modal,
            Some(approval),
            "an approval modal must not be closeable without a decision"
        );
    }
}

#[cfg(test)]
mod error_message_tests {
    use super::{
        DETAIL_MAX_CHARS, KeymapsError, config_error_category, detail_for_bar, error_message,
        io_error_category, keymaps_error_category, theme_error_category,
    };
    use crate::config;
    use norte_proto::{ConflictKind, Error};

    /// Cada categoría rinde un mensaje LOCALIZADO propio — jamás el `Display`
    /// inglés hardcodeado del proto (#20, spec §17.7).
    #[test]
    fn cada_categoria_tiene_mensaje_propio_no_display() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let nf = error_message(&Error::NotFound);
        assert!(nf.contains("no encontrado"), "localizado ES: {nf}");
        assert!(
            !nf.contains("not found"),
            "NO es el Display inglés del proto: {nf}"
        );
        // Las variantes de Conflict se distinguen entre sí.
        let exists = error_message(&Error::Conflict {
            conflict: ConflictKind::Exists,
        });
        let case = error_message(&Error::Conflict {
            conflict: ConflictKind::CaseCollision,
        });
        assert_ne!(exists, case, "cada ConflictKind rinde distinto");
        // PolicyDenied jamás filtra la regla concreta (vocabulario cerrado).
        let pd = error_message(&Error::PolicyDenied {
            rule: "scope-expired".into(),
        });
        assert!(
            !pd.contains("scope-expired"),
            "la regla concreta NO se muestra: {pd}"
        );
    }

    /// Una categoría futura desconocida cae a `err-unknown`, jamás vacía.
    #[test]
    fn categoria_desconocida_cae_a_unknown() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let u = error_message(&Error::Unknown);
        assert!(u.contains("unknown error"), "{u}");
    }

    /// `error_category` (la base de TODOS los renders de la barra) jamás
    /// filtra el host de un `HostKeyUnknown` — un host hostil con override
    /// bidi sería un spoof de la barra — ni la `rule` de un `PolicyDenied`.
    #[test]
    fn categoria_no_filtra_host_hostil_ni_rule() {
        use super::error_category;
        let hk = error_category(&Error::HostKeyUnknown {
            host: "evil\u{202E}host".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
        });
        assert!(!hk.contains("evil"), "el host NO se muestra: {hk:?}");
        assert!(!hk.contains('\u{202E}'), "sin bidi en la barra: {hk:?}");
        let pd = error_category(&Error::PolicyDenied {
            rule: "scope-expired".into(),
        });
        assert!(!pd.contains("scope-expired"), "la regla NO se filtra: {pd}");
    }

    /// La clave ESTABLE (`error_key`, la que ven los scripts Lua) y el texto
    /// localizado (`error_category`) son el MISMO mapa: cero duplicación,
    /// cero deriva entre lo que compara un script y lo que pinta la barra.
    #[test]
    fn error_key_es_la_clave_estable_de_la_categoria() {
        use super::{error_category, error_key};
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        assert_eq!(error_key(&Error::NotFound), "err-not-found");
        assert_eq!(error_key(&Error::Cancelled), "err-cancelled");
        assert_eq!(error_key(&Error::Unknown), "err-unknown");
        // La categoría es exactamente t(clave).
        assert_eq!(
            error_category(&Error::NotFound),
            norte_i18n::t("err-not-found")
        );
    }

    /// #73: un error LOCAL de io va por categoría Fluent — jamás el
    /// `Display` del OS («Permission denied (os error 13)», que el SO
    /// localiza a su antojo — regla 1).
    #[test]
    fn categoria_io_local_no_filtra_el_display_del_os() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let s = io_error_category(&e);
        assert!(s.contains("permission denied"), "{s}");
        assert!(!s.contains("os error"), "sin string del OS: {s}");
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        let s = io_error_category(&full);
        assert!(s.contains("no space"), "kind con clave propia: {s}");
    }

    /// #73: `ConfigError` rinde categoría localizada + path; el diagnóstico
    /// del parser se conserva (la posición es lo accionable) pero pasa por
    /// `display_name` — jamás bidi/controles crudos en la barra — y con tope.
    #[test]
    fn categoria_config_no_filtra_el_diagnostico_del_parser() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = config::ConfigError::Toml {
            path: "/etc/norte/config.toml".into(),
            message: "unknown field `colr\u{202E}` at line 3".into(),
        };
        let s = config_error_category(&e);
        assert!(s.contains("config.toml"), "el path SÍ se muestra: {s}");
        assert!(s.contains("line 3"), "la posición es lo accionable: {s}");
        assert!(!s.contains('\u{202E}'), "sin bidi en la barra: {s}");
        let e = config::ConfigError::Io {
            path: "/etc/norte/config.toml".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let s = config_error_category(&e);
        assert!(s.contains("permission denied"), "io por categoría: {s}");
        assert!(!s.contains("os error"), "sin string del OS: {s}");
    }

    /// #73 (ALTA-1 del encoding-auditor): el pipeline de TEMAS tenía el
    /// mismo bug — spec hostil (puede venir del `./.norte` de un repo AJENO)
    /// y Display del OS, crudos a la barra vía `apply_theme`.
    #[test]
    fn categoria_tema_no_filtra_spec_hostil_ni_os() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = crate::theme::ResolveError::Io {
            spec: "temas/\u{202E}x.toml".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let s = theme_error_category(&e);
        assert!(!s.contains("os error"), "sin string del OS: {s}");
        assert!(!s.contains('\u{202E}'), "sin bidi en la barra: {s}");
        assert!(s.contains("not found"), "io por categoría: {s}");
        let e = crate::theme::ResolveError::Parse {
            spec: "nord".into(),
            detail: "role `panel\u{202E}` desconocido".into(),
        };
        let s = theme_error_category(&e);
        assert!(s.contains("nord"), "el spec saneado sí se muestra: {s}");
        assert!(!s.contains('\u{202E}'), "detalle enmascarado: {s}");
    }

    /// #73: un diagnóstico kilométrico (un TOML hostil puede citar valores
    /// arbitrarios) sale RECORTADO — la barra es una línea.
    #[test]
    fn el_detalle_del_parser_tiene_tope() {
        let s = detail_for_bar(&"x".repeat(1000));
        assert!(s.chars().count() <= DETAIL_MAX_CHARS + 1, "{}", s.len());
        assert!(s.ends_with('…'), "recorte marcado: {s}");
    }

    /// #73: el error de keymaps se localiza por Fluent (los contextos anyhow
    /// castellanos hardcodeados violaban la convención de i18n).
    #[test]
    fn error_de_keymap_se_localiza_con_el_nombre_del_preset() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let s = keymaps_error_category(&KeymapsError::UnknownPreset {
            name: "vintage".into(),
            available: "cua, orthodox".into(),
        });
        assert!(s.contains("vintage"), "el nombre pedido es accionable: {s}");
        assert!(s.contains("cua, orthodox"), "y los disponibles: {s}");
        assert!(
            !s.contains("preset desconocido"),
            "sin castellano fijo: {s}"
        );
        let s = keymaps_error_category(&KeymapsError::Invalid {
            detail: "conflicto en F5".into(),
        });
        assert!(s.contains("invalid keymap"), "localizado: {s}");
        assert!(s.contains("F5"), "el detalle diagnóstico se conserva: {s}");
    }
}
