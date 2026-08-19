//! El panel de diferencias de la GUI (#158, spec 3 fase C1): el run abierto,
//! las reglas PURAS que deciden qué le entra y cómo acaba, el TEXTO de cada
//! fila y el árbol GPUI que lo pinta.
//!
//! El render vive aquí y no en `main.rs` a propósito: ese fichero pasa de las
//! catorce mil líneas y su único `Render` ya reparte diez pantallas. Lo que
//! `main.rs` conserva es la ORQUESTACIÓN —dónde encaja este panel entre las
//! demás pantallas, y qué hace con lo que una tecla decide—, igual que con
//! [`crate::settings_view`].
//!
//! # Lo que decide una fila es TEXTO, no color
//! Spec §17: un veredicto tiene que leerse sin color. Cada fila pinta los dos
//! glifos de [`norte_frontend::compare`] —qué se decidió y cuánto vale esa
//! decisión— como texto, y la etiqueta accesible los dice además con
//! palabras. El color solo los REFUERZA, y se resuelve una vez por frame
//! (nunca por fila: es la trampa O(N)-por-frame del #87).
//!
//! # Lo que este módulo NO reimplementa
//! El modelo (filas, filtros, selección, lado activo) y el estado del run son
//! [`norte_frontend::compare`], los MISMOS que pinta la TUI. Aquí solo se
//! añade lo que es de esta GUI: a qué task pertenece lo que llega, y cómo se
//! traduce un [`SessionEvent`](crate::session::SessionEvent) en una mutación
//! de ese modelo. Reimplementar la cuenta de «¿llegaron todas las filas?» es
//! exactamente lo que el CLI (fase A) y la tool MCP (fase B) hicieron, y las
//! dos se equivocaron: ambas dieron por completa una respuesta a la que le
//! faltaban lotes.

use gpui::px;
use norte_frontend::compare::CompareState;
use norte_proto::methods::CompareRow;
use norte_proto::{TaskId, TaskState, VPath};

use crate::sp;

/// El panel de diferencias abierto en la GUI: el run compartido con la TUI,
/// más la task de la que es dueño.
pub struct CompareView {
    /// La task de ESTA comparación.
    ///
    /// No es decoración: es lo único que distingue un lote de esta
    /// comparación de uno de otra que el lector lanzó y canceló. Los eventos
    /// de la anterior siguen en vuelo por el puente GPUI↔tokio cuando la
    /// vista nueva ya está abierta —el hilo de sesión los emite desde otra
    /// task de tokio, sin ninguna barrera con esta—, así que un lote ajeno
    /// llega SIEMPRE que se comparan dos árboles seguidos. La TUI no necesita
    /// el filtro porque su run vive junto al panel en el mismo bucle; aquí no
    /// hay tal cosa.
    pub task_id: TaskId,
    /// El run: filas, filtros, selección, estado terminal y las dos raíces.
    /// Es [`norte_frontend::compare::CompareView`], el mismo tipo que la TUI.
    pub run: norte_frontend::compare::CompareView,
    /// Cuántas filas han LLEGADO por el canal, contadas aquí y no derivadas
    /// del pane: es la mitad de la comparación que decide `Done` contra
    /// `Incomplete`, y tiene que contar lo RECIBIDO aunque el modelo algún
    /// día deje de guardar todo lo que recibe.
    pub rows_received: u64,
}

impl CompareView {
    /// Un panel recién abierto sobre la task que acaba de arrancar.
    ///
    /// `left_pane` es el pane que la LANZÓ (el lado izquierdo, aunque sea el
    /// pane derecho de la pantalla) y viaja congelado desde que se pidió la
    /// comparación, no leído del foco al llegar la respuesta: el foco puede
    /// haberse movido mientras la petición estaba en vuelo.
    pub fn new(
        task_id: TaskId,
        left_root: VPath,
        right_root: VPath,
        left_pane: usize,
        left_encoding: Option<norte_encoding::NameEncoding>,
        right_encoding: Option<norte_encoding::NameEncoding>,
    ) -> Self {
        Self {
            task_id,
            run: norte_frontend::compare::CompareView::new(
                left_root,
                right_root,
                left_pane,
                left_encoding,
                right_encoding,
            ),
            rows_received: 0,
        }
    }

    /// Aplica un lote de filas. Devuelve `false` —y no toca nada— si el lote
    /// es de OTRA comparación (ver [`CompareView::task_id`]).
    pub fn on_rows(&mut self, task_id: TaskId, rows: Vec<CompareRow>) -> bool {
        if task_id != self.task_id {
            return false;
        }
        self.rows_received = self
            .rows_received
            .saturating_add(rows.len().try_into().unwrap_or(u64::MAX));
        self.run.pane.extend(rows);
        true
    }

    /// Se cerró el canal de filas: fija el estado terminal a partir del
    /// snapshot de progreso que el hilo de sesión leyó al cerrarse
    /// (`state`/`entries_done`). Devuelve `false` —y no toca nada— si el
    /// final es de OTRA comparación.
    ///
    /// El mapeo `TaskState` → estado del panel no está aquí: es
    /// [`norte_frontend::compare::CompareView::finish_from_task`], el MISMO
    /// que llama la TUI. Aquí había una transcripción a mano de sus cuatro
    /// brazos (revisión de rama, MAJOR-1), y solo uno de los cuatro
    /// —`Completed`— llegaba al crate compartido: los otros tres eran copias
    /// que el día del #183 habría que arreglar dos veces, y una de ellas
    /// decide si una comparación FALLIDA cuenta como completa.
    ///
    /// La categoría del fallo queda en `run.error`, ya localizada: es lo que
    /// el pie pinta de forma PERSISTENTE y lo que el llamante convierte en
    /// banner.
    pub fn on_done(&mut self, task_id: TaskId, state: &TaskState, entries_done: u64) -> bool {
        if task_id != self.task_id {
            return false;
        }
        self.run.finish_from_task(
            state,
            entries_done,
            self.rows_received,
            norte_i18n::active(),
        );
        true
    }
}

/// Abre el panel para la comparación que acaba de ARRANCAR, y devuelve la
/// Task a la que hay que mandarle `task.cancel`: la del panel al que
/// sustituye, si lo había.
///
/// Dos comparaciones a la vez serían dos flujos alimentando un panel (regla
/// 3), así que la anterior no se deja corriendo — mismo criterio que
/// `launch_compare` en la TUI, que cancela el run que reemplaza. Es una
/// función y no un método porque la decisión es sobre el HUECO (`Option`), no
/// sobre una vista: quien elige a quién cancelar es precisamente el caso en
/// el que todavía no hay vista abierta.
#[must_use]
pub fn open(slot: &mut Option<CompareView>, started: Started) -> Option<TaskId> {
    let superseded = slot.take().map(|old| old.task_id);
    *slot = Some(CompareView::new(
        started.task_id,
        started.left_root,
        started.right_root,
        started.left_pane,
        started.left_encoding,
        started.right_encoding,
    ));
    superseded
}

/// La lista `include` que sale de las filas MARCADAS del panel, o el motivo
/// por el que no hay una (#249).
///
/// Aquí y no dentro de `App::start_sync` para que se pueda probar sin ventana:
/// lo que hacía falta demostrar es que las marcas LLEGAN al plan, y el defecto
/// era exactamente que no llegaban —`include: None` incondicional, o sea el
/// árbol entero, o sea, bajo `Mirror`, un `DeleteTree` por cada huérfano del
/// destino—. QUÉ cuenta como negativa y contra qué raíz se mide cada marca lo
/// decide [`norte_frontend::sync::include_from_rows`], que es la misma que usa
/// la TUI: dos respuestas a «qué entra en el plan» es la clase de divergencia
/// que produce un plan plausible sobre el árbol equivocado.
///
/// # Errors
/// Lo que devuelva aquella; el llamante lo traduce con
/// [`norte_frontend::sync::include_error_message`].
pub fn include_from_marks(
    run: &norte_frontend::compare::CompareView,
    source: &norte_proto::VPath,
    dest: &norte_proto::VPath,
) -> Result<Option<Vec<norte_proto::methods::RelPath>>, norte_frontend::sync::IncludeError> {
    norte_frontend::sync::include_from_rows(source, dest, &run.pane.marked_rows())
}

/// La frase de un `fs.compare` RECHAZADO antes de existir Task alguna, o
/// `None` si esa petición ya está SUPERADA.
///
/// El `None` no es «no hay nada que decir»: es lo que el llamante escribe en
/// `errors[pane]`, y escribir `None` retira el «comparando…» que esa misma
/// petición dejó puesto. Nadie más lo va a retirar — la petición que la superó
/// limpia el SUYO, en el pane que ella lanzó, que no tiene por qué ser este.
///
/// El guard existe porque cada `Compare` es su propio `tokio::spawn` y dos
/// teclas seguidas pueden contestar en orden INVERSO: sin él, la negativa de
/// una petición vieja pintaba «la comparación falló» describiendo algo que el
/// lector ya había reemplazado (revisión de rama, MINOR-3). Es el mismo
/// `generation_is_current` que filtra `CompareStarted`, no una segunda regla.
///
/// La categoría va por `banner_safe`, como todo lo que entra en un banner.
#[must_use]
pub fn failed_banner(
    current_gen: u64,
    generation: u64,
    error: &norte_proto::Error,
) -> Option<String> {
    if !crate::generation_is_current(current_gen, generation) {
        return None;
    }
    Some(norte_i18n::ta(
        "compare-status-failed",
        &[(
            "error",
            crate::banner_safe(&norte_frontend::error::error_category(error)).as_str(),
        )],
    ))
}

/// Lo que hace falta para abrir un panel: el evento `CompareStarted` con las
/// dos reinterpretaciones ya resueltas. Un struct y no seis argumentos
/// sueltos, que es como se cruzan dos raíces del mismo tipo por error.
pub struct Started {
    /// La Task recién creada.
    pub task_id: TaskId,
    /// Raíz izquierda: la del pane que lanzó.
    pub left_root: VPath,
    /// Raíz derecha.
    pub right_root: VPath,
    /// El pane que lanzó (índice ya acotado a 0|1 por el llamante).
    pub left_pane: usize,
    /// Reinterpretación de nombres (#57) del lado izquierdo.
    pub left_encoding: Option<norte_encoding::NameEncoding>,
    /// La del lado derecho.
    pub right_encoding: Option<norte_encoding::NameEncoding>,
}

/// Encamina un lote de filas: entra si es del panel abierto, y se descarta
/// si no.
///
/// **Nunca cancela nada**, y esa es UNA regla para los tres casos. La tenía
/// invertida entre dos ramas vecinas frente al mismo riesgo (revisión de
/// rama, MINOR-9): descartaba sin cancelar el lote de una comparación
/// anterior —porque los `TaskId` son únicos por PROCESO del daemon, así que
/// tras un reinicio ese id puede pertenecer ya a otra task, y cancelarla
/// dejaría un `.norte-partial` que nadie pidió (revisión MINOR-3)— pero
/// cancelaba ese mismo id cuando el panel estaba cerrado, donde el riesgo es
/// idéntico.
///
/// Y no hace falta: cancelar es de quien SUELTA el panel, y los dos únicos
/// caminos que lo sueltan ya lo hacen — [`open`] cancela a la que sustituye y
/// `NorteGui::close_compare` cancela la suya, siempre. Así que un lote que
/// llega sin panel viene de una Task que ya recibió su `task.cancel`, y
/// repetirlo solo añade el riesgo del id reciclado. **Quien añada un tercer
/// camino que ponga `compare` a `None` tiene que cancelar allí**, no aquí.
pub fn route_rows(slot: &mut Option<CompareView>, task_id: TaskId, rows: Vec<CompareRow>) {
    if let Some(view) = slot.as_mut() {
        view.on_rows(task_id, rows);
    }
}

// ---------------------------------------------------------------------------
// El texto de una fila (puro: sin GPUI, testeable sin ventana ni GPU)
// ---------------------------------------------------------------------------

/// Una cara de la fila, ya pintable: el nombre saneado (con el badge de la
/// GUI delante si el saneado tuvo que alterarlo) y su tamaño.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaceText {
    /// Badge hostil + nombre + indicador de tipo. **Vacío** cuando de este
    /// lado no hay nada: el huérfano se pinta con la cara ausente EN BLANCO
    /// y jamás con un guion inventado — la columna del centro ya dice `<` o
    /// `>`, y un relleno en la cara vacía es lo que hace que un huérfano se
    /// lea como una pareja (mismo criterio que `compare_face_span` en la
    /// TUI).
    pub label: String,
    /// Tamaño legible, o vacío si el provider no lo sabe (jamás un cero
    /// fabricado).
    pub size: String,
    /// El saneado ALTERÓ el nombre (spec §6). El badge visual ya va dentro de
    /// [`FaceText::label`]; esto existe para la superficie AURAL, que no lo
    /// tiene: un lector de pantalla con la verbosidad de símbolos por defecto
    /// no pronuncia `⚠`, y bajo una reinterpretación activa (#57) el nombre
    /// enmascarado no lleva ni un solo `U+FFFD` — es texto limpio y legible
    /// que difiere de los bytes del disco, y el badge es su ÚNICA marca. Sin
    /// este bool, quien escucha la fila no se entera (auditoría de encoding
    /// MAJOR-2).
    pub hostile: bool,
}

/// Una fila entera, ya pintable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowText {
    /// Cara izquierda.
    pub left: FaceText,
    /// Cara derecha.
    pub right: FaceText,
    /// Las DOS marcas del centro, veredicto y confianza, como texto: esto es
    /// lo que hace la fila legible sin color (spec §17).
    pub marks: String,
    /// Nombre accesible de la FILA: el veredicto y la confianza **con
    /// palabras**, para quien no ve ni el glifo ni el color — y **nada más**.
    ///
    /// Los nombres NO están aquí, y esa ausencia es la corrección de la
    /// auditoría de encoding (MAJOR-2). El camino visual separa cara
    /// izquierda, marcas y cara derecha en tres elementos hermanos, así que
    /// un nombre no puede falsificar el veredicto: el separador es
    /// ESTRUCTURAL. Aplanarlos en una sola cadena unida por `·` le devolvía
    /// esa capacidad a la superficie aural — un fichero llamado
    /// `informe · igual · seguro.txt` (el fixture `score_spoof_inband` usa
    /// exactamente ese separador) se leería como una fila completa cuyo
    /// veredicto lo elige quien nombró el fichero. Cada cara lleva su propio
    /// nombre accesible, en su propio elemento.
    pub a11y: String,
}

/// Etiqueta de una cara: badge hostil + nombre + indicador de tipo, con el
/// MISMO badge (`crate::HOSTILE_BADGE`) y el mismo indicador que el listado
/// de esta GUI — no un segundo marcador propio de este panel.
fn face_text(face: Option<&norte_frontend::compare::RowFace>) -> FaceText {
    let Some(f) = face else {
        return FaceText::default();
    };
    FaceText {
        label: format!(
            "{}{}{}",
            if f.hostile {
                format!("{} ", crate::HOSTILE_BADGE)
            } else {
                String::new()
            },
            f.name,
            crate::kind_indicator(f.kind),
        ),
        size: f.size.map_or_else(String::new, norte_frontend::human_bytes),
        hostile: f.hostile,
    }
}

/// Nombre accesible de UNA cara: el nombre ya enmascarado, precedido de una
/// PALABRA localizada cuando el saneado lo alteró.
///
/// La palabra y no el glifo: `⚠` no se pronuncia con la verbosidad de
/// símbolos por defecto de NVDA ni de Orca (auditoría de encoding MAJOR-2).
/// Vacío para la cara ausente de un huérfano — no hay nada que nombrar.
#[must_use]
pub fn face_a11y(f: &FaceText) -> String {
    if f.label.is_empty() {
        return String::new();
    }
    if f.hostile {
        return format!("{}: {}", norte_i18n::t("gui-a11y-hostile-name"), f.label);
    }
    f.label.clone()
}

/// Lo que una fila PINTA, resuelto de una vez: las dos caras (con la
/// reinterpretación de nombres de SU lado, #57 — dos y no una, porque los dos
/// panes son dos ubicaciones) y las dos marcas.
///
/// Todo sale de [`norte_frontend::compare::cells_for`], el mismo cálculo que
/// pinta la TUI: aquí solo se compone el texto de esta GUI.
#[must_use]
pub fn row_text(
    row: &CompareRow,
    left_reinterpret: Option<norte_encoding::NameEncoding>,
    right_reinterpret: Option<norte_encoding::NameEncoding>,
) -> RowText {
    use norte_frontend::compare::{cells_for, confidence_label, verdict_label};

    let cells = cells_for(row, left_reinterpret, right_reinterpret);
    let left = face_text(cells.left.as_ref());
    let right = face_text(cells.right.as_ref());
    let lang = norte_i18n::active();
    RowText {
        marks: format!("{}{}", cells.glyphs.verdict, cells.glyphs.confidence),
        a11y: format!(
            "{} · {}",
            verdict_label(row.verdict, lang),
            confidence_label(row.confidence, lang),
        ),
        left,
        right,
    }
}

/// Tope de celdas por raíz en el título. Una raíz más larga se corta CON su
/// marca (`middle_ellipsis` recorta por el medio, #79: por celdas y no por
/// chars) en vez de que el `.truncate()` del div se la lleve por la derecha
/// en silencio — «jamás pérdida silenciosa» (spec §6).
const ROOT_MAX_CELLS: usize = 72;

/// Las dos raíces del título, cada una por su lado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleText {
    /// Raíz izquierda, badgeada y acotada.
    pub left: String,
    /// Raíz derecha, ídem.
    pub right: String,
}

/// Las dos raíces que el título pinta, ya saneadas, badgeadas y ACOTADAS —
/// **y nunca unidas en una sola cadena**.
///
/// Los dos defectos que esta separación cierra (auditoría de encoding
/// MAJOR-1) se componían entre sí:
///
/// 1. `↔` y `⟨`/`⟩` son caracteres imprimibles corrientes: no son hazards de
///    terminal, así que `display_name_with` no los enmascara y un directorio
///    llamado `docs ↔ ⟨file⟩/home/victima/backup` es legal en ext4, APFS y
///    NTFS y llega SIN badge. Unido en banda, el título se lee como OTRO par
///    de raíces (mismo molde que el fixture `arrow_join_spoof`).
/// 2. Una raíz izquierda larga expulsaba a la derecha entera por el
///    `.truncate()` del div, sin `…` y sin ninguna otra señal — de modo que
///    lo que quedaba visible era un par completo, plausible y equivocado.
///
/// Por eso el separador es ESTRUCTURAL: el pintor le da a cada raíz su
/// propio elemento y al `↔` el suyo, así que un `↔` incrustado en un nombre
/// no puede fingir el límite, y ninguna de las dos puede comerse a la otra.
#[must_use]
pub fn title_text(run: &norte_frontend::compare::CompareView) -> TitleText {
    let one = |p: &VPath, enc: Option<norte_encoding::NameEncoding>| {
        let (texto, hostil) = norte_frontend::path_display_with(p, enc);
        let texto = norte_frontend::middle_ellipsis(&texto, ROOT_MAX_CELLS);
        if hostil {
            format!("{} {texto}", crate::HOSTILE_BADGE)
        } else {
            texto
        }
    };
    TitleText {
        left: one(&run.left_root, run.left_encoding),
        right: one(&run.right_root, run.right_encoding),
    }
}

/// El pie: cómo va (o cómo acabó) la comparación, y sobre qué lado actúa el
/// `Enter`.
///
/// La frase entera la compone [`norte_frontend::compare::status_line`],
/// COMPARTIDA con la TUI. Aquí había una copia verbatim de sus cinco brazos
/// —revisión rust MAJOR-3—, y esa copia era justo lo que este módulo dice en
/// su cabecera que no hay que hacer: lo que la frase decide es si la
/// respuesta está COMPLETA, y el CLI (fase A) y la tool MCP (fase B) ya se
/// equivocaron cada uno en su propia copia de esa cuenta.
///
/// El `0` es el recuento de marcas: la GUI todavía no marca filas (su
/// superficie de sincronización es #161), así que la frase sale sin esa
/// cláusula. Es un dato, no una rama.
#[must_use]
pub fn status_line(view: &norte_frontend::compare::CompareView) -> String {
    norte_frontend::compare::status_line(view, 0, norte_i18n::active())
}

/// Un estado que ya no espera nada del daemon: el pie deja de decir
/// «comparando…» y el `Esc` deja de tener nada que cancelar.
#[must_use]
pub fn is_running(view: &norte_frontend::compare::CompareView) -> bool {
    view.state == CompareState::Running
}

// ---------------------------------------------------------------------------
// El teclado del panel (puro)
// ---------------------------------------------------------------------------

/// Cuántas filas mueve una tecla de página. El mismo 10 fijo que la TUI
/// (`COMPARE_PAGE_STEP`): el alto real del panel no llega hasta el momento
/// del layout de GPUI, y un salto que cambia con el tamaño de la ventana es
/// peor de aprender que uno constante.
const PAGE_STEP: isize = 10;

/// Lo que una tecla SIGNIFICA dentro del panel de diferencias.
///
/// Separado del despacho por lo mismo que el `CompareKey` de la TUI: lo que
/// se puede equivocar aquí es la DECISIÓN, y una de ellas —el `Esc`— es la
/// única salida de una pantalla que se queda el teclado entero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Ni la toca.
    Ignore,
    /// Pedir la cancelación de la Task, conservando las filas que llegaron.
    CancelTask,
    /// Cerrar el panel (y cancelar lo que quede vivo).
    Close,
    /// Cambiar el lado activo.
    SwapSide,
    /// Mover el cursor.
    Move(isize),
    /// Al principio de lo visible.
    First,
    /// Al final de lo visible.
    Last,
    /// Alternar el filtro de la categoría `n` de
    /// [`norte_frontend::compare::CATEGORIES`].
    Filter(usize),
    /// Ir a donde vive la fila del cursor, por el lado activo.
    Open,
    /// Pedir un plan de sincronización en ese sentido (#188).
    ///
    /// Las dos letras están aquí y no en el keymap por lo mismo que en la TUI:
    /// dentro de este panel el teclado es entero suyo, así que no chocan con
    /// nada.
    ///
    /// `pane.sync-dirs` SÍ se alcanza con el panel abierto —por la ayuda, que
    /// se despacha antes—, pero solo lleva `Update`. Lo que no tenía forma de
    /// pedirse desde esta frontend era `Mirror`: el dispatch conocía un único
    /// punto de entrada y era el otro modo.
    Sync(norte_proto::methods::SyncMode),
    /// Marcar o desmarcar la fila del cursor: lo que se marque siembra el
    /// `include` del plan (#249).
    ///
    /// Sin esto, el espejo de esta frontend era SIEMPRE el árbol entero: cada
    /// huérfano del destino un `DeleteTree`, y ninguna forma de reducirlo. Con
    /// `Update` era inerte; con `Mirror` es la diferencia entre «borra estos
    /// tres» y «borra todo lo que el origen no tiene».
    Mark,
}

/// Traduce una tecla de GPUI (`"escape"`, `"pagedown"`, `"1"`…) a lo que
/// significa en este panel.
///
/// * El primer `Esc` sobre una comparación VIVA la cancela y conserva sus
///   filas; **cualquier `Esc` posterior cierra**, sin mirar el estado de la
///   Task. Condicionar el cierre a un estado terminal deja encerrado al
///   lector cuando el canal de filas no llega a cerrarse nunca —un daemon
///   caído, un provider colgado en una NFS muerta—, que es exactamente el
///   BLOCKER-1 que la TUI ya pagó.
/// * Con `ctrl`/`alt`/`cmd` no significa nada: sin ese filtro `alt+1`
///   togglearía un filtro y `ctrl+tab` cambiaría de lado.
/// * **No hay tecla de salir de norte**, y ahí sí diverge de la TUI, que
///   tuvo que añadir `Ctrl+C` a su panel porque en modo raw `ISIG` está
///   apagado y esa era la única pantalla de la que no se salía. Una ventana
///   tiene el botón de cerrar del gestor de ventanas, que no es algo que
///   este panel pueda comerse.
/// * **`held` descarta la repetición de teclado, y solo para `s`/`m`.** Todas
///   las demás teclas de este panel son locales; esas dos cuestan un recorrido
///   RECURSIVO de los dos árboles, y cada pulsación sube `sync_gen` y lanza un
///   `sync.plan` nuevo. Un plan superado solo se cancela cuando llega su
///   propio `SyncPlanStarted`, un viaje de ida y vuelta después, así que
///   mantener `m` pulsada dos segundos contra un par SFTP arrancaba decenas de
///   recorridos concurrentes antes de que llegara la primera cancelación
///   (#249). Moverse por la lista con la flecha pulsada sigue siendo lo
///   normal.
///
///   **`held` no basta y no es el freno principal**: el backend de X11 de GPUI
///   pone `is_held: false` SIEMPRE —la repetición automática llega como
///   pulsaciones normales—, y solo el de Wayland lo marca. El freno que
///   funciona en los dos es `App::awaiting_sync_plan`, que no deja salir una
///   segunda petición mientras la primera está en vuelo. Esto es la mitad
///   barata: en Wayland corta antes de construir nada.
#[must_use]
pub fn key_meaning(
    key: &str,
    modified: bool,
    running: bool,
    cancel_requested: bool,
    held: bool,
) -> Key {
    if modified {
        return Key::Ignore;
    }
    if held && matches!(key, "s" | "m") {
        return Key::Ignore;
    }
    match key {
        "escape" => {
            if running && !cancel_requested {
                Key::CancelTask
            } else {
                Key::Close
            }
        }
        "tab" => Key::SwapSide,
        "up" => Key::Move(-1),
        "down" => Key::Move(1),
        "pageup" => Key::Move(-PAGE_STEP),
        "pagedown" => Key::Move(PAGE_STEP),
        "home" => Key::First,
        "end" => Key::Last,
        "enter" => Key::Open,
        // LETRAS PELADAS, las mismas que la TUI (#188): `s` actualiza y `m`
        // espeja. Sin ellas `SyncMode::Mirror` era código muerto en esta
        // frontend —la mitad DESTRUCTIVA del spec §5.2, sin superficie— y
        // `sync_roots` no se alcanzaba con el panel abierto.
        "s" => Key::Sync(norte_proto::methods::SyncMode::Update),
        "m" => Key::Sync(norte_proto::methods::SyncMode::Mirror),
        // `insert`, la misma tecla que la TUI: marcar es del lector sobre la
        // FILA entera, no sobre uno de los dos lados.
        "insert" => Key::Mark,
        // El rango del patrón es exactamente el de `CATEGORIES`, así que el
        // índice no puede salirse.
        "1" | "2" | "3" | "4" | "5" => key
            .as_bytes()
            .first()
            .map_or(Key::Ignore, |b| Key::Filter(usize::from(b - b'1'))),
        _ => Key::Ignore,
    }
}

// ---------------------------------------------------------------------------
// El render
// ---------------------------------------------------------------------------

/// Ancho de la columna central de marcas. Fijo: las dos caras se reparten el
/// resto a partes iguales, igual que en la TUI.
const MARKS_W: f32 = 34.0;

/// Los colores del panel, resueltos UNA vez por frame.
///
/// El coste de resolver un rol del tema es por FRAME y jamás por fila: el
/// panel puede tener cientos de miles de filas (la colección es ilimitada a
/// propósito, ver [`norte_frontend::compare::ComparePane`]) y resolver el
/// tema dentro del bucle es la trampa O(N)-por-frame del #87, la misma que
/// el listado evita construyendo su paleta antes de entrar.
///
/// No resuelve nada nuevo del tema: se deriva de la
/// [`crate::ChromeColors`] que `render` ya construyó (con su glow aplicado),
/// así que este panel no puede desviarse del resto del chrome.
#[derive(Clone, Copy)]
struct Palette {
    /// Texto normal.
    fg: gpui::Rgba,
    /// Texto secundario (cabecera de columnas, cuentas, teclas).
    dim: gpui::Rgba,
    /// Algo que mirar: difiere, o está en un lado solo.
    warn: gpui::Rgba,
    /// Algo que salió mal: tipos que no casan, fila ambigua, error.
    bad: gpui::Rgba,
    /// Un veredicto que ESTA build no sabe leer (un daemon más nuevo). Ni
    /// normal ni error: «hay algo aquí que no sé interpretar», que es
    /// exactamente lo que dice su glifo `?`.
    unknown: gpui::Rgba,
    /// Fondo de la fila bajo el cursor.
    sel_bg: gpui::Rgba,
    /// Texto de la fila bajo el cursor, si el tema lo declara.
    sel_fg: Option<gpui::Rgba>,
}

impl Palette {
    fn from_chrome(c: &crate::ChromeColors) -> Self {
        Self {
            fg: c.fg,
            // Roles de tema PROPIOS, no `quick_fg` prestado: ese es la mitad
            // de un par (`Match.fg`/`Match.bg`) y sobre este fondo era una
            // pareja de contraste sin auditar; además dejaba `warn == dim`,
            // o sea la fila que DIFIERE pintada con el color de atenuar y la
            // que es igual a plena fuerza — el énfasis al revés (revisión
            // rust MAJOR-1).
            dim: c.info_fg,
            warn: c.warn_fg,
            bad: c.err_fg,
            unknown: c.info_fg,
            sel_bg: c.sel_bg,
            sel_fg: c.sel_fg,
        }
    }

    /// El color de las dos marcas de una fila. El GLIFO ya distingue el
    /// veredicto sin color ninguno (spec §17); esto solo lo refuerza para
    /// quien sí lo ve. Un `match` sobre un `Copy`, no una consulta al tema:
    /// corre por cada fila PINTADA.
    ///
    /// Brazo por brazo el mismo reparto que `compare_mark_style` en la TUI,
    /// **incluido el último**: un veredicto que esta build no conoce sale en
    /// `Info` y no en rojo. Que un daemon más nuevo haya inventado una
    /// palabra no es que algo vaya mal — pintarlo como un error dice una cosa
    /// que no consta (revisión rust MAJOR-1).
    fn for_verdict(&self, verdict: norte_proto::methods::CompareVerdict) -> gpui::Rgba {
        use norte_proto::methods::CompareVerdict as V;
        match verdict {
            V::Same => self.fg,
            V::Different | V::OnlyLeft | V::OnlyRight => self.warn,
            V::TypeMismatch | V::Ambiguous | V::Error => self.bad,
            _ => self.unknown,
        }
    }
}

/// Pinta el panel de diferencias en el sitio de los dos panes.
///
/// Ocupa los DOS: una fila tiene dos caras y un veredicto en medio, así que
/// no cabe en media pantalla (mismo reparto que la TUI).
///
/// Nada de lo que decide QUÉ se ve está aquí (regla dura 7): las filas
/// visibles, la selección y las cuentas salen de
/// [`norte_frontend::compare`], que se testea sin ventana. Este lado reparte
/// el sitio y elige colores.
///
/// La CONSTRUCCIÓN está virtualizada (`uniform_list`, #124): el processor
/// solo corre para el rango visible, así que una comparación de un millón de
/// filas no construye un millón de elementos por frame. La TUI aprendió esto
/// por las malas —su review BLOCKER-2— y ahí el síntoma fue peor que la
/// lentitud: el pintor llenaba el canal de filas, cuyos lotes se DESCARTAN, y
/// el cliente destruía la completitud de la respuesta para luego culpar al
/// transporte.
///
/// # Lo que todavía es lineal, y no lo tapa la virtualización
/// `ComparePane::visible()` es un filtro sobre TODAS las filas, así que el
/// `skip(range.start)` de abajo recorre las ocultas hasta llegar a la
/// ventana: desplazado al final de un millón de filas, eso es un recorrido
/// por frame. El recuento (`visible_len`) ya no lo es —se deriva de las
/// cuentas cacheadas, O(5)—, pero `visible_index`/`move_by` siguen andando
/// (revisión rust MAJOR-2). Cerrarlo del todo pide un índice incremental de
/// visibles en `norte-frontend`, que pagaría también en la TUI; queda como
/// deuda, no como cosa hecha.
///
/// # El ratón todavía no
/// Las filas no llevan `on_mouse_down`: seleccionar y navegar es de teclado
/// en esta fase. Dicho aquí a propósito y no omitido — un panel de GUI que
/// se come los clics en silencio se lee como roto (revisión rust MINOR-4).
pub fn render(
    view: &CompareView,
    chrome: &crate::ChromeColors,
    fonts: &crate::FontSet,
    scroll: &gpui::UniformListScrollHandle,
    cx: &mut gpui::Context<crate::NorteGui>,
) -> impl gpui::IntoElement {
    use gpui::{ParentElement, Styled, prelude::*};

    let palette = Palette::from_chrome(chrome);
    let run = &view.run;
    let lang = norte_i18n::active();

    // El título lleva las dos raíces con la reinterpretación de SU lado y
    // badgeadas si el saneado las alteró (una ruta es tan hostil como un
    // nombre de fila) — cada una en su PROPIO elemento, ver `title_text`.
    let title = title_text(run);

    let visible = run.pane.visible_len();
    let row_h = fonts.row_h;
    let mono = fonts.mono.clone();

    let body = if visible == 0 {
        gpui::div()
            .flex_1()
            .px(px(sp::S))
            .py(px(sp::XS))
            .text_color(palette.dim)
            // «Todavía no hay filas» y «están todas ocultas» no son lo mismo,
            // y lo que el lector tiene que hacer después (esperar, o pulsar
            // 1-5) depende de cuál sea (revisión rust MINOR-2).
            .child(gpui::SharedString::from(norte_i18n::t(
                if run.pane.is_empty() {
                    "compare-empty"
                } else {
                    "compare-all-filtered"
                },
            )))
            .into_any_element()
    } else {
        // El `Role::List` va en un ENVOLTORIO y no en el propio
        // `uniform_list`: su id ("compare-rows") alimenta el scroll y la
        // medida virtualizados, y pisarlo con `.id()` para colgarle el rol
        // sería arriesgar esa identidad — exactamente el motivo por el que el
        // listado de panes lo hace así (`main.rs`, `pane-list-{i}`). Sin él,
        // las filas quedaban de `Role::ListItem` HUÉRFANAS: un lector de
        // pantalla no sabe de qué lista son ni cuántas hay (revisión de rama,
        // MINOR-4), que es la misma superficie auditiva a la que la tarea 3
        // le dedicó un MAJOR de encoding.
        gpui::div()
            .id("compare-rows-list")
            .role(gpui::Role::List)
            .aria_label(norte_i18n::t("gui-a11y-compare-rows"))
            .flex_1()
            .flex()
            .flex_col()
            .child(
                gpui::uniform_list(
                    gpui::SharedString::from("compare-rows"),
                    visible,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                        let Some(view) = this.compare.as_ref() else {
                            return Vec::new();
                        };
                        let run = &view.run;
                        let selected = run.pane.selected_id();
                        // `skip`/`take` sobre el iterador de visibles, igual que la
                        // TUI: solo se CONSTRUYE lo que se pinta.
                        run.pane
                            .visible()
                            .skip(range.start)
                            .take(range.len())
                            .map(|row| {
                                render_row(
                                    row,
                                    selected == Some(row.id),
                                    run.pane.is_marked(row.id),
                                    run.left_encoding,
                                    run.right_encoding,
                                    &palette,
                                    row_h,
                                )
                            })
                            .collect()
                    }),
                )
                .track_scroll(scroll)
                .flex_1()
                .font(mono),
            )
            .into_any_element()
    };

    // La fila de filtros: la tecla, si está encendido o apagado, el nombre y
    // cuántas filas hay en esa categoría. El apagado se marca con un GLIFO
    // (`-` frente a `+`) y no solo con un color (spec §17), y la cuenta se
    // sigue enseñando: esconder categorías es justo lo que haría mentir al
    // panel si no lo dijera.
    let mut filtros = gpui::div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(sp::M))
        .px(px(sp::S))
        .py(px(1.0)); // sub-XS: acento fino de una línea
    for (i, c) in norte_frontend::compare::CATEGORIES.iter().enumerate() {
        let apagado = run.pane.is_hidden(*c);
        let texto = format!(
            "{}{} {} {}",
            i + 1,
            if apagado { '-' } else { '+' },
            c.label(lang),
            run.pane.count_of(*c),
        );
        filtros = filtros.child(
            gpui::div()
                .text_color(if apagado {
                    gpui::Rgba {
                        a: 0.55,
                        ..palette.dim
                    }
                } else {
                    palette.fg
                })
                .child(gpui::SharedString::from(texto)),
        );
    }

    // El estado en rojo cuando se perdió algo, falló, o NO SE SABE (#183) — y
    // nunca SOLO en rojo: la frase ya lo dice con palabras.
    //
    // `Unknown` entra aquí porque el pane existe para contestar «¿coinciden
    // estos dos árboles?», y un «no consta» leído como un «sí» es el fallo
    // que este color previene. Es el mismo argumento por el que la variante
    // existe en vez de plegarse sobre `Done`.
    let estado_malo = matches!(
        run.state,
        CompareState::Incomplete | CompareState::Failed | CompareState::Unknown
    );

    gpui::div()
        .id("compare-view")
        .role(gpui::Role::Document)
        .aria_label(norte_i18n::t("compare-title"))
        .flex_1()
        .flex()
        .flex_col()
        .overflow_hidden()
        .border_2()
        .border_color(chrome.border_focus)
        .bg(chrome.pane_bg_focus)
        .child(
            gpui::div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(sp::S))
                .px(px(sp::S))
                .py(px(sp::XS))
                .bg(chrome.header_bg)
                .text_color(chrome.header_fg)
                .child(
                    gpui::div()
                        .flex_none()
                        .child(gpui::SharedString::from(norte_i18n::t("compare-title"))),
                )
                // Las dos raíces, cada una en su elemento y con el mismo
                // `flex_1`: ninguna puede expulsar a la otra, y el `↔` de
                // enmedio es un elemento propio que un nombre no puede fingir.
                .child(
                    gpui::div()
                        .flex_1()
                        .truncate()
                        .child(gpui::SharedString::from(title.left)),
                )
                .child(gpui::div().flex_none().child("↔"))
                .child(
                    gpui::div()
                        .flex_1()
                        .truncate()
                        .child(gpui::SharedString::from(title.right)),
                ),
        )
        // Cabecera de columnas: fuera de la lista, para que no se vaya con el
        // scroll (por lo mismo que la TUI la sacó de la fila 0).
        .child(
            gpui::div()
                .flex()
                .flex_row()
                .items_center()
                .px(px(sp::S))
                .text_color(palette.dim)
                // El hueco de la marca del lector, para que la cabecera caiga
                // sobre sus columnas y no una celda a la izquierda.
                .child(gpui::div().w(px(MARK_W)).flex_none())
                .child(
                    gpui::div()
                        .flex_1()
                        .truncate()
                        .child(gpui::SharedString::from(norte_i18n::t(
                            "compare-header-left",
                        ))),
                )
                .child(gpui::div().w(px(MARKS_W)))
                .child(
                    gpui::div()
                        .flex_1()
                        .truncate()
                        .child(gpui::SharedString::from(norte_i18n::t(
                            "compare-header-right",
                        ))),
                ),
        )
        .child(body)
        .child(filtros)
        .child(
            gpui::div()
                .px(px(sp::S))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .truncate()
                .text_color(if estado_malo { palette.bad } else { palette.fg })
                .child(gpui::SharedString::from(status_line(run))),
        )
        .child(
            gpui::div()
                .px(px(sp::S))
                .py(px(1.0)) // sub-XS: acento fino de una línea
                .truncate()
                .text_color(palette.dim)
                .child(gpui::SharedString::from(norte_i18n::t("gui-compare-hint"))),
        )
}

/// Ancho de la columna de la marca del lector. Fijo, a la izquierda del todo
/// y fuera de las dos caras: es una decisión sobre la FILA, no sobre un lado
/// (mismo sitio que en la TUI).
const MARK_W: f32 = 12.0;

/// Una fila: la marca del lector, cara izquierda, las dos marcas del
/// veredicto, cara derecha.
fn render_row(
    row: &CompareRow,
    selected: bool,
    marked: bool,
    left_encoding: Option<norte_encoding::NameEncoding>,
    right_encoding: Option<norte_encoding::NameEncoding>,
    palette: &Palette,
    row_h: gpui::Pixels,
) -> gpui::AnyElement {
    use gpui::{ParentElement, Styled, prelude::*};

    let text = row_text(row, left_encoding, right_encoding);
    // Cada cara es su propio nodo accesible, con su propio nombre: así el
    // nombre de un fichero no queda pegado a la palabra que dice el
    // veredicto, y no puede falsificarla (auditoría de encoding MAJOR-2).
    let face = |f: &FaceText, lado: &str| {
        gpui::div()
            .id(gpui::SharedString::from(format!(
                "compare-row-{}-{lado}",
                row.id
            )))
            .aria_label(gpui::SharedString::from(face_a11y(f)))
            .flex_1()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(sp::S))
            .overflow_hidden()
            .child(
                gpui::div()
                    .flex_1()
                    .truncate()
                    .child(gpui::SharedString::from(f.label.clone())),
            )
            // `flex_none`: el tamaño es un campo de COLA que no puede ser
            // expulsado por un nombre largo — invariante, no coincidencia de
            // layout (misma regla que la TUI, que le reserva su ancho).
            .child(
                gpui::div()
                    .flex_none()
                    .child(gpui::SharedString::from(f.size.clone())),
            )
    };
    let mut r = gpui::div()
        // El id sale del `id` de la FILA (u64, monotónico desde el motor) y
        // no de su posición en la ventana: un `as usize` truncaría en 32 bits
        // y dos filas distintas compartirían identidad de elemento. Mismo
        // molde `format!` que `render_row` del listado.
        .id(gpui::SharedString::from(format!("compare-row-{}", row.id)))
        .role(gpui::Role::ListItem)
        // La marca va TAMBIÉN en el nombre accesible: un lector de pantalla no
        // ve el asterisco, y lo que está marcado es lo que el plan va a tocar.
        .aria_label(gpui::SharedString::from(if marked {
            format!("{} · {}", norte_i18n::t("gui-compare-marked"), text.a11y)
        } else {
            text.a11y.to_string()
        }))
        .aria_selected(selected)
        .flex()
        .flex_row()
        .items_center()
        .h(row_h)
        .px(px(sp::S))
        .rounded(px(sp::RADIUS_ROW))
        .child(
            gpui::div()
                .w(px(MARK_W))
                .flex_none()
                .text_color(palette.warn)
                .child(gpui::SharedString::from(if marked { "*" } else { " " })),
        )
        .child(face(&text.left, "left"))
        .child(
            gpui::div()
                .w(px(MARKS_W))
                .flex()
                .justify_center()
                .text_color(palette.for_verdict(row.verdict))
                .child(gpui::SharedString::from(text.marks)),
        )
        .child(face(&text.right, "right"));
    if selected {
        r = r.bg(palette.sel_bg);
        if let Some(fg) = palette.sel_fg {
            r = r.text_color(fg);
        }
    }
    r.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{CompareView, Key, Started, key_meaning, open, route_rows, row_text, status_line};
    use norte_frontend::compare::{Category, CompareState};
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict,
    };
    use norte_proto::{Error, TaskId, TaskState, VPath};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    /// Una fila cualquiera, con el MISMO molde que la de la TUI
    /// (`norte-tui/src/main.rs::fila`): lo que se prueba aquí es la
    /// contabilidad del run, no el veredicto.
    fn fila(id: u64) -> CompareRow {
        CompareRow {
            id,
            left: None,
            right: None,
            verdict: CompareVerdict::Error,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Unknown,
            newer: None,
            reason: Some(CompareReason::Unreadable),
            side: None,
            paired_under: None,
        }
    }

    fn vista_de_prueba() -> CompareView {
        CompareView::new(
            TaskId::new(1),
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        )
    }

    /// Los lotes que llegan se acumulan, y el veredicto terminal sale de
    /// contrastar lo recibido con lo que la task CONTÓ — no de que el canal se
    /// cerrara.
    #[test]
    fn los_lotes_se_acumulan_y_el_final_se_verifica() {
        let mut v = vista_de_prueba();
        assert!(v.on_rows(TaskId::new(1), vec![fila(1), fila(2)]));
        assert_eq!(v.run.pane.len(), 2);
        assert_eq!(v.run.state, CompareState::Running, "un lote no cierra nada");

        assert!(v.on_done(TaskId::new(1), &TaskState::Completed, 2));
        assert_eq!(v.run.state, CompareState::Done);
    }

    /// Un lote perdido se ve, y NO se pinta «hecho» encima. Es el error que
    /// el CLI (fase A) y la tool MCP (fase B) cometieron cada uno por su lado.
    #[test]
    fn un_lote_perdido_no_se_pinta_como_hecho() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1)]);
        v.on_done(TaskId::new(1), &TaskState::Completed, 2);
        assert_eq!(v.run.state, CompareState::Incomplete);
        assert_eq!(v.run.rows_expected, 2);
    }

    /// **La carrera benigna, y el error simétrico del anterior.** El canal de
    /// filas se cierra ANTES de que el estado terminal se publique (las dos
    /// bombas son tasks independientes), así que el snapshot que se lee sigue
    /// diciendo `Running` y su `entries_done` todavía no es definitivo:
    /// pasarlo por la cuenta acusaría de PÉRDIDA a una carrera que no lo es.
    ///
    /// Hasta #183 la respuesta era `Done`, y eso era media verdad. El mismo
    /// camino cubre una task que MURIÓ antes de publicar nada, y en ese
    /// instante las dos son indistinguibles — así que decir «hecha» era la
    /// única respuesta que una comparación no puede dar cuando no lo sabe.
    /// Ahora es `Unknown`: ni pérdida ni terminación, que es exactamente lo
    /// que consta.
    #[test]
    fn la_carrera_benigna_no_se_pinta_ni_perdida_ni_hecha() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1)]);
        // Estado NO terminal + un contador que va por delante de las filas.
        v.on_done(TaskId::new(1), &TaskState::Running, 9);
        assert_eq!(
            v.run.state,
            CompareState::Unknown,
            "no se acusa de pérdida, y tampoco se afirma que terminó"
        );
    }

    /// Cancelar conserva lo que llegó (la comparación no escribe nada: las
    /// filas ya vistas siguen siendo ciertas) y NO pasa por la cuenta —
    /// `Cancelled` sale del `TaskState`, no de comparar contadores.
    #[test]
    fn cancelar_conserva_las_filas_y_no_pasa_por_la_cuenta() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1), fila(2)]);
        v.on_done(TaskId::new(1), &TaskState::Cancelled, 9);
        assert_eq!(v.run.state, CompareState::Cancelled);
        assert_eq!(v.run.pane.len(), 2, "las filas que llegaron se quedan");
    }

    /// Un fallo guarda su categoría para pintarla de forma PERSISTENTE, y
    /// tampoco pasa por la cuenta.
    #[test]
    fn un_fallo_guarda_su_categoria() {
        let mut v = vista_de_prueba();
        v.on_done(
            TaskId::new(1),
            &TaskState::Failed {
                error: Error::PermissionDenied,
            },
            0,
        );
        assert_eq!(v.run.state, CompareState::Failed);
        assert!(v.run.error.is_some(), "la barra necesita la categoría");
    }

    fn entrada(wire: &str) -> norte_proto::Entry {
        norte_proto::Entry {
            path: vp(wire),
            kind: norte_proto::EntryKind::File,
            size: Some(3),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        }
    }

    /// Una pareja que el motor dio por IGUAL.
    fn fila_same(id: u64) -> CompareRow {
        CompareRow {
            id,
            left: Some(entrada("file:///a/x.txt")),
            right: Some(entrada("file:///b/x.txt")),
            verdict: CompareVerdict::Same,
            criterion: CompareCriterion::Size,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        }
    }

    /// Una pareja que DIFIERE, y con una confianza que no es la de la
    /// anterior: las dos marcas de la fila tienen que poder distinguirse
    /// entre sí.
    fn fila_distinta(id: u64) -> CompareRow {
        CompareRow {
            left: Some(entrada("file:///a/y.txt")),
            right: Some(entrada("file:///b/y.txt")),
            verdict: CompareVerdict::Different,
            criterion: CompareCriterion::Mtime,
            confidence: CompareConfidence::Probable,
            ..fila_same(id)
        }
    }

    /// Ocultar una categoría quita sus filas de lo que se PINTA y no de lo
    /// que se RECIBIÓ: un filtro es una vista, no una pérdida. Si escondiera
    /// filas de verdad, el pie seguiría contando las que ya no están y la
    /// comparación mentiría sobre su propia completitud.
    #[test]
    fn el_filtro_de_categoria_no_pierde_filas() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila_same(1), fila_distinta(2)]);
        v.run.pane.toggle_filter(Category::Same);

        assert_eq!(v.run.pane.visible().count(), 1, "la fila Same está oculta");
        assert_eq!(v.run.pane.len(), 2, "pero sigue estando");
        assert_eq!(
            v.run.pane.count_of(Category::Same),
            1,
            "y el filtro sigue diciendo cuántas esconde"
        );
    }

    /// **Spec §17: el color no es la señal.** Cada fila lleva su veredicto y
    /// su confianza como TEXTO, así que un lector que no distinga los colores
    /// —o una captura en blanco y negro— sigue leyendo qué decidió la
    /// comparación. Un panel que codificara el veredicto solo en el color
    /// habría regresado lo que la TUI hace bien.
    #[test]
    fn cada_fila_pinta_los_dos_glifos_como_texto() {
        let iguales = row_text(&fila_same(1), None, None);
        let distintas = row_text(&fila_distinta(2), None, None);

        assert_eq!(iguales.marks.chars().count(), 2, "veredicto y confianza");
        assert!(!iguales.marks.contains(' '), "ninguna marca en blanco");
        assert_ne!(
            iguales.marks, distintas.marks,
            "dos veredictos distintos no pueden pintar el mismo texto"
        );
        // Y la palabra completa llega al lector de pantalla, que no ve
        // glifos ni colores.
        assert!(
            distintas
                .a11y
                .contains(&norte_frontend::compare::verdict_label(
                    CompareVerdict::Different,
                    norte_i18n::active()
                )),
            "la etiqueta accesible dice el veredicto con palabras: {}",
            distintas.a11y
        );
    }

    /// **Auditoría de encoding MAJOR-2.** El nombre accesible de la FILA no
    /// lleva ni un byte de los nombres: si los llevara, un fichero llamado
    /// `informe · igual · seguro.txt` —el separador del fixture
    /// `score_spoof_inband` es exactamente ese `·`— se leería como una fila
    /// entera cuyo veredicto elige quien nombró el fichero. El camino visual
    /// separa las tres cosas en elementos hermanos; el aural tiene que
    /// separarlas igual.
    #[test]
    fn el_nombre_accesible_de_la_fila_no_lleva_nombres_de_fichero() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|f| f.id == "score_spoof_inband")
            .expect("el corpus trae score_spoof_inband");
        let seg = norte_proto::Segment::new(spoof.bytes.clone()).expect("segmento");
        let row = CompareRow {
            left: Some(norte_proto::Entry {
                path: vp("file:///a").join(seg),
                kind: norte_proto::EntryKind::File,
                size: None,
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }),
            right: None,
            verdict: CompareVerdict::OnlyLeft,
            ..fila_same(1)
        };
        let pintado = row_text(&row, None, None);
        assert!(
            !pintado.left.label.is_empty(),
            "el nombre sí se pinta, solo que en su propio elemento"
        );
        assert!(
            !pintado.a11y.contains("informe"),
            "ningún trozo del nombre puede estar junto al veredicto: {}",
            pintado.a11y
        );
        // Y la cara sí lleva su propio nombre accesible.
        assert!(super::face_a11y(&pintado.left).contains("informe"));
    }

    /// **Auditoría de encoding MAJOR-1.** Las dos raíces salen por campos
    /// SEPARADOS, así que un nombre con una flecha dentro (el fixture
    /// `arrow_join_spoof`) no puede fingir el límite entre ellas, y una raíz
    /// izquierda kilométrica no expulsa a la derecha: la derecha sigue
    /// entera en su campo.
    #[test]
    fn las_dos_raices_del_titulo_no_se_unen_en_banda() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|f| f.id == "arrow_join_spoof")
            .expect("el corpus trae arrow_join_spoof");
        let seg = norte_proto::Segment::new(spoof.bytes.clone()).expect("segmento");
        let run = norte_frontend::compare::CompareView::new(
            vp("file:///izquierda").join(seg),
            vp("file:///derecha/de/verdad"),
            0,
            None,
            None,
        );
        let t = super::title_text(&run);
        assert!(
            t.left.contains('→'),
            "la flecha se queda DENTRO de su raíz: {}",
            t.left
        );
        assert!(
            t.right.ends_with("de/verdad"),
            "y la derecha llega entera a su propio campo: {}",
            t.right
        );

        // Una raíz absurdamente larga se corta CON marca, no en silencio, y
        // sigue sin tocar a la otra.
        let larga =
            vp("file:///").join(norte_proto::Segment::new(vec![b'x'; 4096]).expect("segmento"));
        let run = norte_frontend::compare::CompareView::new(
            larga,
            vp("file:///derecha/de/verdad"),
            0,
            None,
            None,
        );
        let t = super::title_text(&run);
        assert!(
            t.left.chars().count() <= super::ROOT_MAX_CELLS + 8,
            "acotada"
        );
        assert!(t.left.contains('…'), "y el corte se MARCA: {}", t.left);
        assert!(t.right.ends_with("de/verdad"), "la otra sigue intacta");
    }

    /// **Auditoría de encoding MINOR-3.** Las dos reinterpretaciones (#57)
    /// son de LADOS distintos, y transponerlas pintaría la del pane derecho
    /// sobre los nombres del izquierdo — silenciosamente, porque el
    /// resultado sigue siendo texto legible. Esta es la única aserción que
    /// fija ese orden.
    #[test]
    fn cada_lado_usa_su_propia_reinterpretacion() {
        let bytes = b"CAF\x90.TXT".to_vec();
        let seg = norte_proto::Segment::new(bytes).expect("segmento");
        let entrada = |raiz: &str| norte_proto::Entry {
            path: vp(raiz).join(seg.clone()),
            kind: norte_proto::EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        };
        let row = CompareRow {
            left: Some(entrada("file:///a")),
            right: Some(entrada("file:///b")),
            ..fila_same(1)
        };
        // Solo el lado IZQUIERDO reinterpreta cp437.
        let pintado = row_text(&row, Some(norte_encoding::NameEncoding::Cp437), None);
        assert!(
            pintado.left.label.contains("CAFÉ.TXT"),
            "la izquierda usa la suya: {}",
            pintado.left.label
        );
        assert!(
            pintado.right.label.contains('\u{fffd}'),
            "y la derecha, que no tiene override, se queda con el lossy: {}",
            pintado.right.label
        );
    }

    /// Un nombre que el saneado tuvo que ALTERAR llega a la pantalla marcado
    /// con el badge que la GUI ya usa en el listado (`HOSTILE_BADGE`), no a
    /// pelo: cada fila de este panel es un nombre salido de un sistema de
    /// ficheros que el lector puede no controlar (regla 1, spec §6).
    #[test]
    fn un_nombre_hostil_llega_marcado() {
        let mut probados = 0_usize;
        for fixture in norte_testkit::corpus::hostile_names() {
            let Ok(seg) = norte_proto::Segment::new(fixture.bytes.clone()) else {
                continue;
            };
            let path = vp("file:///a").join(seg);
            let (_, hostil) = norte_frontend::display_name(&fixture.bytes);
            if !hostil {
                continue;
            }
            probados += 1;
            let row = CompareRow {
                left: Some(norte_proto::Entry {
                    path,
                    kind: norte_proto::EntryKind::File,
                    size: None,
                    mtime_ms: None,
                    attrs: std::collections::BTreeMap::new(),
                }),
                right: None,
                verdict: CompareVerdict::OnlyLeft,
                ..fila_same(1)
            };
            let pintado = row_text(&row, None, None);
            assert!(
                pintado.left.label.starts_with(crate::HOSTILE_BADGE),
                "{}: llegó sin marcar → {:?}",
                fixture.id,
                pintado.left.label
            );
        }
        assert!(
            probados > 0,
            "el corpus tiene que traer algún nombre hostil"
        );
    }

    /// El lado vacío de un huérfano va en BLANCO, jamás con un relleno
    /// inventado: la columna del centro ya dice `<`/`>`, y un guion en la
    /// cara ausente es lo que hace que un huérfano se lea como una pareja
    /// (mismo criterio que `compare_face_span` en la TUI).
    #[test]
    fn la_cara_ausente_de_un_huerfano_va_en_blanco() {
        let row = CompareRow {
            right: None,
            verdict: CompareVerdict::OnlyLeft,
            ..fila_same(1)
        };
        let pintado = row_text(&row, None, None);
        assert!(!pintado.left.label.is_empty());
        assert!(pintado.right.label.is_empty(), "sin relleno inventado");
        assert!(pintado.right.size.is_empty());
    }

    /// El pie sale de las claves `compare-status-*` COMPARTIDAS con la TUI
    /// (las dos lenguas ya las tienen), y `Incomplete` dice las dos cuentas:
    /// pintar «hecho» sobre una respuesta a la que le faltan lotes es
    /// exactamente el error que este panel no puede repetir.
    #[test]
    fn el_pie_dice_el_estado_con_las_claves_compartidas() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila_same(1)]);
        v.on_done(TaskId::new(1), &TaskState::Completed, 7);

        let pie = status_line(&v.run);
        assert!(
            !pie.contains("compare-status-"),
            "la clave tiene que resolver, no imprimirse: {pie}"
        );
        assert!(pie.contains('7'), "dice cuántas contó la task: {pie}");
        assert!(pie.contains('1'), "y cuántas llegaron: {pie}");
    }

    /// **La salida.** El primer `Esc` sobre una comparación viva la cancela y
    /// conserva sus filas; cualquier `Esc` posterior CIERRA, sin mirar el
    /// estado de la Task — condicionar el cierre a un estado terminal deja
    /// encerrado al lector cuando el canal no llega a cerrarse nunca (un
    /// daemon caído, una NFS muerta). Es la misma regla que la TUI escribió
    /// tras su review BLOCKER-1.
    #[test]
    fn el_primer_esc_cancela_y_el_segundo_cierra_pase_lo_que_pase() {
        assert_eq!(
            key_meaning("escape", false, true, false, false),
            Key::CancelTask
        );
        assert_eq!(key_meaning("escape", false, true, true, false), Key::Close);
        assert_eq!(
            key_meaning("escape", false, false, false, false),
            Key::Close
        );
    }

    /// #188: las dos teclas de sincronizar, y la que NO es.
    ///
    /// `SyncMode::Mirror` no tenía forma de pedirse desde esta frontend: el
    /// dispatch solo conocía `pane.sync-dirs` con `Update`, y ese comando ni
    /// siquiera llega mientras el panel de diferencias tiene el teclado. Con
    /// lo cual la mitad DESTRUCTIVA del spec §5.2 —el `DeleteTree`, el
    /// contador de borrados de la confirmación, `RelAnchor::Dest`— estaba
    /// escrita para un modo que la GUI no podía pedir.
    #[test]
    fn las_dos_teclas_de_sincronizar_del_panel() {
        use norte_proto::methods::SyncMode;
        assert_eq!(
            key_meaning("s", false, false, false, false),
            Key::Sync(SyncMode::Update)
        );
        assert_eq!(
            key_meaning("m", false, false, false, false),
            Key::Sync(SyncMode::Mirror)
        );
        // Con modificador, nada: `ctrl+s` es de la aplicación, no del panel.
        assert_eq!(key_meaning("s", true, false, false, false), Key::Ignore);
        // Y una letra cualquiera sigue sin significar nada aquí.
        assert_eq!(key_meaning("z", false, false, false, false), Key::Ignore);
    }

    /// #249: lo marcado ACOTA el plan, y lo no marcado es el árbol entero.
    ///
    /// El defecto era `include: None` incondicional en `start_sync`: bajo
    /// `Mirror`, cada huérfano del destino un `DeleteTree` y ninguna forma de
    /// reducirlo desde esta frontend. La confirmación daba la cuenta, que era
    /// lo único que lo salvaba.
    #[test]
    fn lo_marcado_acota_el_plan() {
        let src = vp("file:///a");
        let dst = vp("file:///b");
        let mut slot = None;
        let _ = open(&mut slot, arranque(1));
        route_rows(
            &mut slot,
            TaskId::new(1),
            vec![fila_same(1), fila_distinta(2)],
        );
        let view = slot.as_mut().expect("abierto");

        // Sin marcas: el campo AUSENTE, que en el wire es «los dos árboles
        // enteros». Jamás una lista vacía, que sería un plan de cero pasos.
        assert_eq!(super::include_from_marks(&view.run, &src, &dst), Ok(None));

        view.run.pane.toggle_mark(2);
        let include = super::include_from_marks(&view.run, &src, &dst)
            .expect("una marca dentro de las raíces")
            .expect("hay selección");
        assert_eq!(include.len(), 1, "solo la fila marcada: {include:?}");
    }

    /// #249: marcar filas, con la MISMA tecla que la TUI.
    ///
    /// Sin marcas, `include` iba siempre a `None` y el espejo de esta frontend
    /// era el árbol entero: cada huérfano del destino un `DeleteTree`, y
    /// ninguna forma de reducirlo desde aquí. Con `Update` era inerte; con
    /// `Mirror` es la diferencia entre «borra estos tres» y «borra todo lo que
    /// el origen no tiene».
    #[test]
    fn insert_marca_la_fila() {
        assert_eq!(key_meaning("insert", false, false, false, false), Key::Mark);
        // Con modificador no, como todo lo demás en este panel.
        assert_eq!(
            key_meaning("insert", true, false, false, false),
            Key::Ignore
        );
        // Y la repetición SÍ vale aquí: marcar es local, y marcar una tira de
        // filas seguidas es el gesto.
        assert_eq!(key_meaning("insert", false, false, false, true), Key::Mark);
    }

    /// #249: la repetición de teclado NO encola planes — la mitad de Wayland.
    ///
    /// La otra mitad, la que funciona también en X11 (donde `is_held` es
    /// siempre `false`), es `App::awaiting_sync_plan`: una petición en vuelo
    /// no deja salir la siguiente.
    ///
    /// Cada pulsación de `s`/`m` sube `sync_gen` y lanza un `sync.plan`
    /// nuevo, y un plan superado solo se cancela cuando llega su propio
    /// `SyncPlanStarted`, un viaje de ida y vuelta después: mantener `m`
    /// pulsada dos segundos contra un par SFTP arrancaba decenas de recorridos
    /// recursivos concurrentes antes de la primera cancelación. Son las dos
    /// únicas teclas de este panel que cuestan un viaje remoto.
    #[test]
    fn la_repeticion_no_encola_sincronizaciones_pero_si_mueve_el_cursor() {
        use norte_proto::methods::SyncMode;
        assert_eq!(key_meaning("s", false, false, false, true), Key::Ignore);
        assert_eq!(key_meaning("m", false, false, false, true), Key::Ignore);
        // La primera pulsación, la de verdad, sigue valiendo.
        assert_eq!(
            key_meaning("m", false, false, false, false),
            Key::Sync(SyncMode::Mirror)
        );
        // Y todo lo demás es LOCAL: bajar con la flecha pulsada es lo normal.
        assert_eq!(key_meaning("down", false, false, false, true), Key::Move(1));
        assert_eq!(key_meaning("escape", false, false, false, true), Key::Close);
    }

    /// Un modificador no pinta nada aquí: sin este filtro `alt+1` toggleaba
    /// un filtro y `ctrl+tab` cambiaba de lado.
    #[test]
    fn un_modificador_no_significa_nada_en_el_panel() {
        assert_eq!(key_meaning("1", true, false, false, false), Key::Ignore);
        assert_eq!(key_meaning("escape", true, true, false, false), Key::Ignore);
        assert_eq!(key_meaning("1", false, false, false, false), Key::Filter(0));
        assert_eq!(key_meaning("5", false, false, false, false), Key::Filter(4));
        assert_eq!(key_meaning("6", false, false, false, false), Key::Ignore);
    }

    fn arranque(task_id: u64) -> Started {
        Started {
            task_id: TaskId::new(task_id),
            left_root: vp("file:///a"),
            right_root: vp("file:///b"),
            left_pane: 0,
            left_encoding: None,
            right_encoding: None,
        }
    }

    /// **Regla 3.** Una comparación que sustituye a otra tiene que decir a
    /// quién hay que cancelar: dos flujos alimentando un panel serían dos
    /// comparaciones a la vez, y la vieja seguiría recorriendo dos árboles
    /// que ya nadie mira.
    #[test]
    fn abrir_un_panel_encima_de_otro_cancela_el_anterior() {
        let mut slot = None;
        assert_eq!(
            open(&mut slot, arranque(1)),
            None,
            "el primero no supera a nadie"
        );
        assert_eq!(
            open(&mut slot, arranque(2)),
            Some(TaskId::new(1)),
            "el segundo se lleva por delante al primero, y lo dice"
        );
        assert_eq!(slot.expect("el panel nuevo").task_id, TaskId::new(2));
    }

    /// **Una sola regla frente al id reciclado** (revisión de rama, MINOR-9):
    /// un lote que no tiene panel donde entrar se DESCARTA, y no manda
    /// cancelar. Esa Task ya recibió su `task.cancel` de quien soltó el panel
    /// —`close_compare` siempre cancela— y los `TaskId` son únicos por
    /// proceso del daemon, así que un segundo cancel tras un reinicio podría
    /// aterrizar sobre una copia en curso (revisión MINOR-3), que es el mismo
    /// riesgo por el que la rama vecina ya no cancelaba.
    #[test]
    fn un_lote_sin_panel_se_descarta_sin_cancelar_nada() {
        let mut slot = None;
        route_rows(&mut slot, TaskId::new(7), vec![fila(1)]);
        assert!(slot.is_none(), "sigue sin haber panel");
    }

    /// **La negativa de una petición SUPERADA no se pinta** (revisión de
    /// rama, MINOR-3): `CompareFailed` era el único evento de comparación sin
    /// `generation`, y su banner se escribía siempre — así que un rechazo
    /// viejo describía una petición que el lector ya había reemplazado. El
    /// `None` es además lo que retira su propio «comparando…».
    #[test]
    fn una_negativa_superada_no_dice_nada_y_retira_su_aviso() {
        use norte_proto::Error;
        assert_eq!(
            super::failed_banner(5, 4, &Error::PermissionDenied),
            None,
            "la generación 4 ya la superó la 5"
        );
        let frase = super::failed_banner(5, 5, &Error::PermissionDenied).expect("la vigente");
        // La CATEGORÍA localizada, jamás el `Display` inglés del error.
        assert!(
            frase.contains(&norte_frontend::error::error_category(
                &Error::PermissionDenied
            )),
            "«{frase}»"
        );
        assert!(!frase.contains("PermissionDenied"), "«{frase}»");
    }

    /// Un lote de la comparación ABIERTA entra.
    #[test]
    fn un_lote_del_panel_abierto_entra() {
        let mut slot = None;
        assert_eq!(open(&mut slot, arranque(1)), None);
        route_rows(&mut slot, TaskId::new(1), vec![fila(1)]);
        assert_eq!(slot.expect("el panel").run.pane.len(), 1);
    }

    /// Un lote de una comparación ANTERIOR se descarta, por lo mismo.
    #[test]
    fn un_lote_viejo_se_descarta_sin_cancelar_nada() {
        let mut slot = None;
        assert_eq!(open(&mut slot, arranque(2)), None);
        route_rows(&mut slot, TaskId::new(1), vec![fila(1)]);
        assert!(slot.expect("el panel").run.pane.is_empty());
    }

    /// **El filtro por `task_id` no es decoración.** Es lo único que dice que
    /// un lote pertenece a ESTA comparación y no a una que el usuario lanzó y
    /// canceló: los eventos de la anterior siguen en vuelo por el puente
    /// GPUI↔tokio cuando la nueva vista ya está abierta.
    #[test]
    fn un_lote_de_otra_comparacion_se_descarta() {
        let mut v = vista_de_prueba();
        v.on_rows(TaskId::new(1), vec![fila(1)]);

        assert!(
            !v.on_rows(TaskId::new(2), vec![fila(50), fila(51)]),
            "el lote es de otra comparación"
        );
        assert_eq!(v.run.pane.len(), 1, "no entró ni una fila ajena");

        assert!(
            !v.on_done(TaskId::new(2), &TaskState::Completed, 99),
            "y su final tampoco cierra esta vista"
        );
        assert_eq!(v.run.state, CompareState::Running);
        assert_eq!(v.run.rows_expected, 0, "ni le pega un contador ajeno");
    }
}
