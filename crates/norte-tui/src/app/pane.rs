//! Un pane: sus entradas, su cursor, sus marcas y el diálogo de búsqueda que
//! vive dentro de él.

use norte_proto::{Entry, VPath};

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
    /// (`search-status-failed`) tras limpiarse [`crate::app::App::message`] — un fallo no
    /// puede degradar a «done» en la siguiente tecla (review MINOR-2).
    pub search_error: Option<String>,
    /// Contexto del match de contenido por hit de la búsqueda viva (#81):
    /// `path → (línea, preview YA saneado en origen)`. Solo significativo con
    /// [`Pane::virtual_search`]; la barra lo pinta para el hit bajo el
    /// cursor. Se limpia al salir del modo virtual (cd/listado real).
    pub search_matches: std::collections::HashMap<VPath, norte_proto::methods::MatchInfo>,
    /// El listado de este pane NO se pudo hacer al restaurar la sesión, y lo
    /// que se ve no es «este directorio está vacío» (#235).
    ///
    /// Se marca en el TÍTULO del pane, igual que la paginación en curso, y no
    /// en un `message`: es un estado que dura hasta que alguien liste de
    /// verdad, y un mensaje lo borra la tecla siguiente — que es justo el bug
    /// que #232 arregla dos hunks más arriba. Cualquier listado real
    /// ([`Pane::set_listing`], [`Pane::begin_listing`]) lo apaga.
    pub unlisted: bool,
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
/// error concreto viaja por [`crate::app::App::message`] vía `error_message`); se
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
    /// primero, NFC, empate por bytes, ver [`crate::app::sort_entries`]).
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        Self {
            state: norte_frontend::PaneState::new(dir, entries),
            virtual_search: false,
            search_state: SearchState::Running,
            search_error: None,
            search_matches: std::collections::HashMap::new(),
            show_hidden_pref: true,
            unlisted: false,
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

    /// Entradas ordenadas ([`crate::app::sort_entries`]), la fila `..`
    /// incluida: es la lista que se PINTA.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        self.state.entries()
    }

    /// Las entradas de VERDAD, sin la fila `..`: lo que se copia cuando un
    /// pane nace del listado de otro ([`crate::app::App::fork_pane`]).
    #[must_use]
    pub fn real_entries(&self) -> &[Entry] {
        self.state.real_entries()
    }

    /// A dónde apunta el cursor para un gesto de panel: la carpeta bajo él si
    /// lo es, y si no este directorio
    /// ([`norte_frontend::PaneState::target_dir`]).
    #[must_use]
    pub fn target_dir(&self) -> &VPath {
        self.state.target_dir()
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

    /// La entrada bajo el cursor PARA DESCRIBIRLA, fila `..` incluida.
    ///
    /// La otra pregunta, la de los paneles que siguen al cursor: ver
    /// [`norte_frontend::PaneState::cursor_entry`]. **No es un operando.**
    #[must_use]
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.state.cursor_entry()
    }

    /// ¿Lo señalado AHORA es la fila `..`? Ver
    /// [`norte_frontend::PaneState::cursor_is_parent_row`] — sale del mismo
    /// índice que [`Self::cursor_entry`], y por eso no es
    /// `is_parent_row(cursor())`.
    #[must_use]
    pub fn cursor_is_parent_row(&self) -> bool {
        self.state.cursor_is_parent_row()
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

    /// Deja la ventana lista para pintar `rows` filas — delegado puro a
    /// [`norte_frontend::PaneState::reconcile_viewport`]. El run loop lo llama
    /// ANTES de cada draw.
    pub fn reconcile_viewport(&mut self, rows: usize) {
        self.state.reconcile_viewport(rows);
    }

    /// La primera fila visible del listado — delegado puro a
    /// [`norte_frontend::PaneState::viewport_offset`]. Lo leen el pintado y el
    /// hit test del ratón, que tienen que ver la MISMA ventana.
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.state.viewport_offset()
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
        self.unlisted = false;
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
        self.unlisted = false;
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

    /// El espejo del de arriba, hacia ARRIBA (`shift+↑`). Delegado puro.
    pub fn toggle_mark_and_retreat(&mut self) {
        self.state.toggle_mark_and_retreat();
    }

    /// Marca (o desmarca) el tramo de `n` filas desde el cursor y se mueve
    /// allí (`shift+PgDn`/`shift+PgUp`). Delegado puro.
    pub fn toggle_mark_page(&mut self, n: usize, hacia_abajo: bool) {
        self.state.toggle_mark_page(n, hacia_abajo);
    }

    /// Krusader `Shift+Home`: marca del cursor hacia arriba y desmarca el
    /// resto. Delegado puro.
    pub fn mark_to_top(&mut self) {
        self.state.mark_to_top();
    }

    /// Krusader `Shift+End`: marca del cursor hacia abajo y desmarca el
    /// resto. Delegado puro.
    pub fn mark_to_bottom(&mut self) {
        self.state.mark_to_bottom();
    }

    /// Marca todas las entradas visibles. Delegado puro (#103).
    pub fn mark_all(&mut self) {
        self.state.mark_all();
    }

    /// Marca (o desmarca) las que comparten extensión con la del cursor.
    /// Delegado puro (#313).
    pub fn mark_same_extension(&mut self, mark: bool) -> usize {
        self.state.mark_same_extension(mark)
    }

    /// Marca las visibles que son directorios (`dirs`) o las que no lo son.
    /// Delegado puro (#313).
    pub fn mark_kind(&mut self, dirs: bool) -> usize {
        self.state.mark_kind(dirs)
    }

    /// Devuelve la selección anterior al último gesto en bloque. Delegado
    /// puro (#313).
    pub fn restore_previous_marks(&mut self) -> Option<usize> {
        self.state.restore_previous_marks()
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

    /// Las entradas MARCADAS, sin caer al cursor. Delegado puro (#312).
    #[must_use]
    pub fn marked_entries(&self) -> Vec<&Entry> {
        self.state.marked_entries()
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

    /// ¿Se ven los ocultos? Delegado puro.
    #[must_use]
    pub fn show_hidden(&self) -> bool {
        self.state.show_hidden()
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

    /// Enciende o apaga la fila `..` (`[ui] parent_entry`). Delegado puro.
    pub fn set_parent_row(&mut self, on: bool) {
        self.state.set_parent_row(on);
    }

    /// ¿La fila `i` es la de subir? Delegado puro: lo pregunta el pintado
    /// —para escribir `..` en vez del nombre del padre— y la navegación.
    #[must_use]
    pub fn is_parent_row(&self, i: usize) -> bool {
        self.state.is_parent_row(i)
    }

    /// A dónde lleva la fila de subir, si la hay. Delegado puro.
    #[must_use]
    pub fn parent_target(&self) -> Option<&VPath> {
        self.state.parent_target()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::sort_entries;
    use crate::app::testutil::*;
    use norte_proto::{EntryKind, VPath};

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
        let mut empty = pane_con(&[]);
        empty.quick_start(crate::nav::Mode::Jump);
        assert!(
            !empty.quick_confirm(),
            "sin entradas no hay nada que operar"
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
}
