//! Estado puro del TUI (panes, cursor, presentación de nombres) y la
//! presentación de ERRORES para la barra (#73): categorías Fluent
//! ([`error_key`]/[`error_category`] y compañía) + saneado de detalle
//! ([`detail_for_bar`]). Máquina testeable sin terminal — el render (`ui`)
//! y el I/O (`main`) viven aparte; los scripts Lua (M4) consumen de aquí la
//! clave ESTABLE de [`error_key`].

use norte_i18n::{Lang, t, ta};
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

/// El estado del run (`CompareState`) y el panel abierto (`CompareView`)
/// viven en [`norte_frontend::compare`] (#158): la GUI necesita exactamente
/// esta máquina y no una reimplementada, que es como el CLI (fase A) y la
/// tool MCP (fase B) se equivocaron cada uno por su lado — ambos dieron por
/// completa una respuesta a la que le faltaban lotes. Ver
/// [`CompareState::Incomplete`] para la razón de que el cierre del canal no
/// baste.
pub use norte_frontend::compare::{CompareState, CompareView};

/// La frase para una negativa de
/// [`norte_frontend::sync::include_from_rows`].
///
/// Las tres se NIEGAN en vez de recortar: una selección que se encoge sola deja
/// al lector aprobando otra cosa —o el árbol entero, en el caso de la raíz—.
fn sync_include_message(e: &norte_frontend::sync::IncludeError) -> String {
    use norte_frontend::sync::IncludeError;
    match e {
        IncludeError::TooMany { marked, max } => ta(
            "msg-sync-too-many-marks",
            &[("n", &marked.to_string()), ("max", &max.to_string())],
        ),
        IncludeError::Unrooted => t("msg-sync-mark-outside-roots"),
        IncludeError::RootSelected => t("msg-sync-mark-is-the-root"),
    }
}

/// Las dos raíces de una sincronización y cómo se leen sus nombres.
///
/// Vive en [`norte_frontend::sync`] desde #161, con la función que las decide:
/// la GUI llegó a tener la MISMA regla escrita a mano (su brazo «el pane con
/// foco es el origen»), y dos copias de «qué árbol se sobrescribe» es la clase
/// de divergencia que produce un plan perfectamente plausible sobre el árbol
/// equivocado.
use norte_frontend::sync::SyncRoots;

/// El estado del run (`SyncRunState`) y el panel abierto (`SyncView`) viven en
/// [`norte_frontend::sync`] (#161, el mismo argumento que ya llevó
/// [`CompareView`] allí): la GUI necesita exactamente este envoltorio del run
/// y no uno reimplementado. C1 aprendió, a costa de una revisión de rama, que
/// mover el TIPO y dejar sus decisiones a mano en cada frontend es peor que no
/// moverlo — así que lo que viaja con él es el mapeo `TaskState` →
/// [`SyncRunState`] ([`SyncRunState::from_task_state`]) y el paquete de
/// actualizaciones de «se aprobó y arrancó `sync.apply`»
/// ([`SyncView::on_apply_started`]), no solo la struct.
pub use norte_frontend::sync::{SyncRunState, SyncView};

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

    /// Filas de listado pintadas en el último frame (#124) — delegado puro a
    /// [`norte_frontend::PaneState::set_viewport_rows`].
    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.state.set_viewport_rows(rows);
    }

    /// Cuántas filas mueve una página en este pane (#124) — delegado puro a
    /// [`norte_frontend::PaneState::page_step`].
    #[must_use]
    pub fn page_step(&self) -> usize {
        self.state.page_step()
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

    /// Paths VISIBLES sin `size` a `radius` filas del cursor (#52) —
    /// delegado puro a [`norte_frontend::PaneState::needs_stat_window`].
    #[must_use]
    pub fn needs_stat_window(&self, radius: usize) -> Vec<VPath> {
        self.state.needs_stat_window(radius)
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

    /// Celda de una columna `plugin:` (#117-follow-up) — delegado puro a
    /// [`norte_frontend::PaneState::plugin_cell`].
    #[must_use]
    pub fn plugin_cell(&self, display_id: &str, path: &VPath) -> Option<String> {
        self.state.plugin_cell(display_id, path)
    }

    /// Instala el lote de valores de columnas `plugin:` (#117-follow-up) —
    /// delegado puro a [`norte_frontend::PaneState::set_plugin_columns`].
    pub fn set_plugin_columns(
        &mut self,
        columns: std::collections::HashMap<String, std::collections::HashMap<VPath, String>>,
    ) {
        self.state.set_plugin_columns(columns);
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

    /// Cuántas veces ha MOVIDO índices el listado de este pane — delegado
    /// puro a [`norte_frontend::PaneState::listing_epoch`]. Lo lee el ratón
    /// para soltar un gesto cuyos índices ya no nombran lo que se pintó.
    #[must_use]
    pub fn listing_epoch(&self) -> u64 {
        self.state.listing_epoch()
    }

    /// Marca (o desmarca) UNA entrada por su índice. Delegado puro al
    /// primitivo que necesita el ctrl+click
    /// ([`norte_frontend::PaneState::set_mark`]).
    pub fn set_mark(&mut self, index: usize, marked: bool) {
        self.state.set_mark(index, marked);
    }

    /// Marca el rango entre dos índices, inclusive y en cualquier orden;
    /// devuelve cuántas marcas cambió. ADITIVO. Delegado puro a
    /// [`norte_frontend::PaneState::mark_range`].
    pub fn mark_range(&mut self, from: usize, to: usize) -> usize {
        self.state.mark_range(from, to)
    }

    /// Arma un barrido de puntero. Delegado puro a
    /// [`norte_frontend::PaneState::begin_sweep`].
    pub fn begin_sweep(&mut self) {
        self.state.begin_sweep();
    }

    /// Fija la extensión ACTUAL de un barrido (rubber-band: devuelve lo que
    /// deja de cubrir). Delegado puro a
    /// [`norte_frontend::PaneState::apply_sweep`].
    pub fn apply_sweep(&mut self, from: usize, to: usize) -> usize {
        self.state.apply_sweep(from, to)
    }

    /// Devuelve lo que marcó el barrido en curso, dejándolo armado.
    /// Delegado puro a [`norte_frontend::PaneState::revert_sweep`].
    pub fn revert_sweep(&mut self) {
        self.state.revert_sweep();
    }

    /// Cierra un barrido, soltando su baseline. Delegado puro a
    /// [`norte_frontend::PaneState::end_sweep`].
    pub fn end_sweep(&mut self) {
        self.state.end_sweep();
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

    /// El orden activo del listado (#108). Delegado puro.
    #[must_use]
    pub fn sort(&self) -> norte_frontend::SortSpec {
        self.state.sort()
    }

    /// Cambia el orden del listado (#108). Delegado puro.
    pub fn set_sort(&mut self, spec: norte_frontend::SortSpec) {
        self.state.set_sort(spec);
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

/// Comando externo que `pane.open` (F4) dejó resuelto y el run loop lanzará
/// (#28). Se separa la resolución del lanzamiento porque el dueño de la
/// terminal es el run loop, no el despacho.
pub struct PendingOpen {
    /// Binario a sondear en el `PATH` antes de lanzar nada.
    pub program: String,
    /// argv completo, con el binario en `[0]` y las rutas byte-exactas.
    pub argv: Vec<std::ffi::OsString>,
    /// `true` cuando es el lanzador del escritorio (`xdg-open`/`open`/
    /// `explorer.exe`): entrega el fichero al programa asociado y vuelve
    /// enseguida, así que la TUI **no** se suspende — hacerlo pintaría un
    /// parpadeo de pantalla completa para nada. `false` es un opener
    /// declarado en `ns.toml`, que puede ser `bat` o un editor y necesita la
    /// terminal entera para sí.
    pub detached: bool,
    /// El directorio del pane con el foco, que el hijo recibe como cwd
    /// (#144).
    ///
    /// Los tres comandos de shell (#135) ya lo pasaban y los openers no, así
    /// que un editor abierto sobre un fichero del pane heredaba el cwd de
    /// norte y guardaba donde no se estaba mirando. Se dejó así a propósito
    /// en la ola de shell —cambiarlo cambia comportamiento— y se decidió el
    /// 2026-08-14: pasa el del pane. `None` solo si la ruta no convierte a
    /// nativa, donde no hay nada mejor que heredar.
    pub cwd: Option<std::path::PathBuf>,
}

/// Una SUSPENSIÓN que el despacho resolvió y el run loop ejecutará (#135).
///
/// Mismo reparto que [`PendingOpen`] y por la misma razón: quien es dueño de
/// la terminal es el run loop, no el despacho. Lo que cambia es que aquí no
/// hay «programa» que sondear en el PATH — el argv sale de `$SHELL` o de una
/// línea que el usuario escribió, y un `$SHELL` roto se dice con el error del
/// spawn, no con una sonda que adivinaría lo mismo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingShell {
    /// argv completo, con el binario en `[0]`. VACÍO es legítimo y significa
    /// «no lances nada»: es `app.toggle-panels`, que solo enseña la terminal
    /// anfitriona.
    pub argv: Vec<std::ffi::OsString>,
    /// Directorio de trabajo del hijo. `None` = el de norte (que es lo que
    /// hacen hoy los openers de #28).
    pub cwd: Option<std::path::PathBuf>,
    /// Esperar a una tecla ANTES de repintar los paneles. Es lo que hace
    /// legible la salida de un comando: sin esto, el listado vuelve encima de
    /// lo que acaba de escribirse.
    pub wait_for_key: bool,
}

/// The key of the capability cache: a scheme AND the authority it is served
/// by, which together are ONE backend. Owned because the map owns its keys and
/// the lookups are per help open, not per frame.
type CapsKey = (String, Option<String>);

/// The location `at` belongs to, as a cache key. Everything below the
/// authority is dropped on purpose: capabilities are a property of the
/// backend, not of the directory (a flag that varies per directory needs a
/// probe — see `App::caps`).
fn caps_key(at: &VPath) -> CapsKey {
    (
        at.scheme().to_owned(),
        at.authority().map(std::borrow::ToOwned::to_owned),
    )
}

/// How many connection degradations `App` retains at once.
///
/// #44 held ONE, as an `Option<String>`, so it was bounded by construction; a
/// collection keyed on a scheme the WIRE supplies is not, and the daemon is
/// not the only thing that can send those notifications. Thirty-two is far
/// more than the seven schemes that exist, so the cap can only ever be reached
/// by something abnormal — and when it is, the oldest report is dropped and the
/// reader keeps the ones that just arrived. Same discipline as the daemon's
/// retained listings and its approvals cap.
const DEGRADED_MAX: usize = 32;

/// Estado completo del TUI: dos panes y el foco.
pub struct App {
    /// Los dos paneles (izquierda, derecha).
    pub panes: [Pane; 2],
    /// Config de columnas resuelta (#108 bloque 4): set por scheme + sort.
    /// La siembra el arranque desde `[ui.columns]`; el render y los hooks
    /// de cd la consultan.
    pub columns: norte_frontend::columns::ColumnsSettings,
    /// `now` para las celdas de tiempo RELATIVO (#108 L5): `None` = reloj
    /// real; los tests de snapshot fijan `Some(ms)` para render estable.
    pub render_now_ms: Option<i64>,
    /// Catálogo de attrs por SCHEME (#117): una llamada a
    /// `fs.capabilities` por scheme nuevo y por sesión; alimenta hints y
    /// cabeceras del render y las filas del picker (tarea 4). Privado:
    /// lectura por [`Self::attr_catalog`], escritura por
    /// [`Self::insert_attr_catalog`].
    attr_catalogs: std::collections::HashMap<String, norte_proto::AttrCatalog>,
    /// Capability flags per LOCATION, the other half of the same response.
    ///
    /// `fs.capabilities` answers with `capabilities` AND `attrs` in one
    /// message, and this crate was keeping only the attrs — so the honest
    /// answer to "does this location refuse writes" was already in the
    /// process, thrown away, and asking for it again meant a second round trip
    /// over a link that had just carried it. Filled from the SAME call as
    /// [`Self::attr_catalogs`] (`main::first_page`), so caching it costs
    /// nothing.
    ///
    /// Keyed by scheme AND authority (`caps_key`), which the attr catalogue
    /// beside it is not, and the asymmetry is the point: a scheme is not one
    /// place. `sftp://a.org` and `sftp://b.org` are two servers that answer
    /// this question independently, as are two S3 endpoints and two FTP
    /// connections of the same plugin provider. Keyed by scheme alone, the
    /// first host to answer would veto — or fail to veto — every other host of
    /// its scheme for the rest of the session, and nothing would ever correct
    /// it. No built-in provider makes `READ_ONLY` differ per authority today
    /// (an archive and a plugin provider both decide it per scheme), so that
    /// bug would not fire yet; [`Self::caps`] is a general accessor to every
    /// flag, and the next one to be read this way must not be the one that
    /// finds out.
    ///
    /// It is still a per-LOCATION cache and not a per-PATH one: a flag that
    /// can differ between two directories of one connection needs a probe, and
    /// `pane.delete`'s `TRASH` check stays a probe for exactly that reason.
    ///
    /// Private: read through [`Self::caps`], written through
    /// [`Self::insert_caps`].
    caps: std::collections::HashMap<CapsKey, norte_proto::Capabilities>,
    /// Índice del pane con foco (invariante 0|1: privado, ver [`Self::focus`]).
    focus: usize,
    /// `true` cuando el usuario pidió salir.
    pub quit: bool,
    /// Secuencia de teclas pendiente, ya formateada (status bar). Se escribe
    /// SOLO por [`App::show_pending`]/[`App::clear_pending`], que la mantienen
    /// de acuerdo con [`App::which_key`].
    pub pending: String,
    /// The which-key panel, open exactly while a chord sequence is pending
    /// (K3a). `None` = closed.
    ///
    /// It takes NO keys of its own — the pane (or viewer) resolver keeps the
    /// keyboard while it is up, which is the whole point: the reader carries
    /// on typing the sequence and watches the panel narrow. It is the one
    /// overlay of this crate for which that is true, so `keyboard_owner`
    /// counts it only so a dispatch that opens or closes it is noticed, never
    /// to route a key to it.
    ///
    /// Written ONLY by [`App::show_pending`]/[`App::clear_pending`], next to
    /// [`App::pending`]: the panel and the status-bar segment describe the
    /// same resolver state, and two fields that can be updated separately are
    /// two fields that will eventually disagree — a panel left open over a
    /// keymap that was hot-reloaded under it would teach keys nobody has.
    pub which_key: Option<norte_frontend::whichkey::WhichKeyRows>,
    /// Diálogo modal activo (bloquea el keymap hasta resolverse).
    pub modal: Option<Modal>,
    /// Último mensaje para la barra (error por categoría o resultado).
    pub message: Option<String>,
    /// Panel de tasks vivo.
    pub board: crate::tasks::TaskBoard,
    /// Viewer abierto (F3); None = navegando.
    pub viewer: Option<crate::viewer::Viewer>,
    /// Help overlay open (F1, H3b): the navigable view over the `norte-help`
    /// corpus — sidebar, body, filter and history — plus the generated
    /// keyboard page, which is still built from the EFFECTIVE keymap (preset
    /// and the user's layers included, never a hand-kept list).
    pub help: Option<HelpView>,
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
    /// Overlay del picker de columnas (#108 7a): mismo patrón que
    /// `theme_picker` — un Option en App, NO una variante de Modal (Modal es
    /// confirmación; esto es lista con cursor). El modelo vive en
    /// norte-frontend (`ColumnsPicker`, regla 7).
    pub columns_picker: Option<norte_frontend::columns_picker::ColumnsPicker>,
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
    /// #44: remote sessions degraded to plaintext, BY SCHEME.
    ///
    /// Was a single pre-formatted `Option<String>`: `main` formatted the scheme
    /// and the host into a sentence and dropped the structured
    /// `ConnectionDegraded`, so "which connection degraded" had no answer, a
    /// second degradation silently overwrote the first, and the help had no
    /// fact to read. The value is kept whole and the banner
    /// ([`Self::connection_banner`]) is built from it on demand.
    ///
    /// One entry per scheme, so `sftp` and `ftp` coexist. Two HOSTS on one
    /// scheme still collapse into one entry, and the banner does not claim
    /// otherwise.
    ///
    /// A `VecDeque` in arrival order rather than a map, for two reasons that
    /// the map could not give: it is CAPPED at `DEGRADED_MAX` (a map keyed on
    /// a wire-supplied string grows as far as the sender wants), and the newest
    /// report is `back()`, which is the one the banner names when there are
    /// several. Lookup is a scan of at most 32 short strings, on the path that
    /// assembles the help's facts — not a per-frame one.
    ///
    /// NEVER CLEARED, deliberately. A degradation is not known to be resolved
    /// without a successful reconnect that reports the session encrypted, and
    /// the wire has no such notification: `connection.degraded` is only ever
    /// sent, never withdrawn. Any clearing rule this side could invent — a
    /// timeout, the next successful listing, leaving the pane — would say "the
    /// session is encrypted again" on evidence that does not support it, which
    /// is the one wrong answer for a security indicator. Follow-up work is a
    /// wire notification for the recovered case, not a heuristic here.
    ///
    /// Private: read through [`Self::degraded_for`] /
    /// [`Self::connection_banner`], written through [`Self::note_degraded`].
    degraded: std::collections::VecDeque<norte_proto::methods::ConnectionDegraded>,
    /// #177: esta sesión NO está registrando sus mutaciones en el journal.
    ///
    /// El brazo embebido abre el journal del directorio de estado en su primera
    /// mutación, y si lo tiene otro proceso (un daemon vivo, otra sesión que ya
    /// mutó) esta sigue adelante SIN registro: nada de lo que se copie, mueva o
    /// borre a partir de ahí se podrá deshacer ni auditar.
    ///
    /// Persistente, y por el mismo motivo que `degraded`: llega UNA vez, en
    /// mitad de una operación que el usuario acaba de lanzar con el teclado, y
    /// `app.message` lo borra la siguiente tecla — hay 136 sitios que escriben
    /// ese campo. Un aviso que dura hasta el siguiente `↓` no es un indicador
    /// de seguridad.
    ///
    /// No se limpia nunca: la decisión de esta sesión se toma una vez y no se
    /// revisa (ver `norte_core::embedded`), así que mientras la sesión viva la
    /// frase sigue siendo cierta. Si algún día se reintenta la apertura, esto
    /// necesita el evento de recuperación ANTES que el reintento.
    ///
    /// Se retiene el valor ESTRUCTURADO y no un `bool`, por el mismo criterio
    /// que `degraded` (H3d): la frase se compone al pintarla, y el motivo sigue
    /// disponible para quien lo necesite (una página de ayuda, un futuro
    /// detalle en la barra).
    ///
    /// Privado: se lee por [`Self::journal_banner`] y se escribe por
    /// [`Self::note_no_journal`].
    no_journal: Option<norte_core::embedded::NoJournal>,
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
    /// Panel de diferencias abierto (`Shift+F2`,
    /// 2026-08-11-directory-comparison.md): `None` = cerrado.
    ///
    /// Un overlay (`Option` en `App`) y NO un modo del pane, a diferencia de
    /// la búsqueda viva: una fila de comparación tiene DOS caras y un
    /// veredicto entre ellas, así que no cabe en la columna de un pane ni es
    /// una `Entry` que `extend_listing` pueda tragar. Ocupa el sitio de los
    /// dos panes mientras está abierto, que es lo que un diff es.
    pub compare: Option<CompareView>,
    /// Tamaños hidratados bajo demanda para el panel de diferencias, por
    /// `VPath` (#157).
    ///
    /// Solo la fila SELECCIONADA se sondea, nunca una ventana: a diferencia
    /// del pane normal (radio de filas, #52), `list_offset` ya mete la fila
    /// seleccionada dentro del área pintada en cuanto el panel de
    /// diferencias es lo que se está pintando, así que "en pantalla" es casi
    /// una tautología aquí — sondear solo esa fila cubre exactamente el caso
    /// que el issue señala: un huérfano `OnlyLeft`/`OnlyRight` sin tamaño es
    /// la fila que más lo pide, porque es la que decide si se copia.
    ///
    /// Vive en la TUI y no en `ComparePane` (`norte-frontend`) A PROPÓSITO:
    /// es una caché de PRESENTACIÓN, nunca viaja por el wire y ningún otro
    /// frontend la necesita, y `ComparePane` no tiene hoy ninguna vía de
    /// mutar una fila ya llegada — sus filas no cambian nunca tras `extend`
    /// (ver su rustdoc: "Rows only ever grow"). Guardarlo aquí y pintarlo
    /// como una superposición en `ui::draw_compare` evita necesitar esa vía.
    pub compare_size_hints: std::collections::HashMap<VPath, u64>,
    /// Paths YA sondeados para [`Self::compare_size_hints`], acierto o
    /// fallo, para no reintentar un stat que falló en cada frame — mismo
    /// criterio que `last_probed` para el pane normal. Se vacía cuando
    /// `launch_compare` abre una comparación nueva, nunca durante una: las
    /// filas de una comparación en curso no cambian bajo los pies (ver la
    /// nota de [`Self::compare_size_hints`]).
    pub compare_size_probed: std::collections::HashSet<VPath>,
    /// Qué comparación es la de esas dos ([`Self::begin_compare_generation`],
    /// #198). La sonda vive en el run loop y `launch_compare` no la recibe,
    /// así que un resultado en vuelo cuando empieza otra comparación llegaría
    /// a las tablas recién vaciadas de la SIGUIENTE. Lo que impide eso es que
    /// el resultado traiga la generación con la que se pidió.
    compare_generation: u64,
    /// Params de `fs.compare` que el despacho resolvió y el run loop aún no
    /// ha lanzado (`Shift+F2`). Mismo reparto que [`Self::pending_open`] y
    /// [`Self::pending_shell`]: `dispatch` decide QUÉ, el run loop —dueño del
    /// canal y de la Task— lo hace.
    pub pending_compare: Option<norte_proto::methods::FsCompareParams>,
    /// Panel de sincronización abierto (`Ctrl+Y`, o `s`/`m` dentro del panel
    /// de diferencias): `None` = cerrado. Se pinta POR ENCIMA del de
    /// diferencias, que sigue vivo detrás con sus marcas.
    pub sync: Option<SyncView>,
    /// Params de `sync.plan` resueltos y aún sin lanzar. Mismo reparto que
    /// [`Self::pending_compare`].
    pub pending_sync: Option<norte_proto::methods::SyncPlanParams>,
    /// `plan_hash` que el lector aprobó y el run loop aún no ha aplicado.
    ///
    /// Es lo ÚNICO que viaja: `sync.apply` no lleva rutas ni modo, así que no
    /// hay forma de que se ejecute algo distinto de lo que se enseñó (ADR
    /// 0049). Un `Box` porque es el mayor de los `pending_*` con diferencia y
    /// clippy mide el `App` entero.
    pub pending_sync_apply: Option<Box<norte_proto::methods::PlanHash>>,
    /// Reinterpretación de nombres (#57) del lado ORIGEN, congelada junto con
    /// [`Self::pending_sync`] y no cuando el run loop abre el panel: entre una
    /// cosa y la otra el lector puede haber pulsado `Alt+E`, y un plan que se
    /// pintase con otra reinterpretación de la que se pidió enseñaría
    /// `????.txt` donde el origen tenía un nombre CP1251.
    pub pending_sync_encoding: (
        Option<norte_encoding::NameEncoding>,
        Option<norte_encoding::NameEncoding>,
    ),
    /// Este backend registra sus mutaciones en un journal y por tanto puede
    /// sincronizar (`--daemon`).
    ///
    /// Lo fija el arranque, una vez, porque el `Backend` no cambia de brazo en
    /// vida del proceso. Es lo que alimenta
    /// [`norte_frontend::availability::Facts::journalled`]: sin él la hoja de
    /// referencia ofrecería `Ctrl+Y` y el core lo rechazaría en cerrado —una
    /// tecla muerta documentada, que es lo que #159 acaba de costar una vez.
    pub backend_journalled: bool,
    /// Openers declarativos fusionados (#28): clonados en arranque y en cada
    /// hot-reload OK. Fuente de `pane.open` (F4). Vacío = sin openers.
    pub openers: norte_frontend::openers::OpenersConfig,
    /// Comando externo resuelto por `pane.open` y pendiente de lanzar (#28).
    /// `dispatch` lo fija tras validar; el run loop —dueño de la terminal—
    /// lo ejecuta.
    pub pending_open: Option<PendingOpen>,
    /// Suspensión resuelta por el despacho y pendiente de ejecutar (#135):
    /// `app.terminal`, `app.toggle-panels` y el Enter de
    /// [`Modal::CommandLine`]. Mismo reparto que [`Self::pending_open`], y
    /// drenado en UN solo sitio del run loop (arriba del todo de la vuelta,
    /// antes del draw) para que ningún `continue` de los que responde teclas
    /// pueda dejarla encallada.
    pub pending_shell: Option<PendingShell>,
    /// Hints de pie de página de los overlays de diálogo (H1 T3, #24),
    /// PRECOMPUTADOS del efectivo `dialog` vigente — igual que `help_lines`
    /// en `main.rs`, se reconstruyen en el arranque y en cada hot-reload OK
    /// (`main::build_keymaps` + `DialogHints::build`), ANTES de que el
    /// efectivo se mueva al `Resolver` compartido. `ui::draw_*` los lee en
    /// vez de una clave Fluent estática.
    pub dialog_hints: crate::hints::DialogHints,
    /// Resolver of the help's live marks (H3b): rebuilt with the effective
    /// keymaps on every hot reload, exactly like `dialog_hints` and
    /// `help_lines` — a rebind must change the prose, and it does because the
    /// page is drawn through this.
    pub help_chords: std::sync::Arc<crate::help::TuiChords>,
    /// Command palette abierta (`Ctrl+P`/vim `:`, H1 T4): `None` = cerrada.
    pub palette: Option<Palette>,
    /// Filas de la palette PRECOMPUTADAS del keymap vigente
    /// ([`crate::palette::build_rows`]) — igual criterio que `help_lines`/
    /// `dialog_hints`: se reconstruyen en el arranque y en cada hot-reload
    /// OK, ANTES de que los efectivos se muevan al `Resolver`. Abrir la
    /// palette (`dispatch`, brazo `app.palette`) solo clona esta snapshot.
    pub palette_rows: Vec<crate::palette::Row>,
    /// Estado del ratón (captura aparte, que es de la terminal): la
    /// geometría PINTADA del último frame, el gesto armado y el último
    /// click. La geometría la devuelve el run loop tras cada `draw`
    /// (#124): sin ella no se resuelve ningún click.
    pub mouse: crate::mouse::MouseState,
    /// Overlay de ajustes abierto (`app.settings`, S3): `None` = cerrado.
    /// Sus filas se reconstruyen del `cfg` VIGENTE en cada hot-reload OK
    /// (`main::reload_config`, `Settings::refresh`) — a diferencia de
    /// `palette`/`help`, que se CIERRAN, este overlay se queda abierto y se
    /// refresca en su sitio (ver el doc de `Settings::refresh`).
    pub settings: Option<Settings>,
    /// Shortcut editor open (K3c, `Ctrl+K` from the settings overlay):
    /// `None` = closed.
    ///
    /// Sits IN FRONT of `settings`, which stays open behind it — the editor is
    /// a screen of Settings, not a replacement for it, and closing it returns
    /// the reader where they were.
    ///
    /// Like `settings` it is REFRESHED and not closed on a hot reload
    /// (`main::reload_config`), because that reload is usually its own write
    /// coming back through the watcher: an editor that closed on the write it
    /// just made would be unusable for a second rebind. Its capture, unlike a
    /// settings edit buffer, does NOT survive the refresh — see
    /// `ShortcutsState::refresh`, whose verdict belongs to the map that was
    /// just replaced.
    pub shortcuts: Option<Shortcuts>,
    /// How many times the two panes have been exchanged (`pane.swap`).
    ///
    /// It exists because nothing else in the model records that a swap
    /// happened: everything indexed by pane TRAVELS with the pane, so a swap
    /// merely exchanges two values and anything comparing them per side sees
    /// nothing move. Read through [`Self::swap_seq`] by the mouse, whose
    /// armed gesture carries pane indices that the swap has just
    /// re-attributed to the other side's content.
    ///
    /// Only ever compared for EQUALITY, so it wraps rather than saturating —
    /// saturating would eventually stop changing, which is the one thing it
    /// must never do.
    swap_seq: u64,
    /// `--pick` (S2): true for the lifetime of the process once the flag was
    /// passed. Read by the run loop's Enter/Ctrl+Enter override (design §B)
    /// and by `Command::AppPickAccept`'s dispatch arm, which is a no-op
    /// without it — the command exists in the catalogue unconditionally
    /// (help, palette, rebind checks), but only ever FIRES under `--pick`.
    /// Set once in `main`, right after construction; never toggled at
    /// runtime.
    pub pick: bool,
    /// The picker's answer, written by `Command::AppPickAccept` and read by
    /// `main` after the run loop returns (`app.quit` is set alongside it, so
    /// this is always read exactly once). `None` after a normal quit means
    /// the pick was CANCELLED, not that nothing happened — `main` tells the
    /// two apart with `Self::pick`, per the exit-code table in the design
    /// (0 accepted, 1 cancelled, 2 error).
    pub picked: Option<Vec<VPath>>,
}

/// Qué popup de navegación está abierto (spec 2026-07-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavPopupKind {
    /// Historial de directorios del pane con foco (sesión, no persistido).
    History,
    /// Favoritos persistidos en el `norte.toml` del USUARIO.
    Hotlist,
    /// Volúmenes del host (`pane.select-drive`/`-left`/`-right`, design
    /// 2026-08-10-volumes-design.md §D): snapshot congelada al abrir vía
    /// `Backend::volumes` — `main.rs` hace el fetch async (app.rs no conoce
    /// `Backend`) y entrega los items ya construidos a
    /// [`App::open_volumes_popup`].
    Volumes,
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
    /// Historial, hotlist o volúmenes (decide título, footer y qué teclas
    /// extra acepta).
    pub kind: NavPopupKind,
    /// Items congelados al abrir.
    items: Vec<NavItem>,
    /// Índice resaltado.
    cursor: usize,
    /// Input de nombre abierto (`a` en hotlist): captura imprimibles antes
    /// que nada (main.rs); `None` = navegación normal del popup.
    pub name_input: Option<String>,
    /// El pane que `Confirm` navega. El foco para historial, hotlist y
    /// `pane.select-drive`; un LADO fijo para `-left`/`-right`
    /// independientemente de dónde esté el foco (design §D — así se
    /// comportan `Alt+F1`/`Alt+F2` de Total Commander). Congelado al abrir,
    /// misma razón que el resto del item: nada aquí se re-resuelve contra un
    /// foco que pudo moverse debajo del popup.
    target_pane: usize,
    /// Solo volúmenes: si la lista ACTUAL incluye pseudo-filesystems (el
    /// toggle "mostrar todo" del design §E). Sin sentido en historial/
    /// hotlist, donde queda `false`.
    include_pseudo: bool,
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

    /// El pane que `Confirm` debe navegar — ver el campo.
    #[must_use]
    pub fn target_pane(&self) -> usize {
        self.target_pane
    }

    /// Si la lista de volúmenes actual incluye pseudo-filesystems — ver el
    /// campo. Sin significado fuera de `NavPopupKind::Volumes`.
    #[must_use]
    pub fn include_pseudo(&self) -> bool {
        self.include_pseudo
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

/// Rows for the volumes popup (design §D): `main.rs` calls this right after
/// `Backend::volumes` answers and hands the result to
/// [`App::open_volumes_popup`] — this function owns none of the I/O, only the
/// presentation, same split as the rest of the popup family.
#[must_use]
pub fn volume_items(
    volumes: &[norte_proto::methods::Volume],
    enc: Option<norte_encoding::NameEncoding>,
) -> Vec<NavItem> {
    volumes
        .iter()
        .map(|v| NavItem {
            display: volume_item_display(v, enc),
            target: Some(v.mount.clone()),
            hotlist_name: None,
        })
        .collect()
}

/// One volume row: `[label — ]mount  fs_type  free / total`. Every text
/// field the platform hands us — label, mount AND `fs_type` — goes through
/// the same masking [`nav_item_display`] uses (`display_name`/
/// `path_display_with`, both backed by `norte_encoding::is_terminal_hazard`)
/// before it reaches the screen. `fs_type` is not the closed, ASCII-only
/// vocabulary it looks like: a FUSE mount's `fuse.<subtype>` component is the
/// `-o subtype=` value an UNPRIVILEGED user picks (`sshfs`, `rclone mount`,
/// `encfs`…), so it is exactly as untrusted as a filename — encoding-auditor
/// review caught it reaching the row unmasked in an earlier draft of this
/// function, the same class of bug `control_escape` in the canonical corpus
/// exists to catch. `free`/`total` print `volumes-size-unknown` instead of a
/// number when the filesystem did not answer in time — design §A is explicit
/// that a bare `0` here would read as "full", the opposite of what an absent
/// size means.
///
/// `label` is `Option<Vec<u8>>` (V3.5, a second encoding-auditor finding on
/// the same review pass that caught `fs_type` above): it reaches
/// [`display_name`] as the raw bytes the wire carried, with NO `String`
/// upstream to have already thrown away or lossily rewritten a non-UTF-8
/// label before the masking ever saw it — otherwise the badge below would
/// be protecting evidence that was already gone.
fn volume_item_display(
    v: &norte_proto::methods::Volume,
    enc: Option<norte_encoding::NameEncoding>,
) -> String {
    // #98/F4 (same reasoning `nav_item_display` carries): a popup is a
    // decision surface, so it follows the focused pane's reinterpretation.
    let (path_text, path_hostil) = norte_frontend::path_display_with(&v.mount, enc);
    let (label_prefix, label_hostil) = match v.label.as_deref() {
        Some(l) => {
            let (nt, nh) = display_name(l);
            (format!("{nt} — "), nh)
        }
        None => (String::new(), false),
    };
    let (fs_type_text, fs_type_hostil) = display_name(v.fs_type.as_bytes());
    let free = v
        .free_bytes
        .map_or_else(|| t("volumes-size-unknown"), norte_frontend::human_bytes);
    let total = v
        .total_bytes
        .map_or_else(|| t("volumes-size-unknown"), norte_frontend::human_bytes);
    let body = format!("{label_prefix}{path_text}  {fs_type_text}  {free} / {total}");
    if path_hostil || label_hostil || fs_type_hostil {
        format!("{} {body}", crate::ui::HOSTILE_BADGE)
    } else {
        body
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

/// Tope defensivo sobre las etiquetas cortas de un plugin en el wire (H3e):
/// `PluginInfo.name`, `.publisher` y `PluginCommandInfo.title`.
///
/// El manifiesto acota SOLO el tercero — 120,
/// `norte_plugin_host::manifest::COMMAND_TITLE_MAX_CHARS` — y no acota `name`
/// ni `publisher`, así que para esos dos no hay límite de origen que reflejar y
/// el cliente pone el suyo. Se elige EL MISMO valor a propósito: son el mismo
/// tipo de texto (una etiqueta corta de tercero que va a una fila) y la ayuda
/// los pinta uno al lado del otro. Para `title` el tope es además un espejo del
/// del manifiesto, con el mismo criterio que
/// [`PLUGIN_DESCRIPTION_WIRE_CAP`]: el límite de parseo solo protege el camino
/// honesto, y un daemon hostil o comprometido puede mandar cualquier longitud.
///
/// Sin él, un `name` kilométrico no desborda el pintado (la lateral recorta),
/// pero sí el FILTRO del modelo, que pliega el título entero en cada tecla.
pub const PLUGIN_NAME_WIRE_CAP: usize = 120;

/// Acota ([`PLUGIN_NAME_WIRE_CAP`]) y enmascara ([`display_name`]) una etiqueta
/// corta de tercero — el `name`, el `publisher` o el título de un comando de un
/// plugin — para que pueda entrar en el modelo de la ayuda (H3e).
///
/// En el PUNTO DE ENTRADA, no al pintar: `norte_frontend::help::PluginNode`
/// documenta su `title` como «ya enmascarado y acotado», el modelo no enmascara
/// nada — filtra sobre el título crudo que le den — y
/// [`crate::help::TuiChords`] entrega sus etiquetas directas al pintor.
///
/// Un recorte se MARCA con `…`, como lo marcan los vecinos que hacen esto mismo
/// (`masked_and_capped` en el doctor, [`norte_frontend::middle_ellipsis`] en la línea
/// de descripción del gestor). Cortar en seco presenta un nombre truncado como
/// si estuviera completo, que es la misma clase de mentira que H3d fue a
/// perseguir a los pies de overlay: quien lee no puede saber que falta algo, y
/// un nombre acabado en mitad de una palabra es precisamente lo que un tercero
/// usaría para que su etiqueta pase por otra.
///
/// Elipsis por la DERECHA y no media: estas etiquetas se distinguen por su
/// principio (`middle_ellipsis` existe para las rutas, donde lo que identifica
/// está al final).
/// Se acota ANTES de enmascarar, y eso es seguro porque sobre texto ya UTF-8
/// [`display_name`] es 1:1 en chars (mapea char a char, nunca inserta ni
/// borra). Al revés habría que enmascarar los 50 000 chars que un daemon
/// hostil quiera mandar para quedarse con 120.
#[must_use]
pub fn plugin_label(raw: &str) -> String {
    let mut chars = raw.chars();
    let head: String = chars.by_ref().take(PLUGIN_NAME_WIRE_CAP).collect();
    let overflowed = chars.next().is_some();
    let mut out = display_name(head.as_bytes()).0;
    if overflowed {
        out.push('…');
    }
    out
}

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
            has_help: false,
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
            has_help: false,
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

// K3c: the shortcut editor's state machine is shared with the GUI for the same
// reason as the settings one — it is pure (rows, a filter, a cursor and a
// capture with its verdict), and two frontends deciding separately what a
// collision is would be two answers to one question.
pub use norte_frontend::shortcuts::ShortcutsState as Shortcuts;

impl App {
    /// App con foco en el pane izquierdo.
    #[must_use]
    pub fn new(left: Pane, right: Pane) -> Self {
        Self {
            panes: [left, right],
            render_now_ms: None,
            attr_catalogs: std::collections::HashMap::new(),
            caps: std::collections::HashMap::new(),
            columns: norte_frontend::columns::ColumnsSettings::default(),
            focus: 0,
            quit: false,
            pending: String::new(),
            which_key: None,
            modal: None,
            message: None,
            board: crate::tasks::TaskBoard::default(),
            viewer: None,
            help: None,
            pending_collisions: std::collections::VecDeque::new(),
            pending_approvals: std::collections::VecDeque::new(),
            theme: crate::theme::TuiTheme::default(),
            theme_picker: None,
            columns_picker: None,
            extensions: None,
            lua_pending_trust: None,
            lua_status: None,
            degraded: std::collections::VecDeque::new(),
            no_journal: None,
            history: [
                crate::nav::History::default(),
                crate::nav::History::default(),
            ],
            hotlist: Vec::new(),
            nav_popup: None,
            search_dialog: None,
            compare: None,
            compare_size_hints: std::collections::HashMap::new(),
            compare_size_probed: std::collections::HashSet::new(),
            compare_generation: 0,
            pending_compare: None,
            sync: None,
            pending_sync: None,
            pending_sync_apply: None,
            pending_sync_encoding: (None, None),
            // Fail-CLOSED: el `App` de un test no tiene backend, y ofrecer
            // sincronizar por defecto convertiría cada test en un permiso.
            // `main` lo enciende cuando el backend es remoto.
            backend_journalled: false,
            openers: norte_frontend::openers::OpenersConfig::empty(),
            pending_open: None,
            pending_shell: None,
            dialog_hints: crate::hints::DialogHints::default(),
            help_chords: default_help_chords(),
            palette: None,
            palette_rows: Vec::new(),
            mouse: crate::mouse::MouseState::default(),
            settings: None,
            shortcuts: None,
            swap_seq: 0,
            pick: false,
            picked: None,
        }
    }

    /// Publishes a resolver's in-flight state to the screen: the status-bar
    /// segment ([`Self::pending`]) and the which-key panel
    /// ([`Self::which_key`]), which must never disagree about it.
    ///
    /// The ONE place that decides whether the panel is open, and it decides it
    /// from the resolver rather than from the [`Resolution`] variant that got
    /// us here:
    ///
    /// - a pending SEQUENCE opens it — immediately, with no delay of any kind.
    ///   ADR 0006's resolution is timing-free and a panel that waited 400 ms
    ///   would put timing back into what the reader sees;
    /// - a bare COUNT does not. Its pending sequence is empty, so there are no
    ///   rows: the continuation of a count is any key at all, and the panel
    ///   would be the whole keymap. K2a already paints the count in the bar,
    ///   and a count typed BEHIND a prefix still shows — in the panel's title.
    ///
    /// [`Resolution`]: norte_frontend::keymap::Resolution
    pub fn show_pending(&mut self, resolver: &norte_frontend::keymap::Resolver, lang: Lang) {
        self.pending = crate::keymap::pending_display(resolver);
        self.which_key = (!resolver.pending().is_empty()).then(|| {
            norte_frontend::whichkey::WhichKeyRows::build(
                resolver.effective(),
                resolver.pending(),
                resolver.count(),
                lang,
            )
        });
    }

    /// Nothing is pending any more: the bar segment and the panel go together.
    ///
    /// Called by every arm that ENDS a pending state — a command ran, the key
    /// was unavailable, `Esc` reset it, a key the frontend does not model
    /// arrived — and by the hot reload, which replaces the resolvers whole
    /// (ADR 0007): a panel built from the old effective map would survive its
    /// keymap and list keys the new one does not have.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.which_key = None;
    }

    /// The reader is no longer typing a sequence, and it was not a key of that
    /// sequence that ended it: a gesture (a double click), or a key SWALLOWED
    /// before the resolver ever saw it (the `Esc` that cancels a Lua command,
    /// an AI rename or a semantic search in flight).
    ///
    /// Resets the resolver too, which [`Self::clear_pending`] alone does not:
    /// the pending chords live in the resolver, and clearing only the screen
    /// would leave `g` armed inside it, so the NEXT key would complete a
    /// sequence the reader had already abandoned. It is the same treatment the
    /// run loop gives a key the frontend does not model.
    ///
    /// The visible symptom this exists for: with a Lua command running and `g`
    /// pending, `Esc` is consumed by the cancellation and the panel used to
    /// stay on screen — the one key that means "never mind" appearing to do
    /// nothing at all.
    pub fn abandon_pending(&mut self, resolver: &mut norte_frontend::keymap::Resolver) {
        resolver.reset();
        self.clear_pending();
    }

    /// Lays the open help overlay out for a body of `width`×`height` cells,
    /// through the resolver and theme in force. No-op with the overlay closed.
    ///
    /// The split-borrow wrapper exists because [`HelpView::refresh`] needs
    /// three fields of `App` at once ([`Self::help`] mutably,
    /// [`Self::help_chords`] and [`Self::theme`] shared) and a caller outside
    /// this module cannot name them disjointly. The run loop calls it with
    /// [`crate::ui::help_body_size`] of the frame it is about to paint, so
    /// the geometry the model clamps against is the geometry on screen.
    pub fn refresh_help(&mut self, width: usize, height: usize) {
        if let Some(help) = &mut self.help {
            help.refresh(&self.help_chords, width, height, &self.theme);
        }
    }

    /// Abre el diálogo de búsqueda viva (`Alt+F7`, liveSearch T6) vacío. La
    /// raíz del walk se resuelve al lanzar (cwd del pane con foco).
    pub fn open_search_dialog(&mut self) {
        self.search_dialog = Some(SearchDialog::new());
    }

    /// `Shift+F2`: resuelve QUÉ comparar y lo deja pendiente para el run loop.
    ///
    /// El izquierdo es el pane con FOCO (spec: «el panel que lanzó la
    /// comparación es el izquierdo»), el derecho el otro — no `panes[0]` y
    /// `panes[1]`, porque el lector que pulsa la tecla desde el pane derecho
    /// espera que su directorio sea el suyo.
    ///
    /// Dos negativas se dan AQUÍ, sin ir y volver al daemon:
    ///
    /// * **Los dos panes en el mismo sitio.** El daemon responde `-32602` a
    ///   eso (C6) y tiene razón, pero la frase que el lector necesita no
    ///   depende de una vuelta por la red.
    /// * **Un pane virtual.** Una lista de hits no es un directorio, así que
    ///   no hay raíz que mandar — la misma negativa que ya dan mirror y pull.
    pub fn request_compare(&mut self) {
        // El visor sustituye a los panes en la pantalla y `ui::draw` le da
        // precedencia sobre este panel, así que abrirlo por detrás dejaría
        // los píxeles diciendo una cosa y el teclado yendo a otra — el bug
        // exacto contra el que está escrito el rustdoc de `modal_wins`.
        if self.viewer.is_some() {
            return;
        }
        if self.panes[0].virtual_search || self.panes[1].virtual_search {
            self.message = Some(t("msg-pane-not-a-location"));
            return;
        }
        let left = self.focused().dir().clone();
        let right = self.panes[self.focus() ^ 1].dir().clone();
        if left == right {
            self.message = Some(t("compare-same-path"));
            return;
        }
        self.pending_compare = Some(norte_proto::methods::FsCompareParams {
            left,
            right,
            criteria: norte_proto::methods::CompareCriteria::default(),
            max_depth: None,
            // Vive con el modelo (#158), no aquí: la GUI pide la MISMA
            // comparación, y dos copias que se separaran darían veredictos
            // distintos para los mismos dos directorios.
            mtime_tolerance_ms: norte_frontend::compare::MTIME_TOLERANCE_MS,
            // Sin toggle en la UI, y a propósito: `Backend::compare` responde
            // `Unsupported` a `true` antes de que exista Task alguna, porque
            // el engine acepta el campo y lo ignora. Ofrecer la casilla sería
            // ofrecer una promesa que nadie cumple.
            follow_symlinks: false,
            // Tampoco hay toggle: el pane de diferencias enseña un huérfano
            // como UNA fila, y descenderlo es lo que un plan de
            // sincronización pide por su cuenta (spec 2).
            descend_orphans: None,
        });
    }

    /// Las dos raíces de una sincronización, en el orden `(origen, destino)`.
    ///
    /// Con el panel de diferencias abierto las decide su lado ACTIVO, que es
    /// lo que `Tab` cambia: nada se infiere del foco ni del orden de los
    /// panes, porque el sentido de una sincronización es la mitad de lo que
    /// hay que aprobar. Sin panel abierto son el pane con foco y el otro, el
    /// mismo reparto que [`Self::request_compare`].
    ///
    /// La decisión ENTERA es [`norte_frontend::sync::sync_roots`] (#161), no
    /// una copia local de sus dos brazos: la GUI necesita exactamente la misma
    /// —incluido el brazo del lado activo, que es el que llega con su panel—
    /// y aquí solo se le da lo que esta TUI sabe.
    #[must_use]
    fn sync_roots(&self) -> SyncRoots {
        let otro = &self.panes[self.focus() ^ 1];
        norte_frontend::sync::sync_roots(
            self.sync_source_view(),
            &norte_frontend::sync::Panes {
                focused_root: self.focused().dir(),
                focused_encoding: self.focused().name_encoding(),
                other_root: otro.dir(),
                other_encoding: otro.name_encoding(),
            },
        )
    }

    /// El panel de diferencias del que sale la selección, si lo hay.
    fn sync_source_view(&self) -> Option<&CompareView> {
        self.compare.as_ref()
    }

    /// `sync.plan`: resuelve QUÉ sincronizar y lo deja pendiente para el run
    /// loop, o dice por qué no.
    ///
    /// Devuelve los params que dejó pendientes, para que un test lea la
    /// decisión sin run loop.
    ///
    /// Las negativas que se dan AQUÍ, sin ir y volver al daemon:
    ///
    /// * **Sin journal.** Lo dice [`norte_core::backend::Backend::is_journalled`],
    ///   que es `false` en embebido: desde #167 ese engine sí lleva el journal
    ///   del directorio de estado, pero no instala spool, y sin spool
    ///   `sync.plan` se niega en cerrado (regla dura 4). Planificar contra él
    ///   sería enseñar un plan que nadie puede aprobar. La misma verdad que
    ///   [`norte_frontend::availability::Facts::journalled`] ya atenúa en la
    ///   hoja de referencia; esto es lo que pasa si el lector llega igual.
    /// * **Un pane virtual**, y **las dos raíces en el mismo sitio**: idénticas
    ///   a las de comparar, por las mismas razones.
    /// * **Más marcas que [`SYNC_MAX_INCLUDE`]**. `Backend::sync_plan` lo
    ///   rechaza con `InvalidPath`, que no dice cuántas sobran.
    ///
    /// [`SYNC_MAX_INCLUDE`]: norte_proto::methods::SYNC_MAX_INCLUDE
    pub fn request_sync(
        &mut self,
        mode: norte_proto::methods::SyncMode,
    ) -> Option<&norte_proto::methods::SyncPlanParams> {
        if self.viewer.is_some() {
            return None;
        }
        if !self.backend_journalled {
            self.message = Some(t("msg-sync-needs-daemon"));
            return None;
        }
        if self.compare.is_none() && (self.panes[0].virtual_search || self.panes[1].virtual_search)
        {
            self.message = Some(t("msg-pane-not-a-location"));
            return None;
        }
        let SyncRoots {
            source,
            dest,
            source_encoding,
            dest_encoding,
        } = self.sync_roots();
        if source == dest {
            self.message = Some(t("compare-same-path"));
            return None;
        }
        let include = match self.sync_include(&source, &dest) {
            Ok(include) => include,
            Err(e) => {
                self.message = Some(sync_include_message(&e));
                return None;
            }
        };
        self.pending_sync = Some(norte_proto::methods::SyncPlanParams {
            source,
            dest,
            mode,
            // Los criterios son los de comparar y el default del wire ya los
            // trae. `follow_symlinks` y `descend_orphans` se quedan en su
            // default a propósito: `Backend::sync_plan` responde `Unsupported`
            // a los dos, porque el segundo no es del llamante —lo fija el
            // planificador al lado del origen— y el primero no lo cumple nadie.
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::default(),
            include,
        });
        // La reinterpretación del ORIGEN se congela aquí, con las raíces, y
        // viaja al panel: un lector que había pulsado `Alt+E` para leer un
        // share CP1251 no puede recuperar `????.txt` al sincronizarlo (#57,
        // el mismo fallo que el panel de diferencias arregló en su review).
        self.pending_sync_encoding = (source_encoding, dest_encoding);
        self.pending_sync.as_ref()
    }

    /// La lista `include` que sale de las marcas del panel de diferencias, o
    /// el motivo por el que no hay una.
    ///
    /// QUÉ cuenta como negativa —y contra qué raíz se mide cada marca— lo
    /// decide [`norte_frontend::sync::include_from_rows`], que vive junto a
    /// `anchor_of` porque contesta la misma pregunta del otro lado del viaje.
    ///
    /// # Errors
    /// Lo que devuelva aquella; [`Self::request_sync`] lo traduce a una frase.
    fn sync_include(
        &self,
        source: &VPath,
        dest: &VPath,
    ) -> Result<Option<Vec<norte_proto::methods::RelPath>>, norte_frontend::sync::IncludeError>
    {
        let marcadas = self
            .compare
            .as_ref()
            .map(|v| v.pane.marked_rows())
            .unwrap_or_default();
        norte_frontend::sync::include_from_rows(source, dest, &marcadas)
    }

    /// Cierra el panel de sincronización. La cancelación de la Task es del run
    /// loop (es suya); esto solo suelta el estado de presentación.
    pub fn close_sync(&mut self) {
        self.sync = None;
        self.pending_sync_apply = None;
    }

    /// El pane al que pertenece el lado ACTIVO del panel de diferencias.
    ///
    /// `None` con el panel cerrado. Es lo que hace que el `Enter` de una fila
    /// lleve al lector a donde esa fila vive DE VERDAD sin costarle el otro
    /// directorio.
    #[must_use]
    pub fn compare_active_pane(&self) -> Option<usize> {
        let view = self.compare.as_ref()?;
        Some(match view.pane.active_side() {
            norte_proto::methods::Side::Right => view.left_pane ^ 1,
            _ => view.left_pane,
        })
    }

    /// Cierra el panel de diferencias. La cancelación de la Task es del run
    /// loop (es suya); esto solo suelta el estado de presentación.
    pub fn close_compare(&mut self) {
        self.compare = None;
    }

    /// Índice del pane con foco (0 = izquierda, 1 = derecha).
    #[must_use]
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// El catálogo cacheado del scheme dado, si llegó (#117): `None` = el
    /// `fs.capabilities` aún no corrió o falló — se pinta con defaults
    /// Opaque y la cabecera cae al id, jamás se bloquea el render.
    #[must_use]
    pub fn attr_catalog(&self, scheme: &str) -> Option<&norte_proto::AttrCatalog> {
        self.attr_catalogs.get(scheme)
    }

    /// Cachea el catálogo de `scheme` (#117): lo llaman el arranque y el
    /// flujo de cd tras su `fs.capabilities` — una vez por scheme y sesión
    /// (un fallo no cachea nada: el próximo cd al scheme reintenta).
    pub fn insert_attr_catalog(&mut self, scheme: String, catalog: norte_proto::AttrCatalog) {
        self.attr_catalogs.insert(scheme, catalog);
    }

    /// The cached capability flags of the location `at` belongs to, if the
    /// response has landed.
    ///
    /// Takes the PATH and not a scheme so that the caller cannot accidentally
    /// ask a coarser question than the cache answers: the key is scheme plus
    /// authority (see the `caps` field), and a `&str` parameter would have made
    /// "`sftp`" a legal thing to ask about.
    ///
    /// `None` means "not asked yet, or the call failed" and never "no
    /// capabilities": a caller must degrade rather than read absence as a
    /// denial (see [`Self::pane_read_only`] for the shape of that).
    #[must_use]
    pub fn caps(&self, at: &VPath) -> Option<&norte_proto::Capabilities> {
        self.caps.get(&caps_key(at))
    }

    /// Caches the capability flags of the location `at` belongs to.
    ///
    /// Called from the same place as [`Self::insert_attr_catalog`] and with
    /// the halves of ONE `fs.capabilities` response — see [`Self::caps`]'
    /// field docs for why keeping only the attrs was waste.
    pub fn insert_caps(&mut self, at: &VPath, caps: norte_proto::Capabilities) {
        self.caps.insert(caps_key(at), caps);
    }

    /// Whether the pane's location refuses mutation.
    ///
    /// Answered from the capability flags when they have arrived, and
    /// SYNTACTICALLY from the scheme until they do
    /// ([`norte_frontend::availability::scheme_is_read_only`]) — an archive
    /// scheme is read-only by construction, so the guess is right for the case
    /// that matters.
    ///
    /// Where the guess is wrong it errs toward WRITABLE, never toward
    /// read-only: a read-only SFTP export answers `false` here until its flags
    /// land, so the help offers a copy into it and the submitted task fails
    /// with an error the reader sees. That is the direction to be wrong in.
    /// Once the flags are in they decide, `READ_ONLY` included — its own
    /// contract is that the UI vetoes upfront.
    ///
    /// An index outside `0|1` is `false`, i.e. "writable, as far as this
    /// knows": the callers are the `Facts` assembly and the help, and a
    /// verdict is not the place to panic over a bad index.
    #[must_use]
    pub fn pane_read_only(&self, pane: usize) -> bool {
        let Some(p) = self.panes.get(pane) else {
            return false;
        };
        match self.caps(p.dir()) {
            Some(c) => c.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
            None => norte_frontend::availability::scheme_is_read_only(p.dir().scheme()),
        }
    }

    /// The context the help's verdict table is asked against.
    ///
    /// Every predicate here is the one the matching `dispatch` arm uses, and
    /// that is the whole contract of this function: a fact derived a second way
    /// dims a row the app would have run, which is worse than not dimming at
    /// all — the reader stops trying.
    ///
    /// - `enterable`: `nav.enter`'s — `Dir | Symlink`, or an archive the TUI
    ///   knows how to compose a scheme for ([`crate::nav::archive_root_for`],
    ///   so a `.zip` FILE counts). This is where the TUI and the GUI genuinely
    ///   disagree, which is why the table takes the boolean rather than a kind.
    /// - `viewable`: `pane.view`'s — `File | Symlink` (a symlink to a
    ///   directory fails in the viewer with a visible message, which is the
    ///   dispatch arm's own decision).
    /// - `rename_single`: `true`, ALWAYS, and that is the honest answer rather
    ///   than a shortcut. `Command::PaneRename` (`App::open_rename`) targets
    ///   `selected()` and never looks at the marks, so shift+F6 renames exactly
    ///   one entry no matter how many are marked. Filling this from the marked
    ///   set — the obvious reading of the field's old name — dimmed the row for
    ///   a batch the TUI renames one entry of quite happily. The GUI fills the
    ///   same fact from its count, because its rename does refuse a multiple
    ///   selection.
    /// - the two read-only flags: [`Self::pane_read_only`] for the focused pane
    ///   and the other one. "Source" is the focused pane because every command
    ///   in the table acts FROM the focus.
    /// - `degraded`: [`Self::degraded_for`] on the focused pane's scheme. It
    ///   vetoes nothing today — see the field's rustdoc in
    ///   [`norte_frontend::availability::Facts`].
    ///
    /// NOT computed: policy denial. See [`crate::help::TuiChords`]'
    /// `availability` for why faking it would dim a row for a rule that does
    /// not apply to the human sitting here.
    #[must_use]
    pub fn help_facts(&self) -> norte_frontend::availability::Facts {
        let pane = self.focused();
        let sel = pane.selected();
        norte_frontend::availability::Facts {
            enterable: sel.is_some_and(|e| {
                matches!(e.kind, EntryKind::Dir | EntryKind::Symlink)
                    || crate::nav::archive_root_for(e).is_some()
            }),
            viewable: sel.is_some_and(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink)),
            rename_single: true,
            source_read_only: self.pane_read_only(self.focus),
            dest_read_only: self.pane_read_only(self.focus ^ 1),
            degraded: self.degraded_for(pane.dir().scheme()).is_some(),
            journalled: self.backend_journalled,
        }
    }

    /// Freezes [`Self::help_facts`] into the resolver the help overlay renders
    /// through.
    ///
    /// Called when the overlay OPENS, before the first layout, so every row of
    /// every page the reader walks is judged against one context — see
    /// [`crate::help::TuiChords`]' `facts` for why a live read would make a
    /// page disagree with itself.
    ///
    /// And called AGAIN whenever the listing underneath is replaced with the
    /// overlay still open (`main::after_panes_refresh`): the freeze is against
    /// the reader moving, not against the world moving, and `enterable` /
    /// `viewable` describe an entry a finished task can delete.
    pub fn freeze_help_facts(&mut self) {
        let facts = self.help_facts();
        self.help_chords = std::sync::Arc::new(self.help_chords.with_facts(facts));
    }

    /// Freezes the plugin catalogue into the overlay AND into the resolver it
    /// renders through (H3e).
    ///
    /// Both halves of one snapshot, so they cannot disagree: the sidebar offers
    /// a page for every plugin with a `help.md`
    /// ([`HelpView::set_plugins`]), and the resolver dims the command rows of
    /// the ones that are not approved-and-enabled
    /// ([`crate::help::TuiChords::with_plugins`]).
    ///
    /// A PHOTOGRAPH, taken when the help opens and thrown away with it. That is
    /// the same discipline [`Self::freeze_help_facts`] follows and it buys the
    /// same thing — no verdict changes under the reader's cursor — plus two
    /// practical ones: there is no plugin cache in [`App`] to invalidate, and no
    /// round trip to the daemon while a frame is being painted.
    ///
    /// The resolver is updated even with no overlay open. It is the same
    /// snapshot either way, and `with_plugins` carries the frozen facts across
    /// exactly as `with_facts` carries the plugins across — so the re-freeze the
    /// refresh funnel performs mid-overlay cannot drop either half.
    pub fn freeze_help_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        // The id gate of [`HelpView::set_plugins`], applied to the resolver's
        // half of the snapshot as well, so no structure the help owns can hold
        // an id the host had no business announcing. `set_plugins` re-applies it
        // rather than trusting this: it is public and exercised directly by
        // tests, so it stays safe by construction like
        // `norte_frontend::palette::plugin_rows`.
        let plugins: Vec<&norte_proto::methods::PluginInfo> = plugins
            .iter()
            .filter(|p| norte_core::is_valid_plugin_id(&p.id))
            .collect();
        let active: std::collections::BTreeSet<String> = plugins
            .iter()
            .filter(|p| p.approved && p.enabled)
            .map(|p| p.id.clone())
            .collect();
        // The manifest's name for every contributed command, keyed by its
        // DISPATCH key — built with the same `format!` the palette uses
        // (`norte_frontend::palette::plugin_rows`) so these keys and the ones a
        // page carries in `topic.commands` cannot be spelled differently. The
        // command id has no validated charset and may contain `:`, which is why
        // nothing here ever splits one; it is only ever appended.
        //
        // NOT filtered by `approved && enabled`, unlike `active` above: an
        // inactive plugin's page still lists its rows — dimmed, which is the
        // answer the reader came for — and a dimmed row deserves its name as
        // much as a live one. The wire agrees; `commands` is discovery data a
        // human inspects BEFORE approving (`norte_core::plugins`).
        //
        // A blank title is dropped rather than stored: `label` would return it
        // verbatim and `norte_help::label_or_id` would then fall back to the id
        // anyway, so keeping it would only make the map lie about what it knows.
        let titles: std::collections::HashMap<String, String> = plugins
            .iter()
            .flat_map(|p| {
                let plugin_id = p.id.clone();
                p.commands.iter().map(move |c| {
                    (
                        format!("plugin:{plugin_id}:{}", c.id),
                        plugin_label(&c.title),
                    )
                })
            })
            .filter(|(_, title)| !title.is_empty())
            .collect();
        if let Some(help) = self.help.as_mut() {
            let owned: Vec<norte_proto::methods::PluginInfo> =
                plugins.into_iter().cloned().collect();
            help.set_plugins(&owned);
        }
        self.help_chords = std::sync::Arc::new(self.help_chords.with_plugins(active, titles));
    }

    /// Records a `connection.degraded` notification (#44).
    ///
    /// One entry per scheme, so a second scheme does not evict the first, and a
    /// repeat for the SAME scheme replaces the entry AND becomes the newest:
    /// the latest report is the one worth naming, and the old one described the
    /// same session. Past `DEGRADED_MAX` the oldest is dropped — see the
    /// `degraded` field for why a wire-fed collection needs a ceiling.
    pub fn note_degraded(&mut self, d: norte_proto::methods::ConnectionDegraded) {
        self.degraded.retain(|old| old.scheme != d.scheme);
        self.degraded.push_back(d);
        while self.degraded.len() > DEGRADED_MAX {
            self.degraded.pop_front();
        }
    }

    /// The degradation reported for `scheme`, if any.
    ///
    /// This is the fact the help's [`norte_frontend::availability::Facts`]
    /// carries. It vetoes nothing on its own — see that field's rustdoc: the
    /// wire vocabulary means "unencrypted", not "unusable".
    #[must_use]
    pub fn degraded_for(&self, scheme: &str) -> Option<&norte_proto::methods::ConnectionDegraded> {
        self.degraded.iter().rev().find(|d| d.scheme == scheme)
    }

    /// The persistent status-bar banner, or `None` when nothing degraded.
    ///
    /// It always NAMES a connection — the most recent one — and appends how
    /// many others there are. Reporting a bare count ("2 connections in
    /// plaintext") beats silently overwriting one report with another, but
    /// combined with never clearing it means the identity of every degraded
    /// session is lost for the rest of the session, and "which one?" is the
    /// only question this indicator exists to answer.
    ///
    /// Scheme and host are masked (`norte_frontend::display_name`) and the
    /// host is clamped: both are wire-supplied strings, and the status bar is
    /// the one place in the TUI they reach unfiltered. A host of control
    /// characters or bidi overrides is exactly what an attacker sends to a
    /// security indicator.
    ///
    /// Never cleared once set — see the `degraded` field for why that is a
    /// decision and not an omission.
    #[must_use]
    pub fn connection_banner(&self) -> Option<String> {
        /// Cells the host gets before the middle ellipsis takes over. Long
        /// enough for a real FQDN, short enough that the banner cannot push
        /// everything else off the status bar.
        const HOST_MAX: usize = 48;

        let last = self.degraded.back()?;
        let scheme = norte_frontend::display_name(last.scheme.as_bytes()).0;
        let host = norte_frontend::middle_ellipsis(
            &norte_frontend::display_name(last.host.as_bytes()).0,
            HOST_MAX,
        );
        let otras = self.degraded.len() - 1;
        if otras == 0 {
            return Some(ta(
                "status-connection-degraded",
                &[("scheme", &scheme), ("host", &host)],
            ));
        }
        Some(ta(
            "status-connections-degraded",
            &[
                ("scheme", &scheme),
                ("host", &host),
                ("n", &otras.to_string()),
            ],
        ))
    }

    /// Anota que esta sesión no está registrando sus mutaciones (#177).
    ///
    /// Idempotente: el core avisa una vez por EPISODIO, y si alguna vez avisara
    /// dos, la segunda solo reescribe el mismo hecho.
    pub fn note_no_journal(&mut self, why: norte_core::embedded::NoJournal) {
        self.no_journal = Some(why);
    }

    /// Y que volvió a registrarlas (#179): la ventana de propiedad se reabrió.
    ///
    /// Apagar el indicador es la mitad que importa. Un «NO se registra» que no
    /// sabe volverse «ya sí» miente en cuanto el ocupante de paso suelta el
    /// fichero, y miente sobre lo único que la barra dice de TODA la sesión.
    ///
    /// **Lo que el indicador no sabe decir** es que una operación ya en marcha
    /// conserva el veredicto con el que empezó (#205): si se recupera el
    /// journal mientras un borrado largo sigue corriendo sin registrar, la
    /// barra se apaga y ese borrado sigue sin dejar filas. El aviso de
    /// recuperación lo dice con todas las letras —«desde tu PRÓXIMA
    /// operación»— pero lo borra la siguiente tecla. Distinguirlo en la barra
    /// pediría que el core expusiera cuántas Tasks van fijadas a no-registrar,
    /// y no lo hace.
    pub fn note_journal_recovered(&mut self) {
        self.no_journal = None;
    }

    /// El aviso PERSISTENTE de sesión sin journal, o `None` si sí se registra.
    ///
    /// Frase fija y sin el motivo: el motivo salió por `message` cuando ocurrió
    /// (con el error del core saneado), y la barra de estado tiene que caber.
    ///
    /// **DOS frases, porque son dos hechos distintos (#178).** `Busy` es «esto
    /// pasó y no quedó anotado» — la sesión muta, sin registro. `Failed` es
    /// «esto NO va a pasar»: la sesión rehúsa mutar hasta que el fichero se
    /// arregle. Enseñar «no se puede deshacer» sobre la segunda diría lo
    /// contrario de lo que ocurre, y esa clase de indicador es justo lo que
    /// #178 vino a quitar.
    #[must_use]
    pub fn journal_banner(&self) -> Option<String> {
        use norte_core::embedded::NoJournal as N;
        self.no_journal.as_ref().map(|why| match why {
            N::Failed(_) => t("status-journal-refused"),
            // `Busy` y cualquier motivo futuro: el mensaje conservador es el
            // que no promete que la mutación se haya parado.
            _ => t("status-no-journal"),
        })
    }

    /// Los dos indicadores persistentes de la barra, JUNTOS.
    ///
    /// Juntos y no en ramas distintas del `if` de la barra: son dos hechos
    /// simultáneos y de la misma clase —seguridad, hasta el final de la
    /// sesión—, así que elegir uno escondería el otro para siempre. El del
    /// journal va primero: «nada de esto se puede deshacer» pesa más que «esta
    /// conexión va en claro», y es el único que habla de TODA la sesión.
    #[must_use]
    pub fn persistent_banner(&self) -> Option<String> {
        match (self.journal_banner(), self.connection_banner()) {
            (Some(j), Some(c)) => Some(format!("{j}  {c}")),
            (Some(uno), None) | (None, Some(uno)) => Some(uno),
            (None, None) => None,
        }
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

    /// Candidatas a hidratar de la VENTANA visible (#52, listado lazy) en
    /// LOS DOS panes — ambos se pintan a la vez, así que sondear solo la
    /// entrada enfocada dejaba las columnas Tamaño/Fecha en blanco en todo
    /// lo demás. El pane con foco va primero; la selección DENTRO de cada
    /// pane es del modelo compartido
    /// ([`norte_frontend::PaneState::needs_stat_window`], regla 7).
    #[must_use]
    pub fn needs_stat_window(&self, radius: usize) -> Vec<(usize, VPath)> {
        let mut out = Vec::new();
        for pane_idx in [self.focus(), self.focus() ^ 1] {
            out.extend(
                self.panes[pane_idx]
                    .needs_stat_window(radius)
                    .into_iter()
                    .map(|p| (pane_idx, p)),
            );
        }
        out
    }

    /// Paths de la fila SELECCIONADA del panel de diferencias que valen la
    /// pena sondear con un `stat` (#157): un lado con entrada, de tipo
    /// `File` (los directorios y los enlaces no tienen un tamaño que un
    /// `stat` corriente resuelva — mismo criterio que
    /// [`Self::focused_needs_stat`]), sin `size` ya, y que
    /// [`Self::compare_size_probed`] no haya pedido todavía.
    ///
    /// `None` cuando no hay panel abierto o su fila seleccionada no tiene
    /// nada que hidratar — que es el caso normal en cuanto la sonda ya
    /// contestó, así que el run loop no vuelve a pedir lo mismo cada frame.
    #[must_use]
    pub fn compare_size_probe_targets(&self) -> Vec<VPath> {
        let Some(view) = &self.compare else {
            return Vec::new();
        };
        let Some(row) = view.pane.selected_row() else {
            return Vec::new();
        };
        [row.left.as_ref(), row.right.as_ref()]
            .into_iter()
            .flatten()
            .filter(|e| {
                e.kind == EntryKind::File
                    && e.size.is_none()
                    && !self.compare_size_probed.contains(&e.path)
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// Mete el resultado de la sonda #157 en la caché de presentación
    /// ([`Self::compare_size_hints`]) y lo marca sondeado
    /// ([`Self::compare_size_probed`]) pase lo que pase — un `stat` que
    /// falló tampoco se reintenta hasta la próxima comparación, mismo
    /// criterio que el pane normal con `last_probed`.
    pub fn hydrate_compare_size(&mut self, generation: u64, path: VPath, size: Option<u64>) {
        // #198: de OTRA comparación. Ni el tamaño ni la marca de sondeado —
        // marcarlo dejaría a la comparación viva sin pedirlo nunca, que es la
        // mitad silenciosa del mismo fallo.
        if generation != self.compare_generation {
            return;
        }
        self.compare_size_probed.insert(path.clone());
        if let Some(size) = size {
            self.compare_size_hints.insert(path, size);
        }
    }

    /// La comparación que empieza. Vacía la caché de tamaños y su dedup, y
    /// AVANZA la generación: lo uno sin lo otro es el fallo de #198.
    pub fn begin_compare_generation(&mut self) {
        self.compare_size_hints.clear();
        self.compare_size_probed.clear();
        self.compare_generation = self.compare_generation.wrapping_add(1);
    }

    /// La comparación a la que pertenecen las tablas de tamaños ahora mismo.
    /// El run loop la guarda al lanzar la sonda y la devuelve al hidratar.
    #[must_use]
    pub fn compare_generation(&self) -> u64 {
        self.compare_generation
    }

    /// El pane con foco, mutable.
    pub fn focused_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focus]
    }

    /// Alterna el foco entre los dos panes (Tab, keymap mc).
    pub fn switch_focus(&mut self) {
        self.focus ^= 1;
    }

    /// Exchanges the two panes and everything `App` keeps beside them
    /// (`pane.swap`).
    ///
    /// Touches no disk: no listing is refetched, nothing can fail, and the
    /// marks, the filter, the sort and the cursor all survive because the
    /// WHOLE pane moves rather than being rebuilt.
    ///
    /// The focus stays on the same physical SIDE on purpose. Moving it along
    /// with the content would make the command a no-op from where the reader
    /// sits: they would still be looking at the same listing, just on the
    /// other half of the screen.
    ///
    /// The history moves WITH the pane, because it belongs to the content and
    /// not to the side of the screen. Left behind, each pane would offer to
    /// take the reader "back" to places that content has never been.
    ///
    /// NOT the whole story: the run loop keeps its own state indexed by pane
    /// (the paginated fills in flight, the decoration fetches, the stat-probe
    /// dedup, the live search run) which `App` cannot see.
    /// `main::reconcile_swap` is the other half, and the two are driven
    /// together by `Cd::Swapped`.
    pub fn swap_panes(&mut self) {
        self.panes.swap(0, 1);
        self.history.swap(0, 1);
        // Lo ÚNICO que queda como rastro de que hubo intercambio: todo lo
        // demás viaja con su pane, así que quien compare por lado no ve
        // moverse nada (ver [`Self::swap_seq`]).
        self.swap_seq = self.swap_seq.wrapping_add(1);
    }

    /// How many times [`Self::swap_panes`] has run.
    ///
    /// Only useful as an equality check against a previously read value: any
    /// difference means the two sides changed places, so anything holding a
    /// pane INDEX from before now names the other side's content.
    #[must_use]
    pub const fn swap_seq(&self) -> u64 {
        self.swap_seq
    }

    /// Da el foco al pane `i`. Un índice fuera de `0|1` se IGNORA (el
    /// invariante de `focus` es de la propia `App`): el único emisor de
    /// índices que no son literales es el hit test del ratón, y ahí un
    /// índice imposible es un bug nuestro, no algo que deba dejar el foco
    /// apuntando a un pane que no existe.
    pub fn set_focus(&mut self, i: usize) {
        debug_assert!(i < self.panes.len(), "pane fuera de rango");
        if i < self.panes.len() {
            self.focus = i;
        }
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

    /// Abre el picker de columnas para el pane con foco (#108 7a): parte del
    /// set efectivo de su scheme y de su orden VIVO (el del pane, no el de
    /// config — un sort de cabecera previo no se pierde al abrir). Con el
    /// catálogo cacheado del scheme (#117): el picker OFRECE los attrs
    /// anunciados por el provider y cicla sus formatos por hint.
    /// `plugins` = el catálogo VIVO (`plugin.list`), para ofrecer también las
    /// columnas que declaran los plugins aprobados y activados (#120). Vacío
    /// (el fetch falló, o no hay daemon) = se ofrecen solo builtins y attrs:
    /// una columna de plugin que no se puede confirmar que exista no se
    /// ofrece, igual que un attr no anunciado.
    pub fn open_columns_picker(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        let scheme = self.focused().dir().scheme().to_owned();
        let sort = self.focused().sort();
        self.columns_picker = Some(
            norte_frontend::columns_picker::ColumnsPicker::open_with_catalog(
                &self.columns,
                &scheme,
                sort,
                self.attr_catalog(&scheme),
                plugins,
            ),
        );
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
            self.abandon_shortcut_capture();
        }
    }

    /// A modal is taking the keyboard, so the shortcut editor stops ASKING for
    /// a blind keypress (K3c).
    ///
    /// Capture mode paints "press the new key" and the reader is primed to
    /// press anything at all. A modal that arrives on its own — a policy
    /// approval off the bus, a collision at the end of a copy — takes the keys
    /// and is painted on top, so that next key answers a question the reader
    /// did not know was being asked, and on `Modal::ApproveAgentOp` the letter
    /// `y` approves an agent operation. The key still reaches the modal (that
    /// part is the modal's right); what norte must not do is keep inviting it.
    ///
    /// The editor itself SURVIVES: the reader gets their list back after
    /// answering, unless the modal arm of the key chain retires it
    /// (`main::close_stale_overlays`).
    fn abandon_shortcut_capture(&mut self) {
        if let Some(sc) = &mut self.shortcuts {
            sc.cancel_capture();
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
            self.abandon_shortcut_capture();
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

    /// Abre la confirmación de una copia o un movimiento `from` → `to`.
    ///
    /// **Fuente ÚNICA de qué somete una transferencia**, la tecla (F5/F6) y
    /// el arrastre por igual. No es estilo: un drop es una mutación, y una
    /// segunda ruta —aunque hoy naciera idéntica— se quedaría sin la
    /// confirmación, sin el modal de colisión, sin la entrada de journal o
    /// sin el undo en cuanto una de las dos cambiara. Por eso el drop no
    /// construye ningún modal: pide el mismo que pediría `pane.copy`.
    /// Gemela de `transfer_modal` en la GUI.
    ///
    /// `promoted` es la única diferencia entre las dos entradas, y solo dice
    /// SOBRE QUÉ actúa: `None` = las marcas del pane (o el cursor si no hay
    /// ninguna — `marked_paths`, la fuente única de siempre); `Some(idx)` =
    /// esa fila sola, porque el gesto se promovió desde una fila SIN marcar
    /// y las marcas del pane —si las hay— son otra cosa que el usuario no
    /// está arrastrando.
    ///
    /// Con UN solo ítem el nombre de destino es EDITABLE (#105); el lote
    /// multi sigue en el confirm de lista (no hay un nombre único). No-op si
    /// no hay nada que transferir: jamás un diálogo sobre un lote vacío.
    pub fn open_transfer(
        &mut self,
        kind: TransferKind,
        from: usize,
        to: usize,
        promoted: Option<usize>,
    ) {
        let items: Vec<VPath> = match promoted {
            Some(idx) => self.panes[from]
                .entries()
                .get(idx)
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default(),
            None => self.panes[from].marked_paths(),
        };
        let to_dir = self.panes[to].dir().clone();
        match items.as_slice() {
            [] => {}
            [one] => {
                // `from_marks` decide si el envío CONSUME la selección
                // ([`Self::transfer_name_submitted`]). Un arrastre promovido
                // jamás la consume: la promoción cambia lo que el gesto
                // HACE, no lo que está seleccionado — y lo marcado puede ser
                // otra cosa que el usuario no ha soltado.
                let from_marks = promoted.is_none() && self.panes[from].marks_len() > 0;
                self.open_transfer_name_with(kind, from, one.clone(), to_dir, from_marks);
            }
            _ => {
                self.modal = Some(Modal::ConfirmTransfer {
                    kind,
                    items,
                    to: to_dir,
                });
            }
        }
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

    /// Aplica a `pane` el orden de SU scheme según la config (#108 b4):
    /// llamado al aterrizar un cd (el scheme puede haber cambiado) y al
    /// arrancar. `set_sort` es no-op si el spec no cambia.
    pub fn apply_scheme_sort(&mut self, pane: usize) {
        let scheme = self.panes[pane].dir().scheme().to_owned();
        let spec = self.columns.sort_for(&scheme);
        self.panes[pane].set_sort(spec);
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

    /// Abre el rename in situ (shift+F6, #105): Move con destino en el
    /// PADRE del propio `from` — no el dir del pane, que en el pane VIRTUAL
    /// de búsqueda es la raíz del walk y renombraría moviendo el hit de
    /// sitio. Siempre sobre el cursor (las marcas no renombran en bloque —
    /// eso sería un batch-rename, otra feature). No-op sobre una raíz.
    pub fn open_rename(&mut self) {
        let Some(from) = self.focused().selected().map(|e| e.path.clone()) else {
            return;
        };
        let Some(to_dir) = from.parent() else {
            return;
        };
        self.open_transfer_name_with(TransferKind::Move, self.focus, from, to_dir, false);
    }

    /// El modal de nombre editable. `from_pane` es el pane de ORIGEN y no se
    /// da por hecho que sea el que tiene el foco: un drop nace en el pane
    /// donde bajó el botón, y de ahí sale la reinterpretación de nombres
    /// (#57) con la que se siembra el campo.
    fn open_transfer_name_with(
        &mut self,
        kind: TransferKind,
        from_pane: usize,
        from: VPath,
        to_dir: VPath,
        from_marks: bool,
    ) {
        let original = from
            .file_name()
            .map_or(Vec::new(), |n| n.as_bytes().to_vec());
        let enc = self.panes[from_pane].name_encoding();
        // Prefill = lo que el pane PINTA (#98/M1): bajo reinterpretación,
        // un nombre no-UTF8 se decodifica (#57) en vez de pasar por lossy
        // — editar produce el texto que se VE; sin tocar siguen mandando
        // los bytes originales.
        let name = match (enc, std::str::from_utf8(&original)) {
            (_, Ok(s)) => s.to_owned(),
            (Some(e), Err(_)) => norte_encoding::decode_name(&original, e),
            (None, Err(_)) => String::from_utf8_lossy(&original).into_owned(),
        };
        self.modal = Some(Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            original,
            touched: false,
            from_marks,
            enc,
            error: None,
        });
    }

    /// Añade un carácter al nombre en curso (#105). Marca `touched`: desde
    /// el primer edit, el nombre es el TEXTO. No-op sin el modal.
    pub fn transfer_name_push(&mut self, c: char) {
        if let Some(Modal::TransferName {
            name,
            touched,
            error,
            ..
        }) = &mut self.modal
        {
            if name.chars().count() >= MARK_PATTERN_MAX_CHARS {
                return;
            }
            name.push(c);
            *touched = true;
            *error = None;
        }
    }

    /// Borra el último carácter (#105). Marca `touched` SOLO si borró algo
    /// (review MINOR-5: un pop vacío no debe estrechar la vía de bytes
    /// originales).
    pub fn transfer_name_pop(&mut self) {
        if let Some(Modal::TransferName {
            name,
            touched,
            error,
            ..
        }) = &mut self.modal
            && name.pop().is_some()
        {
            *touched = true;
            *error = None;
        }
    }

    /// Cancela sin transferir — mismo contrato guarded que
    /// [`Self::cancel_mkdir`].
    pub fn cancel_transfer_name(&mut self) {
        if !matches!(self.modal, Some(Modal::TransferName { .. })) {
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

    /// Valida y devuelve `(kind, from, dest)` SIN cerrar el modal (misma
    /// disciplina que [`Self::mkdir_confirm`]: cierra el submit que encoló,
    /// vía [`Self::transfer_name_submitted`]). Reglas: sin tocar → los
    /// BYTES originales (regla 1); tocado → los bytes del texto, y un texto
    /// que aún contiene U+FFFD (residuo del prefill lossy de un nombre
    /// hostil) se RECHAZA — confirmarlo escribiría mojibake real en disco.
    /// El guard no distingue residuo de intención: también un U+FFFD
    /// TECLEADO a propósito se rechaza (asimetría deliberada con el mkdir,
    /// que no tiene prefill lossy del que heredar residuos). Bajo
    /// reinterpretación (#57), un nombre TOCADO escribe los bytes UTF-8 del
    /// texto decodificado — transcodifica a propósito: «ver el nombre bien
    /// y arreglarlo» es el caso de uso, y el intocado sigue byte-exacto.
    /// `dest == from` también se rechaza (no-op; en rename, «mismo
    /// nombre»). El nombre pasa por [`norte_proto::Segment`] (ni vacío, ni
    /// `/`, ni NUL, ni `.`/`..`).
    pub fn transfer_name_confirm(&mut self) -> Option<(TransferKind, VPath, VPath)> {
        let Some(Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            original,
            touched,
            ..
        }) = &self.modal
        else {
            return None;
        };
        let bytes = if *touched {
            if name.contains('\u{FFFD}') {
                let msg = norte_i18n::t("msg-transfer-name-fffd");
                self.transfer_name_set_error(msg);
                return None;
            }
            name.as_bytes().to_vec()
        } else {
            original.clone()
        };
        let (kind, from, to_dir) = (*kind, from.clone(), to_dir.clone());
        match norte_proto::Segment::new(bytes) {
            Ok(seg) => {
                let dest = to_dir.join(seg);
                if dest == from {
                    self.transfer_name_set_error(norte_i18n::t("msg-transfer-name-same"));
                    return None;
                }
                Some((kind, from, dest))
            }
            Err(e) => {
                self.transfer_name_set_error(e.to_string());
                None
            }
        }
    }

    /// Cierra el modal tras un submit que SÍ encoló (#105) y, si el origen
    /// era la MARCA, la CONSUME (review MAJOR-1 — misma doctrina que el
    /// lote: la selección se consume al ENVIAR). Esc y los fallos jamás
    /// consumen.
    pub fn transfer_name_submitted(&mut self) {
        if let Some(Modal::TransferName { from_marks, .. }) = &self.modal {
            if *from_marks {
                self.focused_mut().clear_marks();
            }
            self.modal = None;
            self.open_next_pending();
        }
    }

    /// Deja el diagnóstico de un intento fallido (#105): el texto tecleado
    /// sobrevive para corregir.
    pub fn transfer_name_set_error(&mut self, msg: String) {
        if let Some(Modal::TransferName { error, .. }) = &mut self.modal {
            *error = Some(msg);
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

    /// Abre el prompt de `pane.command-line` (#135).
    pub fn open_command_line(&mut self) {
        self.modal = Some(Modal::CommandLine {
            command: String::new(),
            error: None,
        });
    }

    /// Añade un carácter a la línea de comandos. No-op sin su modal. Mismo
    /// tope en `chars` que el resto de los prompts de texto libre.
    /// Alcanzar el tope DEJA DIAGNÓSTICO, a diferencia del resto de los
    /// prompts de texto libre (review de S4, M4). Un nombre de directorio
    /// truncado falla al crearse y se ve; una línea de comandos truncada
    /// CORRE — `rm -rf /proyecto-viejo` recortado a `rm -rf /proyecto` es una
    /// orden distinta, no journaleada y no deshacible. Callarse el recorte
    /// aquí es dejar pulsar Enter a ciegas.
    pub fn command_line_push(&mut self, c: char) {
        if let Some(Modal::CommandLine { command, error }) = &mut self.modal {
            if command.chars().count() >= MARK_PATTERN_MAX_CHARS {
                *error = Some(ta(
                    "modal-command-line-too-long",
                    &[("max", &MARK_PATTERN_MAX_CHARS.to_string())],
                ));
                return;
            }
            command.push(c);
            *error = None;
        }
    }

    /// Borra el último carácter de la línea. No-op sin su modal.
    pub fn command_line_pop(&mut self) {
        if let Some(Modal::CommandLine { command, error }) = &mut self.modal {
            command.pop();
            *error = None;
        }
    }

    /// Cancela `Modal::CommandLine` sin ejecutar nada (mismo contrato y guard
    /// que [`Self::cancel_mkdir`]: un modal de DECISIÓN jamás se cierra por
    /// aquí).
    pub fn cancel_command_line(&mut self) {
        if !matches!(self.modal, Some(Modal::CommandLine { .. })) {
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

    /// Valida y devuelve la línea; NO cierra el modal — el caller cierra con
    /// [`Self::command_line_submitted`] tras dejar la suspensión pendiente
    /// (misma disciplina que [`Self::ai_rename_confirm`]).
    ///
    /// La línea se devuelve TAL CUAL, sin `trim`: solo se usa el recortado
    /// para decidir si está vacía. Un comando que empieza por espacio es una
    /// convención real de bash/zsh (`HISTCONTROL=ignorespace`), y recortarlo
    /// cambiaría en silencio lo que el usuario escribió.
    pub fn command_line_confirm(&mut self) -> Option<String> {
        if let Some(Modal::CommandLine { command, error }) = &mut self.modal {
            if command.trim().is_empty() {
                *error = Some(t("modal-command-line-empty"));
                return None;
            }
            return Some(command.clone());
        }
        None
    }

    /// Cierra el prompt tras dejar la suspensión encolada (misma disciplina
    /// de cierre que [`Self::ai_rename_submitted`]).
    pub fn command_line_submitted(&mut self) {
        if matches!(self.modal, Some(Modal::CommandLine { .. })) {
            self.modal = None;
            self.open_next_pending();
        }
    }

    /// Abre el prompt de instrucción del rename IA (M4-IA).
    pub fn open_ai_rename(&mut self) {
        self.modal = Some(Modal::AiRenameInstruction {
            instruction: String::new(),
            error: None,
        });
    }

    /// Añade un carácter a la instrucción en curso. No-op sin su modal.
    /// Tope en `chars` como el patrón (#103): un paste accidental no
    /// desborda el modal; el límite REAL (4 KiB) lo pone el daemon.
    pub fn ai_rename_push(&mut self, c: char) {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            if instruction.chars().count() >= MARK_PATTERN_MAX_CHARS {
                return;
            }
            instruction.push(c);
            *error = None;
        }
    }

    /// Borra el último carácter de la instrucción. No-op sin su modal.
    pub fn ai_rename_pop(&mut self) {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            instruction.pop();
            *error = None;
        }
    }

    /// Cancela `Modal::AiRenameInstruction` sin lanzar nada — el Esc de ESTE
    /// modal de texto libre (mismo contrato y guard que
    /// [`Self::cancel_mkdir`]: un modal de DECISIÓN jamás se cierra por
    /// aquí).
    pub fn cancel_ai_rename(&mut self) {
        if !matches!(self.modal, Some(Modal::AiRenameInstruction { .. })) {
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

    /// Valida y devuelve la instrucción; NO cierra el modal — el caller
    /// cierra con [`Self::ai_rename_submitted`] tras SPAWNEAR la petición
    /// (audit INFO-7: el spawn en sí no falla; los fallos del modelo llegan
    /// ASÍNCRONOS y salen por la barra, `msg-ai-rename-failed`, no por el
    /// modal). Una instrucción vacía deja su diagnóstico aquí mismo y
    /// devuelve `None`.
    pub fn ai_rename_confirm(&mut self) -> Option<String> {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            let text = instruction.trim();
            if text.is_empty() {
                *error = Some(t("modal-ai-rename-empty-instruction"));
                return None;
            }
            return Some(text.to_owned());
        }
        None
    }

    /// Cierra el prompt tras un lanzamiento que SÍ salió (M4-IA): misma
    /// disciplina de cierre que [`Self::mkdir_submitted`] (jamás dejar una
    /// pendiente esperando).
    pub fn ai_rename_submitted(&mut self) {
        if matches!(self.modal, Some(Modal::AiRenameInstruction { .. })) {
            self.modal = None;
            self.open_next_pending();
        }
    }

    /// Deja un diagnóstico bajo el campo con el texto CONSERVADO. Audit
    /// INFO-7: en el flujo real solo cubre diagnósticos SÍNCRONOS previos al
    /// spawn (hoy, la instrucción vacía la marca el propio
    /// [`Self::ai_rename_confirm`]); un fallo del modelo llega ASYNC con el
    /// prompt ya cerrado y va a la barra, jamás por aquí.
    pub fn ai_rename_set_error(&mut self, msg: String) {
        if let Some(Modal::AiRenameInstruction { error, .. }) = &mut self.modal {
            *error = Some(msg);
        }
    }

    /// Desplaza la ventana del plan IA (audit MAJOR-3): `down` avanza una
    /// pareja, si no retrocede; clampado a `[0, len - ventana]`. No-op sin
    /// su modal. El scroll JAMÁS confirma ni cancela — `dialog_action`
    /// devuelve `None` para `dialog.up`/`dialog.down` en este modal (fuera
    /// de su allowlist de decisión) y el run loop enruta esos comandos aquí.
    pub fn ai_plan_scroll(&mut self, down: bool) {
        if let Some(Modal::AiRenamePlan {
            entries, offset, ..
        }) = &mut self.modal
        {
            let max = entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT);
            *offset = if down {
                (*offset + 1).min(max)
            } else {
                offset.saturating_sub(1)
            };
        }
    }

    /// Deja el plan del LOTE (§17) en el modal del plan IA que lo estaba
    /// esperando. Devuelve `false` si no había ninguno —el humano ya cerró el
    /// modal, o el plan está RETENIDO tras otro modal y lo rellena el run
    /// loop—, para que el caller sepa que tiene que buscarlo en su stash.
    ///
    /// Solo rellena un modal en [`norte_frontend::BatchPlan::Pending`]: una
    /// respuesta jamás pisa a un plan ya resuelto.
    pub fn settle_ai_batch_plan(&mut self, resuelto: &norte_frontend::BatchPlan) -> bool {
        if let Some(Modal::AiRenamePlan { plan, .. }) = &mut self.modal
            && *plan == norte_frontend::BatchPlan::Pending
        {
            *plan = resuelto.clone();
            return true;
        }
        false
    }

    /// Abre el prompt de consulta de la búsqueda semántica (M4-IA-2).
    pub fn open_semantic_search(&mut self) {
        self.modal = Some(Modal::SemanticQuery {
            query: String::new(),
            error: None,
        });
    }

    /// Añade un carácter a la consulta en curso. No-op sin su modal.
    /// Mismo tope en `chars` que la instrucción IA: un paste accidental no
    /// desborda el modal; el límite REAL lo pone el daemon.
    pub fn semantic_push(&mut self, c: char) {
        if let Some(Modal::SemanticQuery { query, error }) = &mut self.modal {
            if query.chars().count() >= MARK_PATTERN_MAX_CHARS {
                return;
            }
            query.push(c);
            *error = None;
        }
    }

    /// Borra el último carácter de la consulta. No-op sin su modal.
    pub fn semantic_pop(&mut self) {
        if let Some(Modal::SemanticQuery { query, error }) = &mut self.modal {
            query.pop();
            *error = None;
        }
    }

    /// Cancela `Modal::SemanticQuery` sin lanzar nada — el Esc de ESTE modal
    /// de texto libre (mismo contrato y guard que [`Self::cancel_ai_rename`]:
    /// un modal de DECISIÓN jamás se cierra por aquí).
    pub fn cancel_semantic(&mut self) {
        if !matches!(self.modal, Some(Modal::SemanticQuery { .. })) {
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

    /// Valida y devuelve la consulta; NO cierra el modal — el caller cierra
    /// con [`Self::semantic_submitted`] tras SPAWNEAR la petición (mismo
    /// contrato que [`Self::ai_rename_confirm`]: los fallos del modelo llegan
    /// ASÍNCRONOS y salen por la barra, `msg-semantic-failed`, no por el
    /// modal). Una consulta vacía deja su diagnóstico aquí mismo y devuelve
    /// `None`.
    pub fn semantic_confirm(&mut self) -> Option<String> {
        if let Some(Modal::SemanticQuery { query, error }) = &mut self.modal {
            let text = query.trim();
            if text.is_empty() {
                *error = Some(t("modal-semantic-empty-query"));
                return None;
            }
            return Some(text.to_owned());
        }
        None
    }

    /// Cierra el prompt tras un lanzamiento que SÍ salió (M4-IA-2): misma
    /// disciplina de cierre que [`Self::ai_rename_submitted`] (jamás dejar
    /// una pendiente esperando).
    pub fn semantic_submitted(&mut self) {
        if matches!(self.modal, Some(Modal::SemanticQuery { .. })) {
            self.modal = None;
            self.open_next_pending();
        }
    }

    /// Deja un diagnóstico bajo el campo con el texto CONSERVADO. Como
    /// [`Self::ai_rename_set_error`]: solo cubre diagnósticos SÍNCRONOS
    /// previos al spawn (hoy, la consulta vacía la marca el propio
    /// [`Self::semantic_confirm`]); un fallo del modelo llega ASYNC con el
    /// prompt ya cerrado y va a la barra, jamás por aquí.
    pub fn semantic_set_error(&mut self, msg: String) {
        if let Some(Modal::SemanticQuery { error, .. }) = &mut self.modal {
            *error = Some(msg);
        }
    }

    /// Mueve el cursor de hits (`down` = true baja); la ventana sigue al
    /// cursor, clampada en ambos extremos. No-op sin su modal. El scroll
    /// JAMÁS confirma ni cancela — `dialog_action` devuelve `None` para
    /// `dialog.up`/`dialog.down` en este modal (fuera de su allowlist de
    /// decisión) y el run loop enruta esos comandos aquí (molde
    /// [`Self::ai_plan_scroll`]).
    pub fn semantic_cursor(&mut self, down: bool) {
        if let Some(Modal::SemanticHits {
            hits,
            offset,
            cursor,
        }) = &mut self.modal
        {
            if hits.is_empty() {
                return;
            }
            *cursor = if down {
                (*cursor + 1).min(hits.len() - 1)
            } else {
                cursor.saturating_sub(1)
            };
            if *cursor < *offset {
                *offset = *cursor;
            }
            if *cursor >= *offset + SEMANTIC_HIT_LIMIT {
                *offset = *cursor + 1 - SEMANTIC_HIT_LIMIT;
            }
        }
    }

    /// Abre el popup de navegación (spec 2026-07-18): historial del pane
    /// con foco (más reciente primero) o la copia de hotlist. Los items se
    /// construyen YA saneados aquí (`nav_item_display`); una entrada de
    /// hotlist inválida se muestra con su aviso y destino `None`.
    ///
    /// # Panics
    /// Con `NavPopupKind::Volumes`: esos items necesitan un fetch ASYNC
    /// contra `Backend::volumes` que este método (síncrono, sin `Backend`)
    /// no puede hacer — `main.rs` abre ese kind vía
    /// [`Self::open_volumes_popup`], nunca aquí.
    pub fn open_nav_popup(&mut self, kind: NavPopupKind) {
        let enc = self.focused().name_encoding();
        let items: Vec<NavItem> = match kind {
            NavPopupKind::Volumes => unreachable!(
                "Volumes se abre vía `open_volumes_popup` (design §D), nunca `open_nav_popup`"
            ),
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
            target_pane: self.focus,
            include_pseudo: false,
        });
    }

    /// Abre el popup de volúmenes (`pane.select-drive`/`-left`/`-right`,
    /// design §D) con `items` YA construidos por [`volume_items`] — `main.rs`
    /// hace el fetch async contra `Backend::volumes` y llama aquí, mismo
    /// reparto que el resto de este popup: main.rs es I/O, app.rs es estado y
    /// presentación.
    ///
    /// `pane` es el LADO que `Confirm` va a navegar: el foco para
    /// `pane.select-drive`, un lado fijo para `-left`/`-right`
    /// independientemente del foco actual. `include_pseudo` es el modo con el
    /// que se pidió ESTA lista — el toggle de dentro del popup vuelve a
    /// llamar aquí con el valor invertido, así que esto es literalmente una
    /// re-apertura, no un caso especial.
    pub fn open_volumes_popup(&mut self, pane: usize, include_pseudo: bool, items: Vec<NavItem>) {
        self.nav_popup = Some(NavPopup {
            kind: NavPopupKind::Volumes,
            items,
            cursor: 0,
            name_input: None,
            target_pane: pane,
            include_pseudo,
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

/// The resolver [`App::new`] starts with: the ORTHODOX preset with no user
/// and no project layer, in the language of the environment.
///
/// `main.rs` OVERWRITES [`App::help_chords`] at startup and on every hot
/// reload with the effectives actually in force, exactly as it does with
/// `help_lines`, `palette_rows` and `dialog_hints` — a rebind that does not
/// reach the resolver is a help page that teaches the OLD key. This default
/// only exists so `App::new` stays infallible for the render tests and the
/// paths that never load a keymap at all.
///
/// Built ONCE per process: `Effective::build_for` materialises the whole
/// merged keymap for three screens, and `App::new` runs in every render test.
///
/// Which makes this dead weight in the BINARY, and deliberately so: `App::new`
/// forces it and `main.rs` throws it away on the very next lines. It earns its
/// place in the render tests, which build an `App` and never load a keymap —
/// and nowhere else.
///
/// One consequence a test author has to know: the LABEL of every row is frozen
/// here at `Lang::from_env()`, because `TuiChords` resolves labels once, when
/// it is built. The prose and the chords do not depend on it, but a test that
/// asserts on a help label through this default reads whatever locale the
/// machine running it happens to have — so such a test must build its own
/// `TuiChords` with the language it means (`snapshots_ui.rs`'s `open_help`
/// does exactly that).
fn default_help_chords() -> std::sync::Arc<crate::help::TuiChords> {
    static DEFAULT: std::sync::LazyLock<std::sync::Arc<crate::help::TuiChords>> =
        std::sync::LazyLock::new(|| {
            // Both `expect`s rest on the same invariant: the preset and the
            // vocabulary are BINARY CONSTANTS, and `tests/keymap.rs` merges
            // this exact combination for all three screens — an invalid one
            // fails the suite, never a user's session. `presets()` itself
            // panics on the same grounds.
            let (_, preset) = crate::keymap::presets()
                .into_iter()
                .find(|(n, _)| *n == "orthodox")
                .expect("`presets()` always ships the orthodox preset");
            // The `dialog` effective merges `[global]` too, so the vocabulary
            // is the UNION — `DIALOG_COMMANDS` alone would reject the preset.
            let known: Vec<&str> = crate::keymap::COMMANDS
                .iter()
                .copied()
                .chain(crate::keymap::DIALOG_COMMANDS.iter().copied())
                .collect();
            let eff = |screen| {
                crate::keymap::Effective::build_for(&preset, &[], &known, screen)
                    .expect("the embedded orthodox preset merges for every screen")
            };
            std::sync::Arc::new(crate::help::TuiChords::new(
                &eff(crate::keymap::Screen::Browse),
                &eff(crate::keymap::Screen::Viewer),
                &eff(crate::keymap::Screen::Dialog),
                norte_i18n::Lang::from_env(),
            ))
        });
    std::sync::Arc::clone(&DEFAULT)
}

/// State of the help overlay (H3b): the shared navigation model, the
/// generated keyboard page, and the body as last laid out.
///
/// The body is PRE-RENDERED into the state rather than laid out by the
/// painter, which is this repo's existing idiom (`help_lines`,
/// [`App::palette_rows`], [`crate::hints::DialogHints`] are all precomputed
/// and rebuilt on hot reload). The reason is concrete:
/// `HelpState::reveal`/`clamp_scroll` need the laid-out LINE COUNT, `draw_*`
/// only ever gets a `&App`, and a renderer that cannot tell the model what it
/// laid out leaves `body_scroll` unbounded — a reader who pages past the end
/// gets a permanently blank body.
#[derive(Debug, Clone)]
pub struct HelpView {
    /// Sidebar, body scroll, filter, history and focus.
    pub state: norte_frontend::help::HelpState,
    /// The effective-keymap cheatsheet ([`crate::help::build`]), the body of
    /// the synthetic `keys` entry — already styled (K3b: an unavailable row
    /// is dimmed there, not here). Rebuilt on hot reload with everything else
    /// derived from the keymap.
    pub keys_lines: Vec<ratatui::text::Line<'static>>,
    /// Body lines and the action→line map of whatever `state.current()` is,
    /// laid out for `width`. See [`HelpView::refresh`].
    ///
    /// `'static` because a rendering that borrowed from [`Self::state`] would
    /// make this a self-referential struct. That is free for the corpus —
    /// `HelpState::topic` hands back a `&'static Topic`, so `render_topic`
    /// produces a `Rendered<'static>` outright — and costs one clone of the
    /// VISIBLE page for a plugin's, whose topic `HelpState` owns
    /// (`crate::help_render::into_static`, H3e).
    body: crate::help_render::Rendered<'static>,
    /// Plugin ids whose page has already been ASKED FOR in this overlay (H3e).
    ///
    /// `HelpState::plugin_needs_fetch` is a POLLING question, not an event: it
    /// keeps answering `Some(id)` until the page is installed, so the run loop
    /// would re-issue the request on every frame — and forever against a daemon
    /// that cannot answer. This set is what turns it into an event, and it
    /// covers BOTH halves at once: the request in flight, and the ones that
    /// already answered. A success stops answering by itself
    /// (`install_plugin_topic`); a failure is what needs remembering.
    ///
    /// Lives on the VIEW, so its scope is the open overlay: closing and
    /// reopening the help asks again, which is the only retry a reader has and
    /// the only one they can ask for.
    asked: std::collections::BTreeSet<String>,
    /// Publisher of each plugin of the snapshot, keyed by id — already masked
    /// and capped ([`plugin_label`]).
    ///
    /// Kept here and not in `norte_frontend::help::PluginNode` because only one
    /// caller needs it and only once: `norte_help::parse_untrusted` takes the
    /// publisher as the attribution of the page it is about to build, and the
    /// page is parsed when its `plugin.help` answer arrives — long after the
    /// snapshot that knew the publisher was taken.
    publishers: std::collections::BTreeMap<String, String>,
    /// `true` when the overlay was opened while a modal was ALREADY on screen
    /// (H3c).
    ///
    /// It decides who owns the keys, and the two directions are different
    /// events:
    ///
    /// * opened FROM a modal (this flag `true`) the help owns them. The reader
    ///   asked to read about the question in front of them, so `Esc` has to put
    ///   them back in front of it rather than answer it, and the modal's own
    ///   verbs stay unreachable meanwhile — an agent operation is approved by
    ///   looking at it, never by a key pressed blind through a page.
    /// * a modal ARRIVING over an already-open help (this flag `false`) closes
    ///   the help instead, exactly as it closes the palette and the settings
    ///   overlay: the next key must land where the pixels point.
    ///
    /// Two consequences, both deliberate. The modal keeps being painted LAST
    /// ([`crate::ui::draw`]), so a help opened over it does not hide the
    /// question — the box stays on top of the page, and its verbs simply do
    /// nothing until the help closes. And a help page left open over an agent
    /// approval lets its TTL expire, which DENIES the agent: fail-closed, which
    /// is the direction to fail in.
    pub over_modal: bool,
}

impl HelpView {
    /// Opens the overlay on the index topic of `lang`, with `keys_lines` as
    /// the body of the synthetic keyboard entry.
    ///
    /// The label of that entry is resolved HERE and handed to the model:
    /// `norte_frontend::help` has no Fluent access on purpose, and this is
    /// the frontend that names the page. Resolving it once, at the seam,
    /// keeps the sidebar and the filter looking at the same string — a
    /// painter-side special case would only make the row unfindable by the
    /// name it wears.
    ///
    /// The body starts EMPTY: nothing has been laid out yet because nothing
    /// knows how wide the terminal is. [`refresh`](Self::refresh) is what
    /// fills it, and the run loop calls it before every paint.
    #[must_use]
    pub fn new(lang: norte_help::Lang, keys_lines: Vec<ratatui::text::Line<'static>>) -> Self {
        Self {
            state: norte_frontend::help::HelpState::new(lang, t("help-topic-keys")),
            keys_lines,
            body: crate::help_render::Rendered {
                lines: Vec::new(),
                action_lines: Vec::new(),
            },
            asked: std::collections::BTreeSet::new(),
            publishers: std::collections::BTreeMap::new(),
            over_modal: false,
        }
    }

    /// Opens the overlay on the page for `context`, falling back to the index
    /// when no page claims it.
    ///
    /// The fallback is not a papering-over: `norte_help::check_contexts` fails
    /// the documentation gate for a context with no page, so the pages that are
    /// still missing are on a shrinking allowlist and nothing else can reach
    /// here. The index is the least surprising place to land.
    ///
    /// The contextual page arrives as the ROOT of the trail
    /// (`HelpState::open_as_root`): `F1` putting the reader on a page is not
    /// navigation the reader did, so `Esc` must close the overlay instead of
    /// walking back to an index they never asked for.
    ///
    /// `over_modal` is the caller's answer to "was a modal already on screen?"
    /// — see the field for what it decides.
    #[must_use]
    pub fn new_at(
        lang: norte_help::Lang,
        keys_lines: Vec<ratatui::text::Line<'static>>,
        context: &str,
        over_modal: bool,
    ) -> Self {
        if let Some(topic) = norte_help::topic_for_context(lang, context) {
            return Self::new_at_topic(lang, keys_lines, &topic.id, over_modal);
        }
        let mut view = Self::new(lang, keys_lines);
        view.over_modal = over_modal;
        view
    }

    /// Opens the help on a page the caller already picked, instead of on a
    /// context the corpus resolves (H3c).
    ///
    /// The sibling of [`new_at`](Self::new_at) for the other bridge into the
    /// corpus: `F1` on a command palette row opens the page that DOCUMENTS that
    /// command ([`norte_help::topic_for_command`]), which is a topic id in hand
    /// and not a place the reader is standing in.
    ///
    /// Same trail treatment for the same reason — the page arrives as the ROOT
    /// (`HelpState::open_as_root`), because being PUT on a page is not
    /// navigation the reader did and `Esc` has to close the overlay rather than
    /// walk back to an index they never saw.
    #[must_use]
    pub fn new_at_topic(
        lang: norte_help::Lang,
        keys_lines: Vec<ratatui::text::Line<'static>>,
        topic: &norte_help::TopicId,
        over_modal: bool,
    ) -> Self {
        let mut view = Self::new(lang, keys_lines);
        view.over_modal = over_modal;
        view.state.open_as_root(topic);
        view
    }

    /// Lays the open page out for `width` and re-establishes the scroll
    /// invariants: clamps `body_scroll` to what exists, and reveals the
    /// focused action when the body has the focus.
    ///
    /// Call after ANY change to what is shown — opening a topic, going back,
    /// moving either cursor, editing the filter, a resize, a hot reload —
    /// and before painting. Cheap: the corpus is static and eight topics.
    pub fn refresh(
        &mut self,
        chords: &crate::help::TuiChords,
        width: usize,
        height: usize,
        theme: &crate::theme::TuiTheme,
    ) {
        let lang = self.state.lang();
        self.body = if let Some(topic) = self.state.topic() {
            // A corpus page. Asked for FIRST and through `topic()` rather than
            // `current_topic()` for the lifetime alone: this one is `'static`,
            // so the common case keeps rendering straight into the field with
            // nothing cloned. `current_topic()` resolves the corpus first too,
            // so the two can never pick different pages.
            crate::help_render::render_topic(topic, lang, chords, width, theme)
        } else if let Some(topic) = self.state.current_topic() {
            // A plugin page (H3e): owned by the model, so the rendering that
            // borrows it has to be detached before it can be stored.
            crate::help_render::into_static(crate::help_render::render_topic(
                topic, lang, chords, width, theme,
            ))
        } else if self.state.current().as_str() == norte_frontend::help::KEYS_ID {
            // The synthetic `keys` page: its body is the effective keymap,
            // generated text with no runnable rows and therefore no action
            // map — a chord is not something Enter runs.
            //
            // Keyed on the ID and not on "no topic resolved", which is the same
            // branch written the safe way round. Three different states answer
            // `None` to `current_topic()` — the keyboard page, a plugin page in
            // flight, and a `current` naming a page that no longer exists — and
            // only the first is this one. `HelpState` can reach the third:
            // `rebuild_rows` moves the body onto a surviving row, but with the
            // sidebar left EMPTY by a filter there is nowhere to move to and it
            // deliberately keeps showing what was being read. Unreachable in
            // this binary (the catalogue is only ever installed on the open
            // path, before any filter), but "unreachable" is a claim about
            // callers and this is a claim about the id.
            // Already styled (K3b: an unavailable row is dimmed by
            // `crate::help::build`, not here) — no `Line::raw` mapping left
            // to do.
            crate::help_render::Rendered {
                lines: self.keys_lines.clone(),
                action_lines: Vec::new(),
            }
        } else {
            // A plugin page still in flight — and any other page that resolves
            // to nothing. EMPTY, never the keyboard page: nothing else on
            // screen tells the two apart, and the whole cheatsheet appearing
            // under an extension's name would read as that extension's own
            // documentation.
            crate::help_render::Rendered {
                lines: Vec::new(),
                action_lines: Vec::new(),
            }
        };
        self.state.clamp_scroll(self.body.lines.len());
        // The guard is not defensive noise: a topic with neither commands nor
        // `see_also` has no line to reveal, and `HelpState` only refuses the
        // FOCUS on an empty action list — the cursor itself can be stale for
        // one frame after a filter rebuilt the page under it.
        if self.state.focus() == norte_frontend::help::Focus::Body
            && let Some(&line) = self.body.action_lines.get(self.state.action_cursor())
        {
            self.state.reveal(line, height);
        }
    }

    /// Body lines to paint and the line each action landed on.
    #[must_use]
    pub fn body(&self) -> (&[ratatui::text::Line<'static>], &[usize]) {
        (&self.body.lines, &self.body.action_lines)
    }

    /// The plugin id whose page must be fetched NOW, claiming it so the next
    /// call does not ask again (H3e). `None` when there is nothing to fetch or
    /// the open page has already been asked for.
    ///
    /// The claim is what makes `HelpState::plugin_needs_fetch` — which polls,
    /// see the view's own `asked` set — usable from a run loop that visits it
    /// every frame.
    /// Claiming BEFORE the request, rather than after it succeeds, is the whole
    /// point: the case worth guarding is the one where the answer never comes.
    ///
    /// ```
    /// use norte_frontend::help::PluginNode;
    /// use norte_help::{Lang, TopicId};
    /// use norte_tui::app::HelpView;
    ///
    /// let mut view = HelpView::new(Lang::En, Vec::new());
    /// view.state.set_plugins(vec![PluginNode {
    ///     id: "acme.ftp".to_owned(),
    ///     title: "FTP".to_owned(),
    ///     has_help: true,
    ///     active: true,
    /// }]);
    /// view.state.open(&TopicId::new("acme.ftp"));
    /// assert_eq!(view.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
    /// // Asked once. A page that never arrives is not asked for again.
    /// assert_eq!(view.claim_plugin_fetch(), None);
    /// ```
    pub fn claim_plugin_fetch(&mut self) -> Option<String> {
        let id = self.state.plugin_needs_fetch()?.to_owned();
        self.asked.insert(id.clone()).then_some(id)
    }

    /// Installs the plugin catalogue this overlay was opened with (H3e).
    ///
    /// The ONE ingest point for third-party text into the help model: `name`
    /// and `publisher` are masked and capped HERE ([`plugin_label`]), exactly as
    /// the palette does with a plugin's `description`, because
    /// `norte_frontend::help::PluginNode` documents its `title` as already safe
    /// and the model masks nothing.
    ///
    /// A node is ACTIVE when the plugin is approved AND enabled. That decides
    /// whether its command rows are runnable, never whether its page shows: a
    /// human reads a plugin's documentation precisely in order to decide
    /// whether to enable it.
    ///
    /// A BLANK `name` falls back to the plugin's id. `name` is required in the
    /// manifest but never checked for content, so `name = "\u{3164}\u{3164}"`
    /// — HANGUL FILLERs, which are not whitespace and survive masking — is a
    /// legal manifest whose sidebar row paints as an empty line under the
    /// `Extensions` header: a page the reader can move onto, open, and read,
    /// attached to a name that says nothing. The id is the one identifier the
    /// host assigns, so it is what the row falls back to; it goes through
    /// [`plugin_label`] like everything else, because until the id itself is
    /// validated at this seam it is no more trustworthy than the name.
    ///
    /// `norte_help::is_blank_id` and not `str::trim().is_empty()`: the filler
    /// characters this exists to catch are not whitespace, so a trim-based
    /// check answers "not blank" about a string that paints nothing. Asked
    /// AFTER masking, so a name of zero-width spaces — hazards rather than
    /// invisibles — has already become `U+FFFD` and counts as blank too.
    ///
    /// The extension MANAGER has the same gap and is deliberately left alone:
    /// its row carries the version and the approval badges beside the name, so
    /// a blank name there is an odd-looking row rather than an unattributed
    /// one. Seen and judged, not missed.
    ///
    /// # The id is validated HERE, and a bad one is DROPPED
    ///
    /// `PluginInfo.id` arrives over the wire. Our own host will only ever send
    /// a reverse-DNS id it validated, but this frontend does not get to assume
    /// the peer enforced what our host enforces — the same reasoning
    /// `norte_frontend::help::HelpState::set_plugins` gives for its own
    /// duplicate and corpus-collision guards. An id is a LOOKUP KEY that flows
    /// straight into `TopicId`, into `plugin_needs_fetch`, and back out as the
    /// argument to `plugin.help`, so it is the one field that must be right
    /// rather than merely paintable.
    ///
    /// DROPPED, never rewritten. Masking an id is not a safety measure — it is
    /// not injective, so it silently maps two distinct plugins onto one row —
    /// and a repaired id would be a key that resolves to nothing or, worse, to
    /// something else. Refusing the node is the only answer that cannot lie:
    /// the reader loses a help page for a plugin the host should not have
    /// announced, and `norte doctor` is where that gets diagnosed. Same
    /// discipline as `norte_help::parse_untrusted`'s command keys, which are
    /// refused rather than rewritten for exactly this reason.
    ///
    /// It also bounds the work: `is_valid_plugin_id` caps the length at 128, so
    /// a megabyte of `id` costs one rejected comparison instead of a masked,
    /// capped copy per plugin and a `TopicId` the sidebar filter folds on every
    /// keystroke.
    pub fn set_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        let plugins: Vec<&norte_proto::methods::PluginInfo> = plugins
            .iter()
            .filter(|p| norte_core::is_valid_plugin_id(&p.id))
            .collect();
        self.publishers = plugins
            .iter()
            .map(|p| (p.id.clone(), plugin_label(&p.publisher)))
            .collect();
        self.state.set_plugins(
            plugins
                .iter()
                .map(|p| {
                    let named = plugin_label(&p.name);
                    norte_frontend::help::PluginNode {
                        id: p.id.clone(),
                        title: if norte_help::is_blank_id(&named) {
                            plugin_label(&p.id)
                        } else {
                            named
                        },
                        has_help: p.has_help,
                        active: p.approved && p.enabled,
                    }
                })
                .collect(),
        );
    }

    /// Who to attribute `id`'s page to, ready to hand to
    /// `norte_help::parse_untrusted`. `None` for a plugin outside the snapshot
    /// or one that declares no publisher — a blank attribution is worse than
    /// none, because the badge would print `published by ` with nothing after
    /// it, which reads as a rendering fault rather than as an absence.
    ///
    /// Blankness is `norte_help::is_blank_id`, not `str::trim().is_empty()`:
    /// `publisher` is a required TOML field that the manifest never checks for
    /// content, and `"\u{3164}"` (HANGUL FILLER) is not whitespace, so a
    /// trim-based check would call it a publisher. Asked AFTER
    /// [`plugin_label`] has masked, so a publisher of zero-width spaces — a
    /// hazard rather than an invisible — is already `U+FFFD` by the time this
    /// looks, and counts as blank too.
    #[must_use]
    pub fn publisher_of(&self, id: &str) -> Option<String> {
        self.publishers
            .get(id)
            .filter(|p| !norte_help::is_blank_id(p))
            .cloned()
    }

    /// `true` while the body shows the generated keyboard page.
    ///
    /// The same predicate [`refresh`](Self::refresh) branches on, so the two
    /// cannot disagree about which body is on screen. Since H3e "no corpus
    /// page" is no longer enough — a plugin page, fetched or in flight, is not
    /// a corpus page either — so both ask the ID.
    #[must_use]
    pub fn on_keys_page(&self) -> bool {
        self.state.current().as_str() == norte_frontend::help::KEYS_ID
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

/// Si una navegación se REGISTRA en el rastro del pane, o es el rastro
/// reproduciéndose a sí mismo.
///
/// Sin esta distinción `nav.back` se alimenta de su propio rastro: volver de
/// B a A registraría "estuve en B", así que el siguiente back devuelve a B y
/// el lector oscila entre dos directorios — el defecto exacto que el rastro
/// existe para evitar, un nivel más arriba.
///
/// Vive aquí (y no junto al `cd` del binario) porque [`Modal::TrustHostKey`]
/// lo TRANSPORTA: el reintento tras confiar en la host key debe reanudar la
/// MISMA navegación que el TOFU interrumpió, y la lib no puede referirse a
/// un tipo declarado en `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trail {
    /// El usuario pidió este movimiento: entra en la MRU y en el rastro, y
    /// poda la rama de forward.
    Record,
    /// `nav.back`/`nav.forward` están reproduciendo, y ESTE es el paso que
    /// están dando. El rastro ya lo sabe, así que la navegación no se
    /// registra; el paso viaja dentro porque un `Replay` sin saber en qué
    /// sentido va no se puede deshacer, y quien tenga que rebobinarlo puede
    /// no ser quien lo empezó: el TOFU suspende la navegación y la respuesta
    /// al modal la termina, minutos después y desde otro sitio del código.
    ///
    /// Va DENTRO de la variante, y no en un campo aparte junto a ella, para
    /// que «registrar» y «tener sentido» no puedan contradecirse: un
    /// `Record` con sentido, o un `Replay` sin él, serían estados que alguien
    /// tendría que acordarse de no construir.
    Replay(TrailStep),
}

impl Trail {
    /// El paso del rastro que esta navegación está dando, si es que está
    /// dando alguno. `None` para un [`Trail::Record`]: no salió del rastro,
    /// así que no hay nada que rebobinar si acaba mal.
    #[must_use]
    pub fn step(self) -> Option<TrailStep> {
        match self {
            Self::Record => None,
            Self::Replay(step) => Some(step),
        }
    }
}

/// Which way `nav.back`/`nav.forward` are walking the trail. The two are the
/// same operation mirrored, so they share one body rather than two arms that
/// must be kept in step by hand.
///
/// Vive aquí por el mismo motivo que [`Trail`], que lo transporta: el modal
/// TOFU ([`Modal::TrustHostKey`]) suspende una navegación que puede ser un
/// paso del rastro, y quien responda al modal necesita saber en qué sentido
/// iba para deshacerlo si la respuesta acaba abandonándola.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailStep {
    /// `nav.back`.
    Back,
    /// `nav.forward`.
    Forward,
}

impl TrailStep {
    /// Fluent id for "there is nothing this way". A key that goes silent is
    /// indistinguishable from a broken one, so the exhausted trail SAYS so.
    #[must_use]
    pub fn empty_message(self) -> &'static str {
        match self {
            Self::Back => "msg-nav-no-back",
            Self::Forward => "msg-nav-no-forward",
        }
    }
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
///
/// Sin `Eq` (M4-IA-2): [`Modal::SemanticHits`] arrastra el `score: f64` de
/// [`norte_proto::methods::SemanticHit`], que es solo `PartialEq` — como su
/// tipo de proto.
#[derive(Debug, Clone, PartialEq)]
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
        /// El pane que estaba navegando cuando saltó el TOFU. El modal lo
        /// CARGA porque la navegación interrumpida no es necesariamente la
        /// del pane con el foco (`pane.mirror` manda el OTRO pane a un sitio
        /// mientras el foco se queda quieto): reintentar contra el foco
        /// reanudaría en el pane EQUIVOCADO.
        pane: usize,
        /// Si la navegación interrumpida se REGISTRA en el rastro o es el
        /// rastro reproduciéndose — y, en ese caso, QUÉ paso estaba dando
        /// ([`Trail::step`]). Se transporta por el mismo motivo que `pane`:
        /// el reintento debe ser la MISMA navegación que el TOFU interrumpió,
        /// no una nueva.
        ///
        /// El paso viaja porque este modal es el ÚNICO sitio donde una
        /// navegación sobrevive a quien la empezó: `walk_trail` ya devolvió
        /// `Suspended` y no rebobinó nada (el reintento iba a terminar el
        /// paso), así que si la respuesta al modal acaba abandonando la
        /// navegación —denegar, o un reintento que falla— el rastro se queda
        /// creyendo que el lector se fue de donde sigue estando. Quien
        /// responde al modal rebobina, y para eso necesita el sentido.
        trail: Trail,
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
    /// Nombre de destino editable (#105): F5/F6 de UN solo ítem, y el
    /// rename in situ (shift+F6 — `to_dir` es el MISMO dir). Multi-ítem
    /// sigue en [`Modal::ConfirmTransfer`]: no hay un nombre único que
    /// editar. Texto libre como [`Modal::Mkdir`].
    TransferName {
        /// Copy o Move (rename = Move con `to_dir` == dir de `from`).
        kind: TransferKind,
        /// Origen, bytes exactos.
        from: VPath,
        /// Directorio destino (el del otro pane; el propio en rename).
        to_dir: VPath,
        /// El nombre como TEXTO editable (lo que se pinta, enmascarado).
        /// Solo manda si `touched`; sin tocar, el confirm usa `original`.
        name: String,
        /// Bytes ORIGINALES del nombre de `from` (regla 1): un F5 sin
        /// editar copia estos bytes, jamás la forma lossy del prefill.
        original: Vec<u8>,
        /// ¿Se editó alguna vez? El primer push/pop lo fija: desde ahí el
        /// nombre es el texto (doctrina #103: editas lo que VES).
        touched: bool,
        /// El origen era la MARCA (no el cursor): el submit que encola la
        /// CONSUME (#105 review MAJOR-1 — mc/TC: la selección se consume al
        /// enviar, también con un solo ítem). Un rename (cursor) jamás.
        from_marks: bool,
        /// Reinterpretación de nombres del pane al ABRIR (#98/M1 y #105
        /// review MAJOR-2): el prefill de un nombre no-UTF8 es el TEXTO que
        /// el pane pinta bajo ella (decode #57), no el lossy — sin esto un
        /// fichero cp437 era irrenombrable (todo edit tropezaba con el
        /// guard de U+FFFD). El render del dir destino usa la misma.
        enc: Option<norte_encoding::NameEncoding>,
        /// Diagnóstico del último intento inválido.
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
    /// `pane.command-line` (#135). Texto libre, molde [`Modal::Mkdir`]: la
    /// línea CRUDA del usuario, enmascarada al pintarla.
    ///
    /// Lo que Enter hace con ella NO pasa por el core: se la lleva el shell
    /// con la TUI suspendida, que es el usuario actuando con sus propios
    /// permisos y no una mutación de norte (design §D — el journal no ve nada
    /// de esto, y decirlo así es más honesto que meter entradas
    /// irreversibles en la cadena).
    ///
    /// # Es el único sitio de norte donde lo pintado es código a aprobar
    ///
    /// Dos consecuencias que la review de S4 dejó decididas, no heredadas:
    ///
    /// - **El pegado multilínea confirma en el primer salto** (encoding H2).
    ///   La TUI no tiene bracketed paste —un pegado llega como pulsaciones
    ///   sueltas y crossterm mapea `\n` a `Enter`—, así que la primera línea
    ///   se envía sola. El RESTO no se ejecuta: [`crate::app::PendingShell`]
    ///   se drena con el type-ahead ya descartado, así que no llega ni al
    ///   hijo ni al despacho de la TUI como comandos. Está dicho en los
    ///   límites honestos del tema `shell`. El arreglo completo (activar
    ///   bracketed paste y enrutar `Event::Paste` en las SEIS superficies de
    ///   texto libre que hay) es trabajo de la TUI entera, no de este item, y
    ///   hacerlo a medias rompería el pegado en las otras cinco.
    /// - **ZWJ y NBSP pasan sin marcar.** `must_mask` los permite a sabiendas
    ///   (fidelidad de emoji), lo cual es correcto para un NOMBRE de fichero.
    ///   Aquí `git\u{200D}status` se lee igual que `git status` y el shell lo
    ///   parte distinto. Se acepta el mismo trato que el resto de campos —una
    ///   excepción por superficie sería peor de razonar— y se hace constar:
    ///   lo peligroso de verdad (RLO y compañía) SÍ se enmascara.
    CommandLine {
        /// Lo tecleado hasta ahora.
        command: String,
        /// Diagnóstico del último intento inválido, bajo el campo.
        error: Option<String>,
    },
    /// Prompt de instrucción del rename IA (M4-IA). Texto libre, molde
    /// [`Modal::Mkdir`]: la instrucción CRUDA del usuario, enmascarada al
    /// pintarla (una instrucción llega por paste con bidi/invisibles tan
    /// fácil como un nombre).
    AiRenameInstruction {
        /// Lo tecleado hasta ahora.
        instruction: String,
        /// Diagnóstico del último intento fallido, bajo el campo.
        error: Option<String>,
    },
    /// Plan de rename IA revisable (M4-IA): superficie de DECISIÓN. Confirmar
    /// aplica (contenido revisado por el humano); Esc/cancel descarta.
    AiRenamePlan {
        /// Dir sobre el que se aplican los renames.
        dir: VPath,
        /// Parejas from→to del modelo (proto, UTF-8 garantizado).
        entries: Vec<norte_proto::methods::AiRenameEntry>,
        /// Primera pareja visible de la ventana (audit MAJOR-3): el plan
        /// ENTERO es revisable por scroll ([`App::ai_plan_scroll`]) — sin
        /// esto, la cola de un plan > [`AI_RENAME_PAIR_LIMIT`] se aplicaba
        /// sin poder verse.
        offset: usize,
        /// El plan del LOTE que contestó `fs.rename_batch_plan` (spec §17,
        /// ADR 0042): veredictos, si es aplicable y el `plan_hash` que hay
        /// que devolver para ejecutar EXACTAMENTE lo que se enseñó.
        ///
        /// Nace [`norte_frontend::BatchPlan::Pending`] —el modal abre y se
        /// rellena cuando el core contesta— y sin un plan APLICABLE
        /// confirmar está DESHABILITADO ([`dialog_action`]): no hay hash
        /// aprobado que mandar.
        plan: norte_frontend::BatchPlan,
    },
    /// Prompt de consulta de la búsqueda semántica (M4-IA-2). Texto libre,
    /// molde [`Modal::AiRenameInstruction`]: la consulta CRUDA del usuario,
    /// enmascarada al pintarla (una consulta llega por paste con
    /// bidi/invisibles tan fácil como una instrucción).
    SemanticQuery {
        /// Lo tecleado hasta ahora.
        query: String,
        /// Diagnóstico del último intento fallido, bajo el campo.
        error: Option<String>,
    },
    /// Hits de la búsqueda semántica (M4-IA-2): superficie de DECISIÓN con
    /// cursor. Confirmar NAVEGA al hit bajo el cursor (cd al padre +
    /// re-anclado, molde `on_search_enter`); Esc/cancel cierra.
    SemanticHits {
        /// Hits del índice, mejor primero (proto, score siempre finito).
        hits: Vec<norte_proto::methods::SemanticHit>,
        /// Primer hit visible de la ventana (sigue al cursor).
        offset: usize,
        /// Hit resaltado — el que Enter abre.
        cursor: usize,
    },
}

/// Hits semánticos visibles a la vez en [`Modal::SemanticHits`] (ventana de
/// scroll) — la constante vive en `norte-frontend` (compartida con la GUI,
/// mismo criterio que [`AI_RENAME_PAIR_LIMIT`]); re-export para el render
/// (`ui`), el alto del modal y el clamp de [`App::semantic_cursor`].
pub use norte_frontend::SEMANTIC_HIT_LIMIT;

/// Parejas del plan IA visibles a la vez en [`Modal::AiRenamePlan`] (ventana
/// de scroll, audit MAJOR-3) — la constante vive en `norte-frontend`
/// (compartida con la GUI, quality review 78eb243 MAJOR-1); re-export para
/// el render (`ui`), el alto del modal y el clamp de [`App::ai_plan_scroll`].
pub use norte_frontend::AI_RENAME_PAIR_LIMIT;

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

/// ALLOWLIST del picker de columnas (#108 7a, `on_columns_key`, main.rs) —
/// única fuente para dispatch y para el hint generado del pie
/// (`hints::DialogHints::columns`), patrón #24. `confirm` SÍ aplica+persiste
/// (mismo criterio que [`ALLOW_PICKER`]: elegir columnas solo toca la config
/// propia, no es una mutación de datos que Enter deba proteger).
pub const ALLOW_COLUMNS: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
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

/// ALLOWLIST de DESPACHO del popup de navegación (`on_nav_popup_key`,
/// main.rs), unión de lo que History, Hotlist y Volumes aceptan: `add`/
/// `remove` los filtra el caller a `kind == Hotlist` (nada que nombrar ni
/// borrar en historial o volúmenes) y `toggle-enabled` a `kind == Volumes`
/// (el toggle "mostrar todo" no significa nada en los otros dos) — mismo
/// criterio que antes de H1.
///
/// El HINT impreso es más estrecho que esto por kind: [`ALLOW_NAV_HOTLIST`]
/// y [`ALLOW_NAV_VOLUMES`] son los que de verdad pinta cada footer (design
/// §D — el footer de volúmenes no debe ofrecer "añadir"/"borrar", que no
/// significan nada sobre un volumen montado).
pub const ALLOW_NAV_POPUP: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.add",
    "dialog.remove",
    "dialog.toggle-enabled",
    "dialog.cancel",
];

/// HINT del popup en modo HOTLIST (H1 T3): historial y volúmenes pintan el
/// suyo propio (o ninguno) — ver [`ALLOW_NAV_POPUP`] para el porqué de la
/// separación.
pub const ALLOW_NAV_HOTLIST: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.add",
    "dialog.remove",
    "dialog.cancel",
];

/// HINT del popup en modo VOLUMES (design §D): navegación, confirmar,
/// cancelar y el toggle "mostrar todo" — nada de `add`/`remove`.
pub const ALLOW_NAV_VOLUMES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.toggle-enabled",
    "dialog.cancel",
];

/// Verbs the help overlay dispatches (H3b). Navigation, confirm (run the
/// focused row or follow the focused link), cancel (close), plus its own
/// three. Nothing that mutates: the overlay itself changes no files — a
/// command it RUNS goes through the normal dispatch, with its own
/// confirmation, gate and journal entry.
pub const ALLOW_HELP: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.confirm",
    "dialog.cancel",
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
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
        // M4-IA: `AiRenamePlan` es una superficie de decisión sobre contenido
        // INICIADO y REVISADO por el humano — semántica [`ALLOW_CONFIRM`]
        // (Enter confirma, como un delete/transfer), NO el allowlist de
        // aprobación de agentes (`ALLOW_APPROVAL`, que excluye confirm).
        //
        // Con una salvedad que este brazo aparte existe para imponer (spec
        // §17): confirmar necesita un plan de lote APLICABLE. Sin plan no hay
        // `plan_hash` aprobado que mandar, y con veredictos el core no
        // ejecutaría nada — en ambos casos la tecla de confirmar queda MUDA
        // (cancelar sigue vivo), y el pie del modal deja de ofrecerla
        // (`modal-rename-batch-plan-hint-blocked`). La decisión de si un plan
        // se puede ejecutar es del core: aquí solo se lee `executable`.
        Modal::AiRenamePlan { plan, .. } => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            let confirma = matches!(cmd, "dialog.approve" | "dialog.confirm");
            if confirma && !plan.confirmable() {
                return None;
            }
            Some(if confirma {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled // dialog.deny | dialog.cancel
            })
        }
        // M4-IA-2: `SemanticHits` es igualmente una superficie de decisión
        // sobre contenido PEDIDO por el humano — Enter navega al hit bajo el
        // cursor, no muta nada.
        Modal::ConfirmDelete { .. }
        | Modal::ConfirmTransfer { .. }
        | Modal::ConfirmQuit
        | Modal::SemanticHits { .. } => {
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
        Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::CommandLine { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferName { .. } => None,
    }
}

/// What a `dialog.*` verb does inside the help overlay (H3b).
///
/// A vocabulary of its own rather than [`DialogOutcome`]: a modal answers a
/// QUESTION (confirm/deny/retry-with-a-policy) and this overlay is a reader —
/// its verbs move a cursor, follow a link and close a window. Sharing the
/// enum would force every modal to carry arms it can never produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpOutcome {
    /// One row up: a topic in the sidebar, an action in the body.
    Up,
    /// One row down.
    Down,
    /// A page up: topics in the sidebar, body lines in the body.
    PageUp,
    /// A page down.
    PageDown,
    /// Enter: open the selected topic, or run/follow the focused body row.
    Activate,
    /// Close the overlay.
    Close,
    /// Hand the focus to the other half.
    TogglePane,
    /// Back to the previously open topic.
    Back,
    /// Start typing into the sidebar filter.
    StartFilter,
}

/// Maps an already-resolved `dialog.*` command to what it means inside the
/// help overlay, or `None` for a verb the overlay does not support — the key
/// is INERT, exactly as in [`dialog_action`].
///
/// Filtered through the SAME [`ALLOW_HELP`] the footer hint is generated from
/// ([`crate::hints::DialogHints::help`]), never a second copy: a verb the
/// footer advertises and dispatch drops (or the reverse) is a hint that lies,
/// and one list cannot drift from itself.
#[must_use]
pub fn help_action(cmd: &str) -> Option<HelpOutcome> {
    if !ALLOW_HELP.contains(&cmd) {
        return None;
    }
    Some(match cmd {
        "dialog.up" => HelpOutcome::Up,
        "dialog.down" => HelpOutcome::Down,
        "dialog.page-up" => HelpOutcome::PageUp,
        "dialog.page-down" => HelpOutcome::PageDown,
        "dialog.confirm" => HelpOutcome::Activate,
        "dialog.cancel" => HelpOutcome::Close,
        "dialog.pane" => HelpOutcome::TogglePane,
        "dialog.back" => HelpOutcome::Back,
        "dialog.filter" => HelpOutcome::StartFilter,
        // Unreachable through the allowlist above, and deliberately not an
        // `unreachable!`: a verb added to `ALLOW_HELP` without an arm here is
        // an inert key, never a panic in a reader's terminal. The test
        // `help_action_accepts_exactly_the_allowlist` is what catches it.
        _ => return None,
    })
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

/// El vocabulario de CATEGORÍAS de error vive en [`norte_frontend::error`]
/// (#158, revisión de la fase C1): la GUI dice los mismos errores y no podía
/// alcanzarlo aquí, así que interpolaba el `Display` inglés en frases por lo
/// demás localizadas. Se re-exporta con el nombre de siempre porque es API
/// pública de este crate (los scripts Lua comparan contra estas claves).
pub use norte_frontend::error::{error_category, error_key};

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

    /// #52: `needs_stat_window` hidrata lo VISIBLE, no solo lo enfocado —
    /// las columnas Tamaño/Fecha salían en blanco en todas las filas salvo
    /// la del cursor. Los dos panes se pintan a la vez, así que los dos
    /// aportan candidatas (el enfocado primero); fuera del radio, no; un
    /// Dir, nunca; ya hidratada, tampoco.
    #[test]
    fn needs_stat_window_cubre_los_dos_panes_dentro_del_radio() {
        let lazy = |n: &str| {
            let mut e = file(n);
            e.size = None;
            e
        };
        let mut dir_lazy = lazy("z-dir");
        dir_lazy.kind = EntryKind::Dir;
        let izq = vec![lazy("a.txt"), lazy("b.txt"), lazy("c.txt"), dir_lazy];
        let der = vec![lazy("d.txt"), file("e.txt")];
        let mut app = App::new(Pane::new(root(), izq), Pane::new(root(), der));
        // `Pane::new` ordena (dirs primero): [z-dir, a, b, c].
        app.panes[0].set_cursor(1);

        let ventana = app.needs_stat_window(1);
        let nombres: Vec<String> = ventana
            .iter()
            .map(|(p, path)| format!("{p}:{}", path.display_lossy()))
            .collect();
        assert!(
            nombres
                .iter()
                .any(|n| n.starts_with("0:") && n.ends_with("/a.txt"))
                && nombres
                    .iter()
                    .any(|n| n.starts_with("0:") && n.ends_with("/b.txt")),
            "cursor ± radio del pane con foco: {nombres:?}"
        );
        assert!(
            !nombres.iter().any(|n| n.contains("c.txt")),
            "fuera del radio no se sondea: {nombres:?}"
        );
        assert!(
            !nombres.iter().any(|n| n.contains("z-dir")),
            "un Dir jamás se sondea: {nombres:?}"
        );
        assert!(
            nombres
                .iter()
                .any(|n| n.starts_with("1:") && n.ends_with("/d.txt")),
            "el pane SIN foco también se pinta: {nombres:?}"
        );
        assert!(
            !nombres.iter().any(|n| n.contains("e.txt")),
            "ya hidratada, no es candidata: {nombres:?}"
        );
        assert_eq!(ventana[0].0, 0, "el pane con foco va primero");

        // Un radio generoso alcanza el listado entero de ambos panes.
        assert_eq!(app.needs_stat_window(64).len(), 4);
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

    /// Fila de comparación de un lado (huérfano) con la clase y el tamaño
    /// pedidos, para las pruebas de `compare_size_probe_targets` (#157).
    fn fila_huerfana(
        id: u64,
        kind: EntryKind,
        size: Option<u64>,
    ) -> norte_proto::methods::CompareRow {
        use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareVerdict};
        norte_proto::methods::CompareRow {
            id,
            left: Some(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: root()
                    .join(norte_proto::Segment::new(format!("f{id}").into_bytes()).unwrap()),
                kind,
                size,
                mtime_ms: None,
            }),
            right: None,
            verdict: CompareVerdict::OnlyLeft,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        }
    }

    /// #157: un huérfano `File` sin `size` es candidato a la sonda de la fila
    /// seleccionada, y deja de serlo en cuanto `hydrate_compare_size` lo
    /// resuelve — con éxito o sin él, para no reintentarlo cada frame.
    #[test]
    fn compare_size_probe_targets_solo_file_sin_size_y_no_repite() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let fila = fila_huerfana(1, EntryKind::File, None);
        let path = fila.left.as_ref().unwrap().path.clone();
        view.pane.extend(vec![fila]);
        app.compare = Some(view);

        assert_eq!(
            app.compare_size_probe_targets(),
            vec![path.clone()],
            "huérfano File sin size es candidato"
        );

        // Sondeado con ÉXITO: ya no es candidato, y el hint queda puesto.
        app.hydrate_compare_size(app.compare_generation(), path.clone(), Some(42));
        assert!(
            app.compare_size_probe_targets().is_empty(),
            "ya hidratado, no se repite"
        );
        assert_eq!(app.compare_size_hints.get(&path), Some(&42));
    }

    /// #198: una sonda lanzada para la comparación A no puede aterrizar en
    /// la B. La sonda vive en el run loop y `launch_compare` no la ve, así
    /// que la única defensa es que el resultado traiga la generación bajo la
    /// que se pidió — sin eso, la caché que el rustdoc llama «de ESTA
    /// comparación» tiene dentro un tamaño de la anterior, en el panel cuyo
    /// asunto entero es si lo que estás mirando es exacto.
    #[test]
    fn una_sonda_de_la_comparacion_anterior_no_aterriza_en_la_nueva() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let fila = fila_huerfana(1, EntryKind::File, None);
        let path = fila.left.as_ref().expect("izquierda").path.clone();
        view.pane.extend(vec![fila.clone()]);
        app.compare = Some(view);
        let vieja = app.compare_generation();

        // Otra comparación empieza: la caché se vacía y la generación avanza.
        app.begin_compare_generation();
        let mut view = CompareView::new(vp("mem:///c"), vp("mem:///d"), 0, None, None);
        view.pane.extend(vec![fila]);
        app.compare = Some(view);
        assert_ne!(app.compare_generation(), vieja);

        // Llega la sonda de la comparación VIEJA.
        app.hydrate_compare_size(vieja, path.clone(), Some(42));
        assert!(
            app.compare_size_hints.is_empty(),
            "ni el tamaño de la anterior"
        );
        assert_eq!(
            app.compare_size_probe_targets(),
            vec![path.clone()],
            "ni marcado sondeado: la nueva todavía tiene que pedirlo"
        );

        // Y la de la nueva sí.
        let ahora = app.compare_generation();
        app.hydrate_compare_size(ahora, path.clone(), Some(7));
        assert_eq!(app.compare_size_hints.get(&path), Some(&7));
    }

    /// Un `stat` que falla (`None`) también se marca sondeado: no se
    /// reintenta cada frame contra un provider roto, mismo criterio que
    /// `last_probed` en el pane normal.
    #[test]
    fn compare_size_probe_targets_no_reintenta_un_stat_fallido() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let fila = fila_huerfana(1, EntryKind::File, None);
        let path = fila.left.as_ref().unwrap().path.clone();
        view.pane.extend(vec![fila]);
        app.compare = Some(view);

        app.hydrate_compare_size(app.compare_generation(), path, None);
        assert!(
            app.compare_size_probe_targets().is_empty(),
            "un fallo también se marca sondeado"
        );
        assert!(app.compare_size_hints.is_empty(), "sin hint sobre un fallo");
    }

    /// Un directorio o un huérfano que YA trae `size` no son candidatos —
    /// mismo criterio que `focused_needs_stat` para el pane normal: un `Dir`
    /// no tiene un tamaño que un `stat` corriente resuelva.
    #[test]
    fn compare_size_probe_targets_ignora_dir_y_lo_ya_hidratado() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        view.pane.extend(vec![
            fila_huerfana(1, EntryKind::Dir, None),
            fila_huerfana(2, EntryKind::File, Some(7)),
        ]);
        app.compare = Some(view);

        assert!(
            app.compare_size_probe_targets().is_empty(),
            "un Dir sin size y un File que ya lo trae no son candidatos"
        );
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

    /// Unas caps cualesquiera: lo que se prueba es el CACHÉ por scheme, no
    /// qué flags trae el provider.
    fn caps_de_test() -> norte_proto::Capabilities {
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::RENAME_ATOMIC,
            max_path: None,
        }
    }

    /// Las caps se cachean por LOCALIZACIÓN, y por la misma razón que el
    /// catálogo de atributos se pide: `fs.capabilities` devuelve las dos
    /// mitades en UNA llamada y la TUI ya la hace para las columnas. Tirar la
    /// mitad de caps y luego sondear otra vez sería pagar dos rondas por un
    /// dato que ya llegó.
    #[test]
    fn las_caps_se_cachean_por_localizacion() {
        let mut app = app_dos_panes();
        let mem = vp("mem:///");
        assert!(app.caps(&mem).is_none(), "sin sembrar, no se inventa nada");
        app.insert_caps(&mem, caps_de_test());
        assert!(app.caps(&mem).is_some());
        assert!(
            app.caps(&vp("sftp://ejemplo.org/")).is_none(),
            "un scheme no responde por otro"
        );
    }

    /// MAJOR-1: `sftp` no es UN sitio. Dos hosts del mismo scheme son dos
    /// backends distintos, y el caché tiene que contarlos aparte o el primero
    /// que contesta decide por todos los demás durante la sesión entera. Hoy
    /// ningún provider del árbol declara `READ_ONLY` por localización (el
    /// archivo y los plugins lo deciden por scheme), así que la clave por
    /// scheme sola no fallaba — por suerte, no por diseño, y `App::caps` es un
    /// accesor general que invita a leer cualquier flag.
    #[test]
    fn dos_authorities_del_mismo_scheme_no_se_responden() {
        let a = vp("sftp://a.org/");
        let b = vp("sftp://b.org/");
        let mut app = App::new(
            Pane::new(a.clone(), Vec::new()),
            Pane::new(b.clone(), Vec::new()),
        );
        app.insert_caps(
            &a,
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
        );
        assert!(app.pane_read_only(0), "a.org dijo que es de solo lectura");
        assert!(
            app.caps(&b).is_none(),
            "a b.org no se le ha preguntado nada todavía"
        );
        assert!(
            !app.pane_read_only(1),
            "b.org no puede heredar el veto de a.org: son dos backends"
        );
    }

    /// Antes de que llegue la primera respuesta, la respuesta honesta es «no
    /// lo sé», y quien pregunta cae al criterio SINTÁCTICO (el scheme dice si
    /// es un archivo comprimido). Lo que no puede hacer es afirmar que se
    /// puede escribir.
    #[test]
    fn sin_caps_todavia_el_solo_lectura_lo_decide_el_scheme() {
        let app = app_dos_panes();
        assert!(!app.pane_read_only(0), "mem:// no es de solo lectura");

        let dentro_de_un_zip = app_en("zip+file:///a.zip/!", "file:///casa");
        assert!(
            dentro_de_un_zip.pane_read_only(0),
            "un scheme de archivo es de solo lectura por construcción"
        );
        assert!(!dentro_de_un_zip.pane_read_only(1));
    }

    /// Cuando las caps SÍ llegaron mandan ellas: un provider que anuncia
    /// `READ_ONLY` sobre un scheme que sintácticamente no lo es (un montaje
    /// remoto en solo lectura) se veta igual.
    #[test]
    fn con_caps_manda_el_flag_read_only() {
        let mut app = app_dos_panes();
        let dir = app.panes[0].dir().clone();
        app.insert_caps(
            &dir,
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
        );
        assert!(app.pane_read_only(0));
        app.insert_caps(&dir, caps_de_test());
        assert!(!app.pane_read_only(0), "sin el flag, escribible");
    }

    /// Los hechos que la ayuda congela salen de los MISMOS predicados que usan
    /// los brazos de `dispatch`: un `.zip` se ENTRA en la TUI (`nav.enter`
    /// compone el scheme) aunque sea un File, y `pane.view` quiere File o
    /// Symlink. Derivarlos otra vez aquí sería atenuar filas que la app
    /// ejecutaría.
    #[test]
    fn los_hechos_de_la_ayuda_siguen_a_los_predicados_del_dispatch() {
        let mut app = app_dos_panes();
        // El cursor está sobre un File normal: no se entra, se ve.
        let f = app.help_facts();
        assert!(!f.enterable, "un fichero cualquiera no se entra");
        assert!(f.viewable);
        assert!(f.rename_single, "shift+F6 renombra UNA: la del cursor");
        assert!(!f.source_read_only && !f.dest_read_only);
        assert!(!f.degraded);

        // Un `.zip` ES entrable en la TUI aunque su kind sea File.
        let zip = Pane::new(
            root(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: root().join(norte_proto::Segment::new(b"a.zip".to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(1),
                mtime_ms: None,
            }],
        );
        let app_zip = App::new(zip, pane_con(&["b"]));
        assert!(
            app_zip.help_facts().enterable,
            "en la TUI un .zip se entra: la ayuda no puede decir lo contrario"
        );

        // Y la degradación del scheme del pane con foco llega al hecho.
        app.note_degraded(degradacion_de_test("mem", "sin-host"));
        assert!(app.help_facts().degraded);
    }

    /// Con VARIAS marcas la ayuda NO atenúa shift+F6, porque la TUI lo
    /// ejecuta: `Command::PaneRename` va a `open_rename`, que renombra
    /// `selected()` y no mira las marcas. Atenuarlo sería el fallo exacto que
    /// H3d existe para no cometer — apagar una fila que la app habría corrido,
    /// que enseña al lector a no volver a intentarlo.
    ///
    /// (La GUI sí se niega con selección múltiple, y su menú lo sigue haciendo:
    /// `norte_gui::context_menu`, `renombrar_es_una_sola_entrada_y_la_de_ia_es_otra`.
    /// El hecho es del llamador precisamente porque las dos respuestas son
    /// correctas.)
    #[test]
    fn con_varias_marcas_la_ayuda_no_atenua_renombrar() {
        use norte_help::ChordResolver as _;

        let mut app = App::new(pane_con(&["a", "b", "c"]), pane_con(&["z"]));
        app.focused_mut().toggle_mark_and_advance();
        app.focused_mut().toggle_mark_and_advance();
        assert_eq!(
            app.focused().marked_paths().len(),
            2,
            "hay DOS marcas: el caso que se atenuaba"
        );
        assert!(app.help_facts().rename_single);

        app.freeze_help_facts();
        assert!(
            app.help_chords.availability("pane.rename").is_available(),
            "la TUI renombra la del cursor con marcas puestas: la ayuda no puede negarlo"
        );
        // Y dentro de un archivo sí se apaga, por el backend — el veto real
        // sigue en pie.
        let mut zip = App::new(
            Pane::new(vp("zip+file:///a.zip/!"), Vec::new()),
            pane_con(&["z"]),
        );
        zip.freeze_help_facts();
        assert_eq!(
            zip.help_chords.availability("pane.rename").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// Congelar los hechos al abrir la ayuda: el resolver que la vista usa
    /// pasa a responder con los hechos de ESE momento.
    #[test]
    fn congelar_los_hechos_reescribe_el_resolver_de_la_ayuda() {
        use norte_help::ChordResolver as _;

        let mut app = app_en("zip+file:///a.zip/!", "zip+file:///b.zip/!");
        assert!(
            app.help_chords.availability("pane.copy").is_available(),
            "antes de congelar el resolver no sabe nada del contexto"
        );
        app.freeze_help_facts();
        assert_eq!(
            app.help_chords.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend),
            "los dos panes son de solo lectura: copiar no tiene destino"
        );
    }

    /// Una notif `connection.degraded` como la del wire (#44).
    fn degradacion_de_test(scheme: &str, host: &str) -> norte_proto::methods::ConnectionDegraded {
        norte_proto::methods::ConnectionDegraded {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            reason: "ftp-plaintext".to_owned(),
            detail: None,
        }
    }

    /// #44 guardaba la degradación como PROSA ya formateada: el scheme y el
    /// host se metían en el mensaje y se tiraban, así que «¿qué conexión se
    /// degradó?» no tenía respuesta. H3d la necesita por pane.
    #[test]
    fn la_degradacion_se_guarda_por_scheme() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ejemplo.org"));
        let d = app.degraded_for("sftp").expect("la degradación se retuvo");
        assert_eq!(d.host, "ejemplo.org", "el host sobrevive, no solo la frase");
        assert_eq!(d.reason, "ftp-plaintext");
        assert!(app.degraded_for("file").is_none());
    }

    /// Y dos conexiones degradadas no se pisan: antes la última ganaba y la
    /// primera desaparecía de la barra sin que nada la hubiera resuelto.
    #[test]
    fn dos_degradaciones_conviven() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.note_degraded(degradacion_de_test("ftp", "b.org"));
        assert!(app.degraded_for("sftp").is_some());
        assert!(app.degraded_for("ftp").is_some());
        // Y la barra deja de mentir sobre cuántas hay. Nombra la ÚLTIMA y dice
        // cuántas más: un recuento pelado («2 conexiones en texto plano»), con
        // el aviso que jamás se limpia, dejaba al lector sin poder averiguar
        // NUNCA cuáles eran — y esa es la única pregunta que este indicador
        // existe para contestar.
        let banner = app.connection_banner().expect("hay aviso");
        assert!(
            banner.contains("b.org"),
            "la más reciente se nombra: {banner}"
        );
        assert!(banner.contains('1'), "y cuántas más hay: {banner}");
    }

    /// #177: «esta sesión no queda registrada» tiene que sobrevivir a la
    /// siguiente tecla. Llega UNA vez, en mitad de una operación que el usuario
    /// acaba de lanzar, y `app.message` lo borra la pulsación siguiente — que
    /// es como decir que no se avisó.
    #[test]
    fn la_sesion_sin_journal_tiene_indicador_persistente() {
        let mut app = app_dos_panes();
        assert!(app.journal_banner().is_none(), "por defecto sí se registra");

        app.message = Some("algo".to_owned());
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        // Lo que borra el `message` en el run loop, tecla a tecla.
        app.message = None;
        assert!(
            app.journal_banner().is_some(),
            "el indicador no se va con el mensaje"
        );
    }

    /// Y no compite con el de #44: los dos son persistentes, de la misma clase
    /// y simultáneos, así que elegir uno escondería el otro para el resto de la
    /// sesión.
    #[test]
    fn los_dos_indicadores_persistentes_caben_juntos() {
        let mut app = app_dos_panes();
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        let banner = app.persistent_banner().expect("hay aviso");
        assert!(
            banner.contains("a.org"),
            "la conexión sigue nombrada: {banner}"
        );
        assert!(
            banner.starts_with(&app.journal_banner().expect("hay journal_banner")),
            "y el del journal va primero: {banner}"
        );
    }

    /// MINOR-5: el `Option<String>` de #44 estaba acotado por construcción;
    /// una colección con clave que viene del WIRE no lo está. El tope es
    /// generoso —hay siete schemes— así que solo lo alcanza algo anómalo, y
    /// cuando pasa se tira lo más viejo y se conserva lo que acaba de llegar.
    #[test]
    fn las_degradaciones_tienen_tope() {
        let mut app = app_dos_panes();
        for i in 0..(super::DEGRADED_MAX + 10) {
            app.note_degraded(degradacion_de_test(&format!("s{i}"), "host"));
        }
        assert_eq!(app.degraded.len(), super::DEGRADED_MAX);
        assert!(
            app.degraded_for("s0").is_none(),
            "la más vieja es la que se cae"
        );
        assert!(
            app.degraded_for(&format!("s{}", super::DEGRADED_MAX + 9))
                .is_some(),
            "la última en llegar se queda"
        );
    }

    /// El host lo elige el OTRO extremo, y la barra de estado es el sitio
    /// donde llegaba crudo mientras el resto de la TUI enmascara. Un host con
    /// controles o bidi es exactamente lo que se le manda a un indicador de
    /// seguridad para que mienta.
    #[test]
    fn el_aviso_enmascara_un_host_hostil() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ma\u{202e}gro.org\n"));
        let banner = app.connection_banner().expect("hay aviso");
        assert!(
            !banner.contains('\u{202e}') && !banner.contains('\n'),
            "el host llegó crudo a la barra: {banner:?}"
        );
        assert!(
            banner.contains('\u{FFFD}'),
            "y el enmascarado se VE (jamás pérdida silenciosa): {banner:?}"
        );
    }

    /// Sin degradación no hay aviso, y con UNA el aviso es el de siempre
    /// (#44): scheme y host, formateados desde el valor estructurado.
    #[test]
    fn el_aviso_de_una_sola_degradacion_nombra_la_conexion() {
        let mut app = app_dos_panes();
        assert!(app.connection_banner().is_none());
        app.note_degraded(degradacion_de_test("sftp", "remoto.example"));
        let banner = app.connection_banner().expect("hay aviso");
        assert!(banner.contains("sftp"), "{banner}");
        assert!(banner.contains("remoto.example"), "{banner}");
    }

    /// `App` con cada pane sobre SU dir (el `app_dos_panes` de arriba pone
    /// los dos sobre `root()`, que no distingue lados).
    fn app_en(izq: &str, der: &str) -> App {
        App::new(
            Pane::new(vp(izq), Vec::new()),
            Pane::new(vp(der), Vec::new()),
        )
    }

    /// El intercambio cruza el pane Y su historial, y deja el foco en el
    /// mismo LADO: quien miraba a la izquierda sigue mirando a la izquierda,
    /// y ahora ahí está lo que había a la derecha.
    #[test]
    fn el_intercambio_cruza_pane_e_historial_y_no_mueve_el_foco() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.history[0].record(vp("mem:///rastro-izq"));
        app.history[1].record(vp("mem:///rastro-der"));
        app.set_focus(0);

        app.swap_panes();

        assert_eq!(app.panes[0].dir(), &vp("mem:///der"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///izq"));
        assert_eq!(app.focus(), 0, "el foco se queda en su lado");
        // El rastro viaja con el CONTENIDO, no con el lado: si no, el popup
        // ofrecería llevar «atrás» a sitios donde ese contenido nunca estuvo.
        assert_eq!(
            app.history[0].entries().front(),
            Some(&vp("mem:///rastro-der"))
        );
        assert_eq!(
            app.history[1].entries().front(),
            Some(&vp("mem:///rastro-izq"))
        );
        // Y el RASTRO de atrás/adelante viaja también, no solo la MRU que
        // pinta el popup: son dos estructuras dentro del mismo `History`.
        assert_eq!(app.history[0].back_len(), 1);
        assert_eq!(
            app.history[0].step_back(vp("mem:///der")),
            Some(vp("mem:///rastro-der")),
            "el atrás del pane 0 apunta al rastro que llegó con su contenido"
        );
    }

    /// Dos intercambios son la identidad.
    #[test]
    fn dos_intercambios_dejan_todo_como_estaba() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.swap_panes();
        app.swap_panes();
        assert_eq!(app.panes[0].dir(), &vp("mem:///izq"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///der"));
    }

    /// El foco se queda en el LADO también cuando estaba a la derecha: el
    /// intercambio no toca `focus` en absoluto. (Mutación de control:
    /// añadir `self.focus ^= 1` a `swap_panes` rompe aquí y en el test de
    /// arriba a la vez.)
    #[test]
    fn el_intercambio_con_el_foco_a_la_derecha_tampoco_lo_mueve() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.set_focus(1);
        app.swap_panes();
        assert_eq!(app.focus(), 1);
        assert_eq!(
            app.focused().dir(),
            &vp("mem:///izq"),
            "en el lado derecho ahora está lo que había a la izquierda"
        );
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

    /// encoding-auditor MAJOR: `fs_type` looked like a closed, ASCII-only
    /// vocabulary (`ext4`, `nfs4`…) but a FUSE mount's `fuse.<subtype>` is
    /// the `-o subtype=` value an UNPRIVILEGED user picks (`sshfs`, `rclone
    /// mount`…) — exactly as untrusted as a filename. An earlier draft of
    /// `volume_item_display` spliced it in with `{}` and skipped
    /// `display_name` entirely, so a hostile `fs_type` reached the row raw.
    #[test]
    fn volume_row_sanea_fs_type_hostil() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: None,
            fs_type: "fuse.evil\u{202E}type".to_owned(),
            kind: norte_proto::methods::VolumeKind::Fixed,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// V3.5 (encoding-auditor MAJOR deferred from V3): `label` is
    /// `Option<Vec<u8>>` end to end now, so a non-UTF-8 label reaches this
    /// row as the ORIGINAL bytes — not a lossy `String` some earlier layer
    /// already mangled — and goes through the exact same masking `fs_type`
    /// gets above. Bytes `\xFF\xFE` are not valid UTF-8 in any position, so
    /// `display_name` must fall back to lossy rendering AND mark it hostile.
    #[test]
    fn volume_row_sanea_label_no_utf8() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(vec![0xFF, 0xFE, b'X']),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(display.starts_with('!'), "badge prefijo: {display}");
        assert!(
            display.contains('\u{FFFD}'),
            "el label no-UTF8 se pinta lossy: {display}"
        );
        assert_eq!(
            items[0].target,
            Some(vp("mem:///media/usb")),
            "el target sigue siendo el mount real, ajeno al label"
        );
    }

    /// V3.5 (encoding-auditor MINOR: the hand-picked byte string above is
    /// not the canonical corpus): every hostile name in
    /// `norte_testkit::corpus::hostile_names()`, used as a LABEL, must reach
    /// the row without panicking, badged EXACTLY when `display_name` alone
    /// says that name comes out altered — the same function
    /// `volume_item_display` calls, so this pins agreement rather than
    /// reimplementing the masking rule a second time. `target` stays the
    /// clean mount throughout: a hostile label must never leak into
    /// Enter-to-navigate.
    ///
    /// #169's `archive_marker_literal` (a label whose own CLEAN text is
    /// `"!"`, the same glyph as [`crate::ui::HOSTILE_BADGE`]) caught this
    /// assertion checking `display.starts_with('!')` — true for that
    /// fixture even with `label_hostil == false`, because the UN-badged
    /// label prefix (`"{label} — "`) itself starts with `!`. A leading `!`
    /// is not proof of a badge, and even `"! "` is not enough: that fixture's
    /// clean prefix is `"! — "`, which also starts with `"! "`. Nothing
    /// short of the FULL string settles it, so the expected display is
    /// rebuilt here from the same primitives `volume_item_display` calls
    /// (`display_name`, `path_display_with`, `t`) — not the masking rule
    /// itself, only the template it is spliced into — and compared for
    /// EXACT equality.
    #[test]
    fn volume_label_hostile_corpus_sweep() {
        let mount = vp("mem:///media/usb");
        let (path_text, path_hostil) = norte_frontend::path_display_with(&mount, None);
        assert!(!path_hostil, "control: el mount fijo del test no es hostil");
        let (fs_text, fs_hostil) = display_name(b"vfat");
        assert!(!fs_hostil, "control: \"vfat\" no es hostil");
        let sizes = format!("{u} / {u}", u = t("volumes-size-unknown"));
        for fixture in norte_testkit::corpus::hostile_names() {
            let vol = norte_proto::methods::Volume {
                mount: mount.clone(),
                label: Some(fixture.bytes.clone()),
                fs_type: "vfat".to_owned(),
                kind: norte_proto::methods::VolumeKind::Removable,
                total_bytes: None,
                free_bytes: None,
                read_only: false,
            };
            let items = volume_items(std::slice::from_ref(&vol), None);
            let display = &items[0].display;
            let (label_text, label_hostil) = display_name(&fixture.bytes);
            let body = format!("{label_text} — {path_text}  {fs_text}  {sizes}");
            let expected = if label_hostil {
                format!("{} {body}", crate::ui::HOSTILE_BADGE)
            } else {
                body
            };
            assert_eq!(
                display, &expected,
                "{}: badge debe coincidir con display_name({:?})",
                fixture.id, fixture.bytes
            );
            assert_eq!(
                items[0].target,
                Some(mount.clone()),
                "{}: el target sigue siendo el mount, ajeno al label",
                fixture.id
            );
        }
    }

    /// The everyday case: an ordinary `fs_type` and absent sizes (the
    /// filesystem never answered `statvfs` in time, design §A) render with
    /// NO badge and say `volumes-size-unknown` rather than a bare zero — a
    /// zero here would read as "full", the opposite of "unknown".
    #[test]
    fn volume_row_talla_ausente_no_es_cero() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(b"USB".to_vec()),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.starts_with('!'), "nada hostil aquí: {display}");
        assert!(!display.contains('0'), "ausente no es cero: {display}");
        assert_eq!(items[0].target, Some(vp("mem:///media/usb")));
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
            pane: 0,
            trail: Trail::Record,
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

    /// #105: F5 de UN ítem abre el nombre editable prefijado con el nombre
    /// ORIGINAL. Sin tocar, el confirm usa los BYTES crudos (regla 1: un
    /// nombre no-UTF8 copiado sin editar jamás pasa por el lossy).
    #[test]
    fn transfer_name_sin_editar_conserva_los_bytes_originales() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"informe\xFF\xFE.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (kind, from, dest) = app.transfer_name_confirm().expect("válido");
        assert_eq!(kind, TransferKind::Copy);
        assert_eq!(from, hostile);
        assert_eq!(
            dest,
            VPath::parse("mem:///dst")
                .unwrap()
                .join(norte_proto::Segment::new(b"informe\xFF\xFE.dat".to_vec()).unwrap()),
            "bytes crudos al destino, jamás la forma lossy"
        );
    }

    /// #105: editar sustituye el nombre por el TEXTO tecleado; y un texto
    /// que aún contiene U+FFFD (residuo del prefill lossy de un nombre
    /// hostil) se RECHAZA — confirmarlo escribiría mojibake en disco.
    #[test]
    fn transfer_name_editado_usa_el_texto_y_rechaza_fffd() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"x\xFF.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile,
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        // Tocar el campo (borra el último char del prefill lossy): el texto
        // sigue llevando el U+FFFD del prefill → rechazo con diagnóstico.
        app.transfer_name_pop();
        assert!(app.transfer_name_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        // Reescrito limpio: vale, y son los bytes del texto.
        while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
            app.transfer_name_pop();
        }
        for c in "limpio.dat".chars() {
            app.transfer_name_push(c);
        }
        let (_, _, dest) = app.transfer_name_confirm().expect("limpio");
        assert_eq!(dest, VPath::parse("mem:///dst/limpio.dat").unwrap());
    }

    /// #105: shift+F6 — rename in situ: destino = MISMO dir; confirmar sin
    /// cambiar el nombre es error (no-op), y un nombre nuevo construye el
    /// destino en el propio dir.
    #[test]
    fn rename_construye_en_el_mismo_dir_y_rechaza_el_mismo_nombre() {
        let mut app = app_with_entries(&["a.txt"]);
        app.open_rename();
        assert!(
            app.transfer_name_confirm().is_none(),
            "mismo nombre = no-op, jamás un submit"
        );
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        app.transfer_name_push('2'); // "a.txt2"
        let (kind, from, dest) = app.transfer_name_confirm().expect("nombre nuevo");
        assert_eq!(kind, TransferKind::Move);
        assert_eq!(from, VPath::parse("mem:///a.txt").unwrap());
        assert_eq!(dest, VPath::parse("mem:///a.txt2").unwrap());
    }

    /// #108 b4: `apply_scheme_sort` aplica el orden de la config al pane
    /// según su scheme — el hook de cd y el arranque pasan por aquí.
    #[test]
    fn apply_scheme_sort_ordena_por_la_config() {
        use norte_frontend::columns::ColumnsSettings;
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str, size: Option<u64>| {
            let mut e = e(&format!("mem:///{n}"), EntryKind::File);
            e.size = size;
            e
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a", Some(3)), mk("b", Some(1))]),
            Pane::new(dir, Vec::new()),
        );
        let cfg = norte_config::ColumnsConfig {
            default_columns: None,
            sort: Some(norte_config::SortChoice {
                column: norte_config::SortColumnKey::Size,
                descending: false,
                dirs_first: true,
            }),
            schemes: std::collections::BTreeMap::new(),
            ..Default::default()
        };
        app.columns = ColumnsSettings::resolve(&cfg);
        app.apply_scheme_sort(0);
        let orden: Vec<_> = app.panes[0]
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            orden,
            vec![
                VPath::parse("mem:///b").unwrap(),
                VPath::parse("mem:///a").unwrap()
            ],
            "size asc desde la config"
        );
    }

    /// #105 review MAJOR-1: el submit de UN ítem que vino de la MARCA la
    /// CONSUME (doctrina mc/TC del lote); un rename (cursor) jamás toca
    /// las marcas, y Esc tampoco.
    #[test]
    fn el_submit_de_un_item_consume_la_marca_y_el_rename_no() {
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(norte_proto::Segment::new(n.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a"), mk("b")]),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.focused_mut().toggle_mark(); // marca "a"
        app.focused_mut().move_down(1); // cursor en "b"
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (_, from, _) = app.transfer_name_confirm().expect("válido");
        assert_eq!(
            from,
            VPath::parse("mem:///a").unwrap(),
            "la MARCA, no el cursor"
        );
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 0, "el envío consume la marca");

        // Esc no consume.
        app.focused_mut().toggle_mark(); // marca "b" (cursor sigue ahí)
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        app.cancel_transfer_name();
        assert_eq!(app.focused().marks_len(), 1, "cancelar conserva la marca");

        // Rename (cursor) no toca marcas ajenas.
        app.open_rename();
        app.transfer_name_push('2');
        assert!(app.transfer_name_confirm().is_some());
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 1, "el rename no consume marcas");
    }

    /// #105 (regla 1, corpus canónico): renombrar un nombre hostil a uno
    /// limpio conserva el `from` BYTE-EXACTO para cada nombre del corpus —
    /// el origen jamás pasa por texto, solo el nombre nuevo es tecleado.
    #[test]
    fn rename_de_cada_nombre_hostil_del_corpus_conserva_el_from() {
        let dir = VPath::parse("mem:///").unwrap();
        for (i, hostile) in norte_testkit::corpus::hostile_names().iter().enumerate() {
            let from = dir
                .clone()
                .join(norte_proto::Segment::new(hostile.bytes.clone()).unwrap());
            let mut app = App::new(
                Pane::new(
                    dir.clone(),
                    vec![Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: from.clone(),
                        kind: EntryKind::File,
                        size: None,
                        mtime_ms: None,
                    }],
                ),
                Pane::new(dir.clone(), Vec::new()),
            );
            app.open_rename();
            while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
                app.transfer_name_pop();
            }
            for c in "limpio".chars() {
                app.transfer_name_push(c);
            }
            let (_, got_from, dest) = app
                .transfer_name_confirm()
                .unwrap_or_else(|| panic!("corpus[{i}] {}", hostile.id));
            assert_eq!(got_from, from, "corpus[{i}]: from byte-exacto");
            assert_eq!(dest, VPath::parse("mem:///limpio").unwrap());
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
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::ConfirmTransfer { items, to, .. }) = &app.modal else {
            panic!("no transfer modal");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(to, &VPath::parse("mem:///dst").unwrap());
    }

    /// Sin ninguna marca, F5 sigue operando sobre el CURSOR (el gesto
    /// clásico no se pierde) — `marked_paths` cae al seleccionado. Con UN
    /// solo ítem la puerta abre el nombre EDITABLE (#105), no el confirm de
    /// lista: es la misma decisión para la tecla y para un drop.
    #[test]
    fn copy_without_marks_still_uses_the_cursor_entry() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        assert_eq!(app.focused().marks_len(), 0, "sin marcas de partida");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::TransferName {
            from,
            to_dir,
            from_marks,
            ..
        }) = &app.modal
        else {
            panic!("no transfer modal");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///a").unwrap(),
            "marked_paths falls back to the cursor"
        );
        assert_eq!(to_dir, &VPath::parse("mem:///dst").unwrap());
        assert!(!from_marks, "no había marca que consumir");
    }

    /// Un arrastre PROMOVIDO lleva la fila del press y NADA más: ni las
    /// marcas del pane (que son otra cosa que el usuario no ha soltado) ni
    /// su consumo al enviar. La promoción cambia lo que el gesto HACE, no lo
    /// que está seleccionado.
    #[test]
    fn a_promoted_transfer_carries_one_row_and_does_not_consume_the_marks() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3);
        app.open_transfer(TransferKind::Copy, 0, 1, Some(2));
        let Some(Modal::TransferName {
            from, from_marks, ..
        }) = &app.modal
        else {
            panic!("un solo ítem: nombre editable");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///c").unwrap(),
            "la fila promovida, no las tres marcas"
        );
        assert!(!from_marks, "el envío NO puede consumir las marcas");
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 3, "las marcas siguen ahí");
    }

    /// Un índice promovido que ya no nombra ninguna fila (el listado encogió
    /// entre el gesto y el drop) no abre nada: jamás un diálogo sobre un
    /// lote vacío, y jamás cayendo hacia las marcas —que sería copiar lo que
    /// nadie arrastró—.
    #[test]
    fn a_promoted_index_out_of_range_opens_nothing() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_transfer(TransferKind::Copy, 0, 1, Some(9));
        assert!(app.modal.is_none());
    }

    /// Las marcas las CONSUME el ENVÍO del lote (mc/Total Commander): tras
    /// `consume_marks` no queda una selección a medio consumir.
    #[test]
    fn submitting_a_bulk_operation_consumes_the_marks() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 2, "marcadas antes de enviar");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
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
        app.open_transfer(TransferKind::Copy, 0, 1, None);
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
                paths_total: 0,
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
        // 0.36.0: los dos brazos del batch de renames viven sobre un
        // `_ => "err-unknown"`, así que borrarlos COMPILA y la suite seguiría
        // verde — con dos errores que su rustdoc llama accionables cayendo en
        // «error desconocido», que es lo contrario. Estos asserts son lo único
        // que lo impide.
        assert_eq!(error_key(&Error::PlanStale), "err-plan-stale");
        assert_eq!(
            error_key(&Error::PlanNotExecutable),
            "err-plan-not-executable"
        );
        // La categoría es exactamente t(clave).
        assert_eq!(
            error_category(&Error::NotFound),
            norte_i18n::t("err-not-found")
        );
    }

    /// 0.40.0: las tres relaciones de solape se pintan DISTINTO, y ninguna
    /// cae en «error desconocido».
    ///
    /// Vive sobre el mismo `_ => "err-unknown"` que los dos de arriba, así que
    /// borrar el brazo compila y deja al lector con «error desconocido» ante la
    /// única negativa de esta familia que se arregla moviéndose de sitio — que
    /// es exactamente lo que la variante existe para no ser. Y las tres claves
    /// tienen que ser tres: la frase accionable de `Same` («elige otro
    /// directorio») no es la de las otras dos («sal del árbol que contiene al
    /// otro»).
    #[test]
    fn cada_relacion_de_solape_tiene_su_propia_frase() {
        use super::{error_category, error_key};
        use norte_proto::RootOverlap;
        let mut vistas = std::collections::BTreeSet::new();
        for relation in [
            RootOverlap::Same,
            RootOverlap::SourceInsideDest,
            RootOverlap::DestInsideSource,
        ] {
            let clave = error_key(&Error::OverlappingRoots { relation });
            assert!(
                clave.starts_with("err-overlapping-roots"),
                "{relation:?} → {clave}"
            );
            assert!(vistas.insert(clave), "dos relaciones comparten {clave}");
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let texto = norte_i18n::t_in(lang, clave);
                assert_ne!(texto, clave, "{clave} sin traducir en {lang:?}");
                assert_ne!(
                    texto,
                    norte_i18n::t_in(lang, "err-unknown"),
                    "{clave} dice lo mismo que «error desconocido»"
                );
                assert_ne!(
                    texto,
                    norte_i18n::t_in(lang, "err-internal"),
                    "{clave} dice lo mismo que «error interno»"
                );
            }
        }
        // Y una relación de un protocolo más nuevo cae en la clave GENÉRICA de
        // la familia, no en `err-unknown`: sigue siendo un solape.
        assert_eq!(
            error_key(&Error::OverlappingRoots {
                relation: RootOverlap::Unknown
            }),
            "err-overlapping-roots"
        );
        assert_eq!(
            error_category(&Error::OverlappingRoots {
                relation: RootOverlap::Same
            }),
            norte_i18n::t("err-overlapping-roots-same")
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

#[cfg(test)]
mod help_view_tests {
    use super::{ALLOW_HELP, HelpOutcome, HelpView, help_action};
    use crate::keymap::DIALOG_COMMANDS;
    use norte_frontend::help::{Focus, KEYS_ID};
    use norte_help::{Lang, TopicId};

    /// The default resolver plus the shipped theme: deterministic, and the
    /// same pair `App` starts with.
    fn refresh(view: &mut HelpView, width: usize, height: usize) {
        let chords = super::default_help_chords();
        let theme = crate::theme::TuiTheme::new(
            norte_theme::Theme::preset_default(),
            norte_theme::ColorDepth::Truecolor,
        );
        view.refresh(&chords, width, height, &theme);
    }

    fn flatten(lines: &[ratatui::text::Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_new_view_opens_on_the_index_with_an_empty_body() {
        let view = HelpView::new(Lang::En, Vec::new());
        assert_eq!(view.state.current().as_str(), "index");
        assert!(!view.on_keys_page(), "the index IS a corpus topic");
        let (lines, actions) = view.body();
        assert!(
            lines.is_empty() && actions.is_empty(),
            "nothing is laid out until `refresh` knows how wide the terminal is"
        );
    }

    /// The allowlist and the dispatcher are ONE list: a verb the footer hint
    /// advertises (`DialogHints::help`, generated from `ALLOW_HELP`) and that
    /// dispatch drops is a hint that lies. Swept over the WHOLE `dialog`
    /// vocabulary, so a verb added to `ALLOW_HELP` without an arm — or an arm
    /// added without the allowlist — fails here.
    #[test]
    fn help_action_accepts_exactly_the_allowlist() {
        for cmd in ALLOW_HELP {
            assert!(
                help_action(cmd).is_some(),
                "{cmd} is allowed but dispatches to nothing"
            );
        }
        let mut outside = 0_usize;
        for cmd in DIALOG_COMMANDS {
            if ALLOW_HELP.contains(cmd) {
                continue;
            }
            outside += 1;
            assert_eq!(
                help_action(cmd),
                None,
                "{cmd} is outside `ALLOW_HELP`: the key must be INERT"
            );
        }
        assert!(
            outside > 5,
            "the sweep must actually cover verbs the overlay refuses: {outside}"
        );
        assert_eq!(help_action("pane.copy"), None, "not even a `dialog.*` verb");
        // Named samples, so a regression says WHICH arm was transposed.
        assert_eq!(help_action("dialog.confirm"), Some(HelpOutcome::Activate));
        assert_eq!(help_action("dialog.cancel"), Some(HelpOutcome::Close));
        assert_eq!(help_action("dialog.pane"), Some(HelpOutcome::TogglePane));
        assert_eq!(help_action("dialog.back"), Some(HelpOutcome::Back));
        assert_eq!(help_action("dialog.filter"), Some(HelpOutcome::StartFilter));
    }

    /// Why the body is pre-rendered at all: `page_down` deliberately does not
    /// bound itself (the model cannot know how many lines the prose wrapped
    /// into), so without this clamp a reader who pages past the end gets a
    /// PERMANENTLY blank body — no key scrolls back into a body that is not
    /// there.
    #[test]
    fn refresh_clamps_a_scroll_paged_past_the_end() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        view.state.toggle_focus();
        assert_eq!(view.state.focus(), Focus::Body, "`copying` has actions");
        view.state.page_down(1_000_000);
        refresh(&mut view, 60, 10);
        let (lines, _) = view.body();
        assert!(!lines.is_empty(), "the topic laid out");
        assert!(
            view.state.body_scroll() < lines.len(),
            "scroll {} outside a body of {} lines: the page is blank",
            view.state.body_scroll(),
            lines.len()
        );
    }

    /// The other half of the same contract: with the focus on the body, the
    /// action the cursor is on has to be ON SCREEN — the cursor walks
    /// ACTIONS and the body scrolls in LINES, and only the renderer can
    /// translate one into the other.
    #[test]
    fn refresh_reveals_the_focused_action() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.state.open(&TopicId::new("copying"));
        view.state.toggle_focus();
        assert_eq!(view.state.focus(), Focus::Body);
        for _ in 0..50 {
            view.state.down();
        }
        let height = 6;
        refresh(&mut view, 60, height);
        let (_, action_lines) = view.body();
        let line = action_lines[view.state.action_cursor()];
        let first = view.state.body_scroll();
        assert!(
            (first..first + height).contains(&line),
            "action line {line} outside the window [{first}, {}): the reader \
             cannot see what Enter would run",
            first + height
        );
    }

    #[test]
    fn the_keys_page_paints_keys_lines_and_maps_no_action() {
        let mut view = HelpView::new(
            Lang::En,
            vec![
                ratatui::text::Line::raw("── Browsing ──"),
                ratatui::text::Line::raw("  f5   copy"),
            ],
        );
        view.state.open(&TopicId::new(KEYS_ID));
        assert!(view.on_keys_page());
        refresh(&mut view, 60, 10);
        let (lines, action_lines) = view.body();
        assert_eq!(lines.len(), 2, "one painted line per generated line");
        assert!(flatten(lines).contains("f5   copy"), "{:?}", flatten(lines));
        assert!(
            action_lines.is_empty(),
            "its rows are chords, and a chord is not something Enter runs"
        );

        // And going back to a corpus topic restores a real action map: the
        // empty one above is the KEYS page, not a renderer that lost it.
        view.state.open(&TopicId::new("copying"));
        refresh(&mut view, 60, 10);
        assert!(!view.on_keys_page());
        assert!(!view.body().1.is_empty());
    }

    /// Un plugin del catálogo, con la forma que llega por el wire.
    fn plugin(id: &str, name: &str) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            publisher: "ACME".to_owned(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            has_help: true,
        }
    }

    /// H3e: la página de un plugin que aún NO ha llegado se pinta VACÍA, jamás
    /// como la página de teclado. Las dos son «no hay tema del corpus» para
    /// `HelpState::topic`, y sin la distinción el chuletario entero aparecería
    /// bajo el nombre de una extensión, leyéndose como su documentación.
    #[test]
    fn una_pagina_de_plugin_en_vuelo_sale_vacia_y_no_es_el_teclado() {
        let mut view = HelpView::new(Lang::En, vec![ratatui::text::Line::raw("  f5   copy")]);
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        view.state.open(&TopicId::new("acme.ftp"));
        assert!(
            !view.on_keys_page(),
            "una página de plugin no es la de teclado"
        );
        refresh(&mut view, 60, 10);
        let (lines, action_lines) = view.body();
        assert!(lines.is_empty(), "cuerpo vacío mientras llega: {lines:?}");
        assert!(action_lines.is_empty());

        // Y cuando llega, se pinta — con su insignia de procedencia.
        let parsed = norte_help::parse_untrusted(
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\n+++\ncuerpo del plugin",
            "acme.ftp",
            view.publisher_of("acme.ftp"),
        )
        .fold_flags(true, false);
        view.state.install_plugin_topic(parsed.topic);
        refresh(&mut view, 60, 10);
        let pintado = flatten(view.body().0);
        assert!(pintado.contains("cuerpo del plugin"), "{pintado}");
        assert!(
            pintado.contains("ACME"),
            "el publicador acompaña: {pintado}"
        );
        assert!(
            pintado.contains(&norte_i18n::t_in(Lang::En, "help-plugin-truncated")),
            "y la insignia de recorte: {pintado}"
        );
    }

    /// El texto de terceros se enmascara y se acota en el PUNTO DE ENTRADA:
    /// `PluginNode::title` promete llegar seguro y el modelo no enmascara nada
    /// — filtra sobre lo que le den.
    #[test]
    fn el_nombre_de_un_plugin_entra_enmascarado_y_acotado() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        let mut p = plugin("acme.ftp", &format!("a\u{202E}{}", "x".repeat(5_000)));
        p.publisher = "AC\u{202E}ME".to_owned();
        view.set_plugins(&[p]);
        let fila = view
            .state
            .rows()
            .iter()
            .find_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, title }
                    if id.as_str() == "acme.ftp" =>
                {
                    Some(title.clone())
                }
                _ => None,
            })
            .expect("el nodo está en la barra");
        assert!(!fila.contains('\u{202E}'), "sin bidi crudo: {fila:?}");
        assert!(
            fila.chars().count() <= super::PLUGIN_NAME_WIRE_CAP + 1,
            "acotado: {} chars",
            fila.chars().count()
        );
        assert!(
            fila.ends_with('…'),
            "y el recorte se MARCA, como lo marcan los vecinos que hacen esto \
             mismo: presentar un nombre cortado como completo es la mentira \
             que la fase fue a perseguir: {fila:?}"
        );
        let pub_ = view.publisher_of("acme.ftp").expect("hay publicador");
        assert!(!pub_.contains('\u{202E}'), "publicador limpio: {pub_:?}");
    }

    /// H3e: un `name` en BLANCO cae al id del plugin.
    ///
    /// `name` es obligatorio en el manifiesto pero nadie comprueba que tenga
    /// contenido, y U+3164 (HANGUL FILLER) no es espacio en blanco: sobrevive
    /// al `trim` y al enmascarado. Sin el repliegue, la barra pinta una fila
    /// VACÍA bajo la cabecera «Extensiones» — una página que el lector puede
    /// pisar, abrir y leer, colgando de un nombre que no dice nada.
    #[test]
    fn un_nombre_en_blanco_cae_al_id_del_plugin() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        let mut p = plugin("acme.ftp", "\u{3164}\u{3164}");
        p.publisher = "\u{3164}".to_owned();
        view.set_plugins(&[p]);
        let fila = view
            .state
            .rows()
            .iter()
            .find_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, title }
                    if id.as_str() == "acme.ftp" =>
                {
                    Some(title.clone())
                }
                _ => None,
            })
            .expect("el nodo está en la barra");
        assert_eq!(fila, "acme.ftp", "la fila se nombra con el id: {fila:?}");

        // Y un publicador en blanco no se atribuye: la insignia pintaría
        // «publicada por » sin nada detrás, que se lee como un fallo del
        // pintor y no como una ausencia.
        assert_eq!(view.publisher_of("acme.ftp"), None);

        // Anti-vacuidad: un nombre REAL no se toca.
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        assert!(view.state.rows().iter().any(|r| matches!(
            r,
            norte_frontend::help::SidebarRow::Topic { title, .. } if title == "FTP"
        )));
        assert_eq!(view.publisher_of("acme.ftp").as_deref(), Some("ACME"));
    }

    /// H3e: un id que NO es un id de plugin válido se DESCARTA en el punto de
    /// entrada — nunca se repara.
    ///
    /// El id llega por el wire y es una CLAVE: viaja a `TopicId`, a
    /// `plugin_needs_fetch` y de vuelta como argumento de `plugin.help`.
    /// Enmascararlo no sería una medida de seguridad (el enmascarado no es
    /// inyectivo: dos plugins distintos caerían en la misma fila) y un id
    /// «reparado» sería una clave que no resuelve a nada, o peor, a otra cosa.
    /// Negarse es la única respuesta que no puede mentir. Mismo criterio que
    /// las claves de comando de `parse_untrusted`, que se rechazan en vez de
    /// reescribirse.
    #[test]
    fn un_id_que_no_es_de_plugin_se_descarta_en_la_entrada() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        let mut bidi = plugin("acme.\u{202E}ftp", "Bidi");
        bidi.publisher = "ACME".to_owned();
        view.set_plugins(&[
            plugin("acme.ftp", "Bueno"),
            bidi,
            plugin("sinpunto", "Sin punto"),
            plugin(&"a.".repeat(500), "Kilométrico"),
        ]);
        let ids: Vec<String> = view
            .state
            .rows()
            .iter()
            .filter_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, .. } => Some(id.as_str().to_owned()),
                norte_frontend::help::SidebarRow::Group { .. } => None,
            })
            // La barra lleva TODO el corpus además de las extensiones: lo que
            // se mira aquí son las filas de plugin, que son las que este
            // filtro decide.
            .filter(|id| {
                id != norte_frontend::help::KEYS_ID && norte_help::topic(Lang::En, id).is_none()
            })
            .collect();
        assert_eq!(
            ids,
            vec!["acme.ftp".to_owned()],
            "solo sobrevive el id válido: {ids:?}"
        );
        // Y no se queda una atribución colgando del que se fue.
        assert_eq!(view.publisher_of("acme.\u{202E}ftp"), None);
    }

    /// Un nombre de invisibles que son HAZARDS (no `INVISIBLE`) también cuenta
    /// como blanco — porque se pregunta DESPUÉS de enmascarar, cuando ya son
    /// `U+FFFD`. Es el orden lo que hace que una sola pregunta cubra las dos
    /// familias.
    #[test]
    fn un_nombre_de_espacios_de_ancho_cero_tambien_cae_al_id() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "\u{200B}\u{200B}")]);
        assert!(view.state.rows().iter().any(|r| matches!(
            r,
            norte_frontend::help::SidebarRow::Topic { title, .. } if title == "acme.ftp"
        )));
    }

    /// `plugin_needs_fetch` PREGUNTA, no avisa: sigue contestando `Some` hasta
    /// que la página se instala, y el run loop lo visita en cada vuelta. Sin la
    /// reclamación, un daemon que no contesta se reintentaría a ritmo de frame.
    #[test]
    fn la_pagina_se_pide_una_sola_vez_por_overlay() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        view.state.open(&TopicId::new("acme.ftp"));
        assert_eq!(view.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
        for _ in 0..100 {
            assert_eq!(
                view.claim_plugin_fetch(),
                None,
                "un fallo no se reintenta dentro del mismo overlay"
            );
            assert_eq!(
                view.state.plugin_needs_fetch(),
                Some("acme.ftp"),
                "y el modelo sigue diciendo que falta: es la reclamación la que \
                 corta el bucle, no el modelo"
            );
        }
        // Cerrar y reabrir la ayuda SÍ vuelve a pedir: es el único reintento
        // que el lector tiene, y el único que puede pedir.
        let mut otra = HelpView::new(Lang::En, Vec::new());
        otra.set_plugins(&[plugin("acme.ftp", "FTP")]);
        otra.state.open(&TopicId::new("acme.ftp"));
        assert_eq!(otra.claim_plugin_fetch().as_deref(), Some("acme.ftp"));
    }

    /// Una página del corpus no pide nada, y la de teclado tampoco: pedir por
    /// ellas sería una llamada al daemon por frame durante toda la lectura.
    #[test]
    fn una_pagina_del_corpus_no_pide_nada() {
        let mut view = HelpView::new(Lang::En, Vec::new());
        view.set_plugins(&[plugin("acme.ftp", "FTP")]);
        assert_eq!(view.claim_plugin_fetch(), None, "el índice no pide nada");
        view.state.open(&TopicId::new(KEYS_ID));
        assert_eq!(view.claim_plugin_fetch(), None, "el teclado tampoco");
    }
}

#[cfg(test)]
mod help_plugin_snapshot_tests {
    use super::App;
    use norte_help::ChordResolver;
    use norte_vfs::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(
            super::Pane::new(d.clone(), Vec::new()),
            super::Pane::new(d, Vec::new()),
        )
    }

    fn plugin(id: &str, approved: bool, enabled: bool) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: "ACME".to_owned(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            has_help: true,
        }
    }

    /// El MISMO `help.md` en los dos casos del test de abajo: declara el
    /// comando en su front matter (la fila) y lo cita en la prosa (la marca en
    /// línea). Fíjese en lo que NO lleva: un título. El header de un tema no
    /// tiene dónde ponerlo — `parse_untrusted` solo conserva claves de
    /// despacho — así que el nombre solo puede salir del manifiesto.
    const PAGINA: &[u8] = b"+++\nid = \"org.norte.demo\"\ntitle = \"Demo\"\n\
                            commands = [\"plugin:org.norte.demo:greet\"]\n+++\n\
                            La marca propia: {{cmd:plugin:org.norte.demo:greet}}";

    /// El nombre de un comando sale de la FOTO (el manifiesto), jamás del
    /// `help.md`. El plugin escribe los dos, así que solo uno puede mandar, y
    /// tiene que ser el que ve el humano que aprueba el plugin: el gestor de
    /// extensiones, la paleta y la solicitud de aprobación muestran el del
    /// manifiesto, y una página que llamara `greet` de otra manera dejaría al
    /// lector sin saber qué está aprobando.
    ///
    /// Se demuestra cambiando el manifiesto con los MISMOS bytes de página: si
    /// el texto pintado sigue al manifiesto, la página no es la fuente.
    #[test]
    fn el_nombre_de_un_comando_sale_de_la_foto_no_de_la_pagina() {
        let pintado_con = |titulo: &str| -> String {
            let mut app = app();
            app.help = Some(super::HelpView::new(norte_help::Lang::En, Vec::new()));
            let mut p = plugin("org.norte.demo", true, true);
            p.commands = vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".to_owned(),
                title: titulo.to_owned(),
            }];
            app.freeze_help_plugins(&[p]);
            let help = app.help.as_mut().expect("abierta");
            help.state.open(&norte_help::TopicId::new("org.norte.demo"));
            let parsed = norte_help::parse_untrusted(PAGINA, "org.norte.demo", None);
            help.state.install_plugin_topic(parsed.topic);
            app.refresh_help(70, 20);
            let (lines, _) = app.help.as_ref().expect("abierta").body();
            lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let texto = pintado_con("Greet the world");
        assert!(
            texto.contains("Greet the world"),
            "la fila y la marca llevan el nombre del manifiesto: {texto}"
        );
        assert!(
            !texto.contains("plugin:org.norte.demo:greet"),
            "y NO su clave de despacho, ni en la prosa ni en la fila: {texto}"
        );
        // Dos veces: una en la prosa (la marca en línea) y otra en la tabla de
        // filas ejecutables. `render_command` y `rows_of` comparten
        // `label_or_id` justo para que no puedan discrepar.
        assert_eq!(texto.matches("Greet the world").count(), 2, "{texto}");

        // Mismos bytes de página, otro manifiesto: manda el manifiesto.
        let otro = pintado_con("Saludar al mundo");
        assert!(otro.contains("Saludar al mundo"), "{otro}");
        assert!(!otro.contains("Greet the world"), "{otro}");
    }

    /// H3e: la foto congela las DOS mitades a la vez — la barra ofrece la
    /// página de cada plugin con `help.md`, y el resolver atenúa los comandos
    /// de los que no están aprobados-y-activos. Si sólo cuajara una, el lector
    /// leería una página cuyas filas prometen lo que la app va a rechazar.
    #[test]
    fn la_foto_llega_a_la_barra_y_al_resolver() {
        let mut app = app();
        app.help = Some(super::HelpView::new(norte_help::Lang::En, Vec::new()));
        app.freeze_help_plugins(&[
            plugin("acme.ftp", true, true),
            plugin("otro.off", true, false),
        ]);

        let help = app.help.as_ref().expect("la ayuda está abierta");
        let ids: Vec<String> = help
            .state
            .rows()
            .iter()
            .filter_map(|r| match r {
                norte_frontend::help::SidebarRow::Topic { id, .. } => Some(id.as_str().to_owned()),
                norte_frontend::help::SidebarRow::Group { .. } => None,
            })
            .collect();
        assert!(ids.iter().any(|i| i == "acme.ftp"), "{ids:?}");
        assert!(
            ids.iter().any(|i| i == "otro.off"),
            "un plugin apagado CONSERVA su página — leerla es cómo se decide \
             encenderlo: {ids:?}"
        );

        assert!(
            app.help_chords
                .availability("plugin:acme.ftp:sync")
                .is_available()
        );
        assert_eq!(
            app.help_chords
                .availability("plugin:otro.off:sync")
                .reason(),
            Some(norte_help::Reason::PluginInactive),
            "pero sus filas no se ofrecen"
        );
    }

    /// La misma puerta, en la mitad del RESOLVER: ni el conjunto de activos ni
    /// el mapa de títulos pueden guardar un id que el host no debió anunciar.
    ///
    /// El id sale del corpus canónico (`plugin_id_bidi_segment`) y no de un
    /// literal: la GUI prueba su mitad de esta misma puerta contra la misma
    /// fixture, y dos frontends con su propia ortografía del adversario es
    /// justo la deriva que el corpus existe para no tener.
    #[test]
    fn un_id_invalido_no_entra_en_la_foto_del_resolver() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "plugin_id_bidi_segment")
            .expect("la fixture vive en el corpus canónico");
        let id = String::from_utf8(fixture.bytes).expect("la fixture es UTF-8");
        let key = format!("plugin:{id}:sync");
        let mut app = app();
        app.help = Some(super::HelpView::new(norte_help::Lang::En, Vec::new()));
        let mut malo = plugin(&id, true, true);
        malo.commands = vec![norte_proto::methods::PluginCommandInfo {
            id: "sync".to_owned(),
            title: "Sincronizar".to_owned(),
        }];
        app.freeze_help_plugins(&[malo]);
        assert_eq!(
            norte_help::ChordResolver::availability(&*app.help_chords, &key).reason(),
            Some(norte_help::Reason::PluginInactive),
            "no está activo: su id nunca entró en el conjunto"
        );
        assert_eq!(
            norte_help::ChordResolver::label(&*app.help_chords, &key)
                .chars()
                .filter(|c| norte_encoding::is_terminal_hazard(*c))
                .count(),
            0,
            "y su título no llegó al mapa: la etiqueta cae al repliegue seguro"
        );
    }

    /// El re-congelado de hechos que el embudo de refresco hace con la ayuda
    /// abierta (`main::after_panes_refresh`) NO puede apagar las filas de
    /// plugin a mitad de lectura, ni al revés.
    #[test]
    fn recongelar_los_hechos_no_pierde_la_foto_de_plugins() {
        let mut app = app();
        app.help = Some(super::HelpView::new(norte_help::Lang::En, Vec::new()));
        app.freeze_help_plugins(&[plugin("acme.ftp", true, true)]);
        app.freeze_help_facts();
        assert!(
            app.help_chords
                .availability("plugin:acme.ftp:sync")
                .is_available()
        );
    }
}

#[cfg(test)]
mod which_key_tests {
    use super::{App, Pane};
    use norte_frontend::keymap::{
        Availability, Effective, Resolution, Resolver, Screen, parse_chord, parse_keymap,
    };
    use norte_i18n::Lang;
    use norte_proto::{Scheme, VPath};

    /// Counts ON, one `g` prefix with an available branch, an unavailable one
    /// (`pane.pack` is `Planned`, #132 — with K2b's presets that is a normal
    /// row, not an edge case) and a deeper branch.
    fn resolver() -> Resolver {
        let src = r#"
counts = true

[pane]
keymap = [
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["g", "p"], run = "pane.pack" },
    { on = ["j"], run = "cursor.down" },
]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = ["cursor.top", "cursor.down"];
        let eff =
            Effective::build_for(&preset, &[], &known, Screen::Browse).expect("fixture builds");
        Resolver::new(eff)
    }

    fn app() -> App {
        let root = VPath::root(Scheme::new("mem").expect("scheme"), None);
        App::new(
            Pane::new(root.clone(), Vec::new()),
            Pane::new(root, Vec::new()),
        )
    }

    fn push(app: &mut App, r: &mut Resolver, key: &str) -> Resolution {
        let res = r.push(parse_chord(key).expect("chord"));
        match res {
            Resolution::Pending(_) | Resolution::Counting(_) => app.show_pending(r, Lang::En),
            _ => app.clear_pending(),
        }
        res
    }

    /// The pending arm OPENS it — on the keystroke itself, with no delay of
    /// any kind (ADR 0006) — and the `Run` that ends the sequence closes it.
    #[test]
    fn a_pending_prefix_opens_the_panel_and_a_run_closes_it() {
        let (mut app, mut r) = (app(), resolver());
        assert!(matches!(
            push(&mut app, &mut r, "g"),
            Resolution::Pending(1)
        ));
        let wk = app.which_key.as_ref().expect("the panel is open");
        assert_eq!(wk.title, "g");
        let chords: Vec<&str> = wk.rows.iter().map(|row| row.chord.as_str()).collect();
        assert_eq!(chords, vec!["g", "p"], "{chords:?}");

        assert!(matches!(
            push(&mut app, &mut r, "g"),
            Resolution::Run { .. }
        ));
        assert!(
            app.which_key.is_none(),
            "the sequence ended: so does the panel"
        );
        assert!(app.pending.is_empty(), "and the bar segment goes with it");
    }

    /// A BARE count does not open it: the continuation of a count is any key
    /// at all, so the panel would be the whole keymap. The bar still paints
    /// the number (K2a) — a count that cannot be seen cannot be cancelled.
    #[test]
    fn a_bare_count_does_not_open_the_panel() {
        let (mut app, mut r) = (app(), resolver());
        assert!(matches!(
            push(&mut app, &mut r, "1"),
            Resolution::Counting(1)
        ));
        assert!(matches!(
            push(&mut app, &mut r, "2"),
            Resolution::Counting(12)
        ));
        assert!(app.which_key.is_none(), "a bare count has no panel");
        assert_eq!(app.pending, "12", "but the bar shows it");
    }

    /// A count BEHIND a prefix is the state a reader most often cannot
    /// explain, so the panel that opens says the number is still in flight.
    #[test]
    fn a_count_behind_a_prefix_shows_in_the_title() {
        let (mut app, mut r) = (app(), resolver());
        push(&mut app, &mut r, "1");
        push(&mut app, &mut r, "2");
        push(&mut app, &mut r, "g");
        let wk = app.which_key.as_ref().expect("the panel is open");
        assert_eq!(wk.title, "12 g");
    }

    /// An unavailable continuation is a ROW, dimmed and explained — hiding it
    /// would recreate the silence K1 removed.
    #[test]
    fn the_rows_include_an_unavailable_binding_with_its_reason() {
        let (mut app, mut r) = (app(), resolver());
        push(&mut app, &mut r, "g");
        let wk = app.which_key.as_ref().expect("the panel is open");
        let p = wk
            .rows
            .iter()
            .find(|row| row.chord == "p")
            .expect("the pane.pack row");
        assert!(matches!(p.avail, Availability::NotBuilt { issue: 132, .. }));
        assert!(p.reason.contains("132"), "{:?}", p.reason);
    }

    /// `Esc` cancels a sequence, and every other end of the pending state
    /// closes the panel with the bar segment: the two are written by the same
    /// pair of methods precisely so they cannot disagree.
    #[test]
    fn esc_and_a_miss_close_it_too() {
        let (mut app, mut r) = (app(), resolver());
        push(&mut app, &mut r, "g");
        assert!(matches!(push(&mut app, &mut r, "esc"), Resolution::Reset));
        assert!(app.which_key.is_none(), "Esc cancelled the sequence");

        push(&mut app, &mut r, "g");
        // A chord that continues nothing is a miss: same treatment.
        assert!(matches!(push(&mut app, &mut r, "z"), Resolution::Reset));
        assert!(app.which_key.is_none());

        // And a key the frontend does not model at all reaches neither arm:
        // the run loop resets the resolver and clears both by hand.
        push(&mut app, &mut r, "g");
        r.reset();
        app.clear_pending();
        assert!(app.which_key.is_none());
        assert!(app.pending.is_empty());
    }
}
