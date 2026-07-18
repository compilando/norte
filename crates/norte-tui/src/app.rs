//! Estado puro del TUI (panes, cursor, presentación de nombres) y la
//! presentación de ERRORES para la barra (#73): categorías Fluent
//! ([`error_key`]/[`error_category`] y compañía) + saneado de detalle
//! ([`detail_for_bar`]). Máquina testeable sin terminal — el render (`ui`)
//! y el I/O (`main`) viven aparte; los scripts Lua (M4) consumen de aquí la
//! clave ESTABLE de [`error_key`].

use norte_i18n::{t, ta};
use norte_proto::{Entry, EntryKind, Error, VPath};

/// Un panel: directorio actual y sus entradas YA ordenadas.
#[derive(Debug)]
pub struct Pane {
    /// Directorio listado.
    pub dir: VPath,
    /// Entradas ordenadas ([`sort_entries`]).
    pub entries: Vec<Entry>,
    /// Índice bajo el cursor (0 incluso con lista vacía).
    pub cursor: usize,
    /// El listado se está RELLENANDO en background (paginación, ADR 0017): la
    /// primera página ya se pintó y llegan más entradas. La UI lo marca — un
    /// listado incompleto JAMÁS es silencioso.
    pub loading: bool,
    /// Quick search vivo (`/`, spec 2026-07-18): `None` = navegación normal.
    /// En modo Filter la SELECCIÓN vive dentro del estado (el cursor real no
    /// se mueve hasta confirmar); en Jump el cursor real salta directo.
    pub quick: Option<crate::nav::QuickSearch>,
}

impl Pane {
    /// Pane sobre `dir` con `entries` (ordénalas antes con [`sort_entries`]).
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        Self {
            dir,
            entries,
            cursor: 0,
            loading: false,
            quick: None,
        }
    }

    /// La entrada seleccionada: con quick search en modo Filter, la
    /// selección DENTRO del filtro (así F5/F8/F3… operan sobre lo filtrado
    /// sin que cada comando sepa del quick search — feed-to-listbox); si el
    /// filtro no tiene matches, `None` (las ops no-opean, jamás actúan
    /// sobre una entrada que el usuario no ve). Sin filtro (o en Jump, que
    /// mueve el cursor real), la entrada bajo el cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        if let Some(q) = &self.quick
            && q.mode() == crate::nav::Mode::Filter
        {
            return self.entries.get(q.selected_entry_index()?);
        }
        self.entries.get(self.cursor)
    }

    /// Arranca el quick search (`/`) en `mode` sobre las entries actuales.
    pub fn quick_start(&mut self, mode: crate::nav::Mode) {
        self.quick = Some(crate::nav::QuickSearch::new(mode, &self.entries));
    }

    /// En Jump el cursor REAL sigue a la selección del quick search (el
    /// listado no cambia; saltar ES mover el cursor). En Filter, no-op.
    fn quick_sync_jump(&mut self) {
        if let Some(q) = &self.quick
            && q.mode() == crate::nav::Mode::Jump
            && let Some(i) = q.selected_entry_index()
        {
            self.cursor = i;
        }
    }

    /// Un carácter tecleado con el quick search activo.
    pub fn quick_char(&mut self, c: char) {
        if let Some(q) = &mut self.quick {
            q.push_char(c, &self.entries);
            self.quick_sync_jump();
        }
    }

    /// Backspace con el quick search activo.
    pub fn quick_backspace(&mut self) {
        if let Some(q) = &mut self.quick {
            q.backspace(&self.entries);
            self.quick_sync_jump();
        }
    }

    /// Selección del quick search una posición abajo.
    pub fn quick_down(&mut self) {
        if let Some(q) = &mut self.quick {
            q.down();
            self.quick_sync_jump();
        }
    }

    /// Selección del quick search una posición arriba.
    pub fn quick_up(&mut self) {
        if let Some(q) = &mut self.quick {
            q.up();
            self.quick_sync_jump();
        }
    }

    /// Siguiente match con wrap (Tab en modo Jump).
    pub fn quick_next(&mut self) {
        if let Some(q) = &mut self.quick {
            q.next_match();
            self.quick_sync_jump();
        }
    }

    /// Cierra el quick search SIN tocar el cursor real: en Filter el listado
    /// completo vuelve con el cursor donde estaba (el filtro nunca lo movió
    /// — test del plan); en Jump el cursor se queda donde saltó.
    pub fn quick_cancel(&mut self) {
        self.quick = None;
    }

    /// Cierra el quick search fijando el cursor REAL a la selección (Enter:
    /// la op siguiente parte de ahí). Devuelve `true` si FIJÓ selección:
    /// sin matches cierra sin mover nada y devuelve `false` — el caller no
    /// debe despachar nada sobre el cursor real, que apunta a una entrada
    /// que el usuario NO veía (la lista filtrada estaba vacía).
    pub fn quick_confirm(&mut self) -> bool {
        if let Some(q) = self.quick.take()
            && let Some(i) = q.selected_entry_index()
        {
            self.cursor = i;
            return true;
        }
        false
    }

    /// Índices REALES visibles bajo el filtro; `None` = sin filtro (quick
    /// inactivo, o modo Jump: el listado se pinta entero).
    #[must_use]
    pub fn quick_visible(&self) -> Option<&[usize]> {
        self.quick
            .as_ref()
            .filter(|q| q.mode() == crate::nav::Mode::Filter)
            .map(crate::nav::QuickSearch::visible)
    }

    /// Path de la entrada seleccionada DENTRO del quick search, capturado
    /// ANTES de mutar/re-sortear `entries` (contrato de
    /// [`crate::nav::QuickSearch::refresh`]: los índices de antes del sort
    /// no identifican nada).
    fn quick_prev_selected(&self) -> Option<VPath> {
        let i = self.quick.as_ref()?.selected_entry_index()?;
        Some(self.entries.get(i)?.path.clone())
    }

    /// Sube el cursor `n` posiciones (con tope en 0).
    pub fn move_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// Baja el cursor `n` posiciones (con tope en la última entrada).
    pub fn move_down(&mut self, n: usize) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = (self.cursor + n).min(max);
    }

    /// Cursor a la primera entrada.
    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    /// Cursor a la última entrada.
    pub fn move_to_end(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
    }

    /// Reemplaza el contenido tras un cd/refresh, reseteando el cursor.
    /// Un quick search vivo muere: filtraba OTRO listado.
    pub fn set_listing(&mut self, dir: VPath, entries: Vec<Entry>) {
        self.dir = dir;
        self.entries = entries;
        self.cursor = 0;
        self.loading = false;
        self.quick = None;
    }

    /// Primera página de un listado paginado: reemplaza el contenido y MARCA
    /// que faltan entradas por llegar (ADR 0017). El drenador irá llamando a
    /// [`Pane::extend_listing`] y, al terminar, [`Pane::finish_listing`].
    pub fn begin_listing(&mut self, dir: VPath, first_page: Vec<Entry>, more: bool) {
        self.dir = dir;
        self.entries = first_page;
        self.cursor = 0;
        self.loading = more;
        self.quick = None;
    }

    /// Añade un lote del drenador: re-ordena TODO y re-ancla el cursor al path
    /// que estaba seleccionado (si desapareció del re-orden, clamp por índice)
    /// para que rellenar no mueva la selección del usuario bajo sus pies. Un
    /// quick search vivo se RE-APLICA sobre el listado nuevo (spec: el filtro
    /// no se congela mientras el fill sigue), conservando su selección por
    /// path — capturado ANTES del sort, que invalida los índices.
    pub fn extend_listing(&mut self, batch: Vec<Entry>) {
        if batch.is_empty() {
            return;
        }
        let quick_prev = self.quick_prev_selected();
        let selected = self.entries.get(self.cursor).map(|e| e.path.clone());
        self.entries.extend(batch);
        sort_entries(&mut self.entries);
        self.cursor = match selected.and_then(|p| self.entries.iter().position(|e| e.path == p)) {
            Some(i) => i,
            None => self.cursor.min(self.entries.len().saturating_sub(1)),
        };
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }

    /// El drenador terminó: el listado ya está completo. El quick search se
    /// re-aplica por contrato (hoy no muta entries: refresh barato; si algún
    /// día el cierre re-sortea, el filtro no se queda con índices muertos).
    pub fn finish_listing(&mut self) {
        let quick_prev = self.quick_prev_selected();
        self.loading = false;
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
    }

    /// Listado COMPLETO nuevo del MISMO dir (refresh tras una mutación):
    /// cursor conservado por ÍNDICE con clamp (tras un delete queda en la
    /// siguiente entrada — semántica ortodoxa) y quick search re-aplicado
    /// por path (los índices del listado viejo no identifican nada).
    pub fn refresh_listing(&mut self, entries: Vec<Entry>) {
        let quick_prev = self.quick_prev_selected();
        self.cursor = self.cursor.min(entries.len().saturating_sub(1));
        self.entries = entries;
        self.loading = false;
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
        self.quick_sync_jump();
    }
}

/// Orden del listado (presentación): directorios primero; dentro de cada
/// grupo, por la forma NFC del nombre (spec §6.1: `unicode_compare = nfc`
/// por defecto — SOLO como clave de orden, los bytes jamás se mutan) con
/// desempate por bytes crudos. Nombres no-UTF8: bytes tal cual.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| {
        let name = name_bytes(e);
        (e.kind != EntryKind::Dir, nfc_key(name), name.to_vec())
    });
}

fn nfc_key(name: &[u8]) -> Vec<u8> {
    use unicode_normalization::UnicodeNormalization;
    match std::str::from_utf8(name) {
        Ok(s) => s.nfc().collect::<String>().into_bytes(),
        Err(_) => name.to_vec(),
    }
}

fn name_bytes(e: &Entry) -> &[u8] {
    e.path.file_name().map_or(b"", |n| n.as_bytes())
}

/// ¿Debe enmascararse en un terminal? Cc (controles: `\n`, ESC — ratatui
/// los BORRA en silencio y un frontend directo los ejecutaría), los
/// overrides bidi Cf (spoofing RTL del orden visual) y los INVISIBLES Cf/Zl/Zp
/// (encoding-auditor H4 de M3-3b: dos nombres visualmente idénticos que
/// difieren en bytes engañan a un humano que aprueba "el que ya vio"):
/// ZWSP/ZWNJ, LRM/RLM/ALM, WORD JOINER, BOM/ZWNBSP, SOFT HYPHEN, TAG chars
/// (strings enteros invisibles) y los separadores Zl/Zp (U+2028/9, que
/// `is_control` no coge). ZWJ (U+200D) se PERMITE a sabiendas: enmascararlo
/// rompería los emoji compuestos legítimos (fixture `emoji_zwj_family`) —
/// fidelidad de emoji > el residual de un twin invisible solo-ZWJ.
fn must_mask(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            | '\u{200B}' | '\u{200C}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
            | '\u{2060}' | '\u{FEFF}' | '\u{00AD}' | '\u{2028}' | '\u{2029}'
            | '\u{E0000}'..='\u{E007F}')
}

/// Nombre listo para pintar: `(texto, hostil)`. `hostil = true` cuando el
/// texto pintado DIFIERE del nombre real: bytes no-UTF8 (lossy `�`),
/// controles o bidi enmascarados a `�` (spec §6: display siempre lossy y
/// MARCADO — jamás pérdida silenciosa, jamás controles crudos).
#[must_use]
pub fn display_name(bytes: &[u8]) -> (String, bool) {
    let (raw, lossy) = match std::str::from_utf8(bytes) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), false),
        Err(_) => (String::from_utf8_lossy(bytes), true),
    };
    let mut masked = false;
    let texto: String = raw
        .chars()
        .map(|c| {
            if must_mask(c) {
                masked = true;
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect();
    (texto, lossy || masked)
}

/// Path completo listo para pintar: `display_lossy` + marca si CUALQUIER
/// segmento saldría alterado (mismo criterio que [`display_name`]).
#[must_use]
pub fn path_display(p: &VPath) -> (String, bool) {
    let hostil = p.segments().any(|s| display_name(s).1);
    (p.display_lossy(), hostil)
}

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
        }
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

/// Diálogo modal activo. Sus teclas van HARDCODEADAS (son la semántica del
/// diálogo, no bindings del usuario); el contexto `dialog` del keymap es
/// deuda anotada (issue #24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Confirmación de borrado (F8). `permanent = false` → papelera.
    ConfirmDelete {
        /// Lo que se borraría.
        target: VPath,
        /// `true` = borrado PERMANENTE (sin papelera aquí, o elección
        /// explícita): el diálogo AVISA (ADR 0009).
        permanent: bool,
    },
    /// Confirmación de copy/move (F5/F6).
    ConfirmTransfer {
        /// Copy o Move.
        kind: TransferKind,
        /// Origen (la entrada seleccionada).
        from: VPath,
        /// Destino (el dir del otro pane + el nombre).
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

/// Teclas de diálogo: Esc SIEMPRE cancela; confirmaciones aceptan Enter/y
/// y rechazan n; la colisión elige o/s/r/n (sin default en Enter: no hay
/// respuesta inocua que merezca dispararse sola).
#[must_use]
pub fn dialog_key(modal: &Modal, code: crossterm::event::KeyCode) -> DialogOutcome {
    use crossterm::event::KeyCode as K;
    use norte_proto::CollisionPolicy as P;
    if code == K::Esc {
        return DialogOutcome::Cancelled;
    }
    match modal {
        Modal::ConfirmDelete { .. } | Modal::ConfirmTransfer { .. } => match code {
            K::Enter | K::Char('y') => DialogOutcome::Confirmed,
            K::Char('n') => DialogOutcome::Cancelled,
            _ => DialogOutcome::Open,
        },
        Modal::Collision { .. } => match code {
            K::Char('o') => DialogOutcome::Retry(P::Overwrite),
            K::Char('s') => DialogOutcome::Retry(P::Skip),
            K::Char('r') => DialogOutcome::Retry(P::RenameAuto),
            K::Char('n') => DialogOutcome::Retry(P::Newer),
            _ => DialogOutcome::Open,
        },
        // Decisiones de SEGURIDAD (aprobar una op de agente, o confiar en una
        // host key TOFU #45): solo `y` confirma, `n`/Esc cancelan (con el Esc
        // global de arriba; cerrar ES denegar — fail-safe), Enter NUNCA
        // confirma (sin default peligroso que se dispare solo).
        Modal::ApproveAgentOp { .. } | Modal::TrustHostKey { .. } | Modal::TrustLuaInit { .. } => {
            match code {
                K::Char('y') => DialogOutcome::Confirmed,
                K::Char('n') => DialogOutcome::Cancelled,
                _ => DialogOutcome::Open,
            }
        }
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
    use norte_proto::Scheme;

    fn root() -> VPath {
        VPath::root(Scheme::new("mem").unwrap(), None)
    }

    fn file(name: &str) -> Entry {
        Entry {
            path: root().join(norte_proto::Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    fn names(p: &Pane) -> Vec<String> {
        p.entries
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
        p.loading = true;
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
        p.cursor = 1; // "z.txt"
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
        p.cursor = 0;
        p.extend_listing(vec![]);
        assert_eq!(names(&p), vec!["a.txt"]);
        assert_eq!(p.cursor, 0);
    }

    /// `finish_listing` limpia el flag de carga.
    #[test]
    fn finish_limpia_loading() {
        let mut p = Pane::new(root(), vec![]);
        p.loading = true;
        p.finish_listing();
        assert!(!p.loading);
    }

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Pane sobre `mem://` con archivos en el ORDEN dado (sin sort: los
    /// tests del quick search razonan sobre índices reales conocidos).
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
        assert!(p.quick.is_none(), "confirmar cierra el quick search");
        assert_eq!(p.cursor, 2, "cursor real = índice real de a2");
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
        p.cursor = 1;
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('x'); // cero matches
        assert!(p.selected().is_none(), "sin matches no hay selección");
        assert!(
            !p.quick_confirm(),
            "confirmar sin matches NO fija selección"
        );
        assert!(p.quick.is_none(), "el quick search sí se cierra");
        assert_eq!(p.cursor, 1, "el cursor real queda intacto");
    }

    /// Modo salto: el listado NO cambia; teclear mueve el cursor REAL al
    /// primer match y Tab (`quick_next`) al siguiente con wrap.
    #[test]
    fn quick_jump_mueve_el_cursor_real() {
        let mut p = pane_con(&["ab", "zz", "ac"]);
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('a');
        assert_eq!(p.cursor, 0, "salta al primer match");
        assert!(
            p.quick_visible().is_none(),
            "en salto el listado queda intacto"
        );
        p.quick_next();
        assert_eq!(p.cursor, 2, "Tab: siguiente match");
        p.quick_next();
        assert_eq!(p.cursor, 0, "wrap");
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

    /// TOFU (#45): confiar es decisión de seguridad — solo `y` confía; `n` y
    /// Esc cancelan; Enter NO confía (sin default peligroso).
    #[test]
    fn trust_host_key_solo_y_confia() {
        use crossterm::event::KeyCode as K;
        let m = Modal::TrustHostKey {
            host: "h".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            dir: root(),
        };
        assert_eq!(dialog_key(&m, K::Char('y')), DialogOutcome::Confirmed);
        assert_eq!(dialog_key(&m, K::Char('n')), DialogOutcome::Cancelled);
        assert_eq!(dialog_key(&m, K::Esc), DialogOutcome::Cancelled);
        assert_eq!(
            dialog_key(&m, K::Enter),
            DialogOutcome::Open,
            "Enter jamás confía en una host key"
        );
    }

    /// TOFU Lua (M4): mismo contrato que la host key — ejecutar el script de
    /// un repo ajeno es decisión de seguridad: solo `y` confía; `n` y Esc
    /// deniegan; Enter NO decide.
    #[test]
    fn trust_lua_init_solo_y_confia_y_enter_no_decide() {
        use crossterm::event::KeyCode as K;
        let m = Modal::TrustLuaInit {
            path: "repo/.norte/init.lua".into(),
            hash_abbrev: "ab12cd34".into(),
        };
        assert_eq!(dialog_key(&m, K::Char('y')), DialogOutcome::Confirmed);
        assert_eq!(dialog_key(&m, K::Char('n')), DialogOutcome::Cancelled);
        assert_eq!(dialog_key(&m, K::Esc), DialogOutcome::Cancelled);
        assert_eq!(
            dialog_key(&m, K::Enter),
            DialogOutcome::Open,
            "Enter jamás aprueba ejecutar un script ajeno"
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
