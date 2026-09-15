//! Los diálogos vistos desde el despacho: qué comandos acepta cada modal
//! (los `ALLOW_*`), en qué se traduce un `dialog.*` ya resuelto y las dos
//! decisiones que no pasan por ahí (la ayuda y la confianza de `init.lua`).

use super::modal::{DialogOutcome, Modal};

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

/// ALLOWLIST de `Modal::ConfirmPluginUninstall` (ADR 0104): como
/// [`ALLOW_CONFIRM`] pero SIN `dialog.approve`. Es la tecla que el lector
/// acaba de pulsar en la lista para conceder capabilities, y sobre este
/// modal borra ficheros; en el host solo `confirm` es afirmativo, y el pie
/// del modal no debe ofrecer una tecla que aquí no significa nada.
pub const ALLOW_UNINSTALL: &[&str] = &["dialog.confirm", "dialog.deny", "dialog.cancel"];

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

/// ALLOWLIST de `Modal::AskSecret` (#325). Aquí `dialog.confirm` SÍ entra, al
/// revés que en los tres de arriba, y la diferencia es de qué se pregunta:
/// ellos piden un JUICIO sobre algo que el usuario no escribió —un
/// fingerprint, una op de agente, unas capabilities—, donde un Enter reflejo
/// aprueba sin haber mirado. Este pide un DATO que el usuario acaba de
/// teclear, y sobre el que ya decidió al escribirlo.
///
/// `dialog.approve` queda fuera por lo contrario: entregar una contraseña no
/// es aprobar nada, y ofrecer la tecla de aprobar aquí enseñaría que sirve
/// para eso.
///
/// Con el campo VACÍO, confirmar vuelve a ser inerte — el guard está dentro
/// de [`dialog_action`], que devuelve `None` y deja el diálogo abierto (mismo
/// mecanismo que un plan de lote no aplicable). Las
/// dos mitades importan: entregar la cadena vacía reproduciría lo que #320
/// cerró —un secreto vacío deja al provider tomando credenciales del
/// ambiente—, y cerrar obligaría a rehacer la navegación entera por un Enter
/// de más, que en un campo donde no se ve lo tecleado es el error fácil de
/// cometer. Cancelar sigue vivo: irse SÍ es una respuesta.
pub const ALLOW_ASK_SECRET: &[&str] = &["dialog.confirm", "dialog.cancel"];

/// ALLOWLIST de `Modal::ConfirmPluginApproval` (#280): mismo principio que
/// [`ALLOW_APPROVAL`] — conceder capabilities a una extensión es LA decisión
/// de seguridad de ese sistema, y `dialog.confirm` queda fuera a propósito:
/// Enter no concede permiso para leer los ficheros de nadie.
pub const ALLOW_PLUGIN_APPROVAL: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

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
    // Desinstalar (ADR 0104): el verbo que en la lista de favoritos quita
    // una entrada quita aquí la extensión entera, y por eso pregunta.
    "dialog.remove",
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

/// ALLOWLIST del sidebar de sitios (L3, `on_places_key` en main.rs).
///
/// El mismo vocabulario `dialog.*` que ya atan los siete presets: un panel que
/// se mueve con flechas y confirma con Enter no necesita idioma propio, y
/// dárselo habría sido siete presets tocados por una tecla nueva.
/// `toggle-enabled` pliega la sección, `cancel` devuelve el teclado a los
/// listados SIN cerrar el sidebar — cerrarlo es cosa de `layout.places`.
pub const ALLOW_PLACES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.toggle-enabled",
    "dialog.cancel",
    // Ancho: con el teclado DENTRO, `layout.grow`/`shrink` cambian el ancho
    // de ESTE panel. Es el único camino por el que se puede — el llamante de
    // `layout_resize` pasa siempre un listado visible (#244 M1).
    "layout.grow",
    "layout.shrink",
    // Su PROPIA tecla, que por eso está atada en `[global]`: sin ella el
    // sidebar se queda el `alt+b` y no puede cerrarse a sí mismo — abrías el
    // panel y la misma tecla dejaba de existir. Lo destapó pilotar la TUI en
    // tmux con la suite entera en verde, que es exactamente para lo que
    // sirve el harness.
    "layout.places",
    // `Tab` sale a los listados sin cerrar el panel. Abrir una columna
    // lateral dejaba muerta la tecla con la que se cambia de panel toda la
    // vida: el panel se come lo que no esté aquí.
    //
    // Los DOS verbos, y no uno: en la pantalla `dialog` los presets atan `tab`
    // a `dialog.pane` —que es como se llama «al otro panel» en un diálogo— y
    // `pane.switch` es como se llama en la de navegar. Aceptar solo el segundo
    // dejaba el arreglo sin efecto con los presets tal y como se envían.
    "dialog.pane",
    "pane.switch",
    // Y el anillo, que es lo que `Tab` NO hace: `pane.switch` devuelve el
    // teclado a los listados, mientras que esto pasa al panel de al lado sea
    // el que sea. Sin estas dos, la tecla del anillo se moría justo dentro
    // del panel del que sirve para salir.
    "layout.focus-next",
    "layout.focus-prev",
    // Y las teclas de los OTROS paneles, por la misma regla que ya trajo aquí
    // la del propio panel y la de cambiar de listado: abrir una columna
    // lateral no puede matar la tecla con la que se abre la de al lado. Antes,
    // con el teclado en el sidebar, `alt+j` no abría nada y no había forma de
    // saber por qué.
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "pane.tree",
    // Y el cromo de la APLICACIÓN, que no es de los listados: con el teclado
    // dentro del sidebar o del árbol, `alt+m` no abría la barra de menús y no
    // había forma de saber por qué — el panel se comía la tecla. Lo despacha
    // `App::panel_chrome_command`, uno para los tres.
    "app.menu",
    // Y salir, que es la tecla que peor puede morirse dentro de un panel: el
    // lector cerraba la terminal creyendo que había salido y el proceso
    // seguía vivo con el lock de la sesión. La atiende el cromo
    // (`App::panel_chrome_command`), honrando `[ui] confirm_quit`.
    "app.quit",
];

/// ALLOWLIST del panel de procesos (`on_processes_key` en main.rs).
///
/// El mismo vocabulario `dialog.*` del sidebar, por lo mismo: un panel que se
/// mueve con flechas y actúa con Enter no necesita idioma propio, y dárselo
/// serían siete presets tocados por una tecla nueva. `confirm` CANCELA la
/// tarea bajo el cursor —es la única acción que el protocolo tiene sobre una
/// task—, `cancel` devuelve el teclado a los listados sin cerrar el panel, y
/// `layout.processes` cierra desde dentro.
///
/// Son DOS pulsaciones y no tres: abrir este panel YA le da el teclado, así
/// que la siguiente cierra. La secuencia de tres —abrir, enfocar, cerrar— es
/// la del visor acoplado, y ahí es deliberada por un motivo que aquí no
/// aplica: el visor sigue al cursor del listado, así que darle el teclado al
/// abrirlo apagaría lo único que hace. Este panel no sigue a nada.
pub const ALLOW_PROCESSES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
    "layout.grow",
    "layout.shrink",
    "layout.processes",
    // Igual que el sidebar: `Tab` devuelve el teclado a los listados, con los
    // dos nombres que esa tecla tiene según la pantalla, y el anillo pasa al
    // panel de al lado.
    "dialog.pane",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    // Y las de los otros paneles, igual que en el sidebar.
    "layout.places",
    "layout.preview",
    "layout.metadata",
    "pane.tree",
    // Y el cromo de la aplicación, por lo mismo que en el sidebar.
    "app.menu",
    // Y salir, que es la tecla que peor puede morirse dentro de un panel: el
    // lector cerraba la terminal creyendo que había salido y el proceso
    // seguía vivo con el lock de la sesión. La atiende el cromo
    // (`App::panel_chrome_command`), honrando `[ui] confirm_quit`.
    "app.quit",
];

/// ALLOWLIST del panel de registro (#323).
///
/// Propia y NO la de procesos, aunque los dos paneles se parezcan: allí
/// `dialog.confirm` **cancela la tarea bajo el cursor**, y un `Enter` en un
/// visor de log que cancela una copia es exactamente la clase de accidente que
/// una allowlist existe para impedir. Aquí no hay nada que confirmar.
///
/// Tampoco lleva `dialog.up`/`down`: las flechas, las páginas, `Fin` y `Esc`
/// los reclama el propio panel antes del keymap ([`crate::logview::key`]),
/// porque son suyos mientras tenga el teclado.
///
/// Lo que sí lleva es el cromo: cerrar desde dentro con la misma tecla que
/// abrió, cambiar de panel, redimensionar y el menú. Sin `layout.log` en esta
/// lista, `alt+l` moría en el embudo y el panel no se podía cerrar con la
/// tecla que lo abría — que es como se descubrió que hacía falta esta lista.
pub const ALLOW_LOG: &[&str] = &[
    "layout.log",
    "layout.grow",
    "layout.shrink",
    "dialog.pane",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "pane.tree",
    "app.menu",
    // Y salir, que es la tecla que peor puede morirse dentro de un panel: el
    // lector cerraba la terminal creyendo que había salido y el proceso
    // seguía vivo con el lock de la sesión. La atiende el cromo
    // (`App::panel_chrome_command`), honrando `[ui] confirm_quit`.
    "app.quit",
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
    // Spec 2026-09-15 D2: abrir en el otro panel (cualquier lista que navega)
    // y vaciar (historia y populares; lo filtra el caller por kind).
    "dialog.confirm-other",
    "dialog.clear",
];

/// HINT del popup en modo HISTORIA o POPULARES (spec 2026-09-15 D2): quitar,
/// vaciar y abrir en el otro panel. Sin `add`: un favorito se crea desde su
/// propia lista.
pub const ALLOW_NAV_HISTORY: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.confirm-other",
    "dialog.remove",
    "dialog.clear",
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
// Tabla modal→desenlace, un brazo por variante y exhaustiva a propósito: es
// LA lista de qué acepta cada diálogo como respuesta, y verla entera de una
// vez es el punto. Partirla movería la frontera de la semántica de seguridad
// a un sitio arbitrario y haría más difícil ver que no falta ningún modal —
// mismo criterio, y misma excepción, que `modal_title_body`.
#[expect(
    clippy::too_many_lines,
    reason = "tabla modal→diálogo; mismo criterio que `modal_title_body`"
)]
#[must_use]
pub fn dialog_action(modal: &Modal, cmd: &str) -> Option<DialogOutcome> {
    use norte_proto::CollisionPolicy as P;
    match modal {
        // #139: las propiedades no PREGUNTAN nada — se leen y se cierran—, así
        // que solo entienden cancelar. Darle un «confirmar» a un cuadro de
        // solo lectura es enseñarle al lector que Enter hace algo aquí.
        Modal::Properties { .. } => (cmd == "dialog.cancel").then_some(DialogOutcome::Cancelled),
        // Las sumas tampoco preguntan nada: `confirm` COPIA la lista al
        // portapapeles —lo único que se puede hacer con ella— y `cancel`
        // cierra. No hay mutación que Enter deba proteger.
        Modal::Checksums { .. } => match cmd {
            "dialog.confirm" => Some(DialogOutcome::Confirmed),
            "dialog.cancel" => Some(DialogOutcome::Cancelled),
            _ => None,
        },
        // Conceder capabilities: `approve` concede y todo lo demás de la
        // lista cierra sin conceder — cerrar ES no conceder, fail-safe.
        Modal::ConfirmPluginApproval { .. } => {
            if !ALLOW_PLUGIN_APPROVAL.contains(&cmd) {
                return None;
            }
            Some(if cmd == "dialog.approve" {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled
            })
        }
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
        Modal::AiRenamePlan {
            plan,
            seen,
            entries,
            ..
        } => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            let confirms = matches!(cmd, "dialog.approve" | "dialog.confirm");
            // Y que el lector HAYA LLEGADO AL FINAL, que es la mitad que
            // faltaba aquí: se podía aprobar un plan de doscientos
            // renombrados habiendo visto los diez primeros. La regla vive en
            // el crate compartido porque la ventana ya la exigía, y una
            // aprobación con dos criterios distintos según la superficie es
            // la peor clase de divergencia (ADR 0077).
            if confirms && !norte_frontend::approval_ready(plan.confirmable(), *seen, entries.len())
            {
                return None;
            }
            Some(if confirms {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled // dialog.deny | dialog.cancel
            })
        }
        // Desinstalar una extensión es un borrado que pregunta, con la
        // semántica de borrar ficheros (Enter confirma) — salvo que aquí
        // `dialog.approve` no está en la lista: ver [`ALLOW_UNINSTALL`].
        Modal::ConfirmPluginUninstall { .. } => {
            if !ALLOW_UNINSTALL.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.confirm" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
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
        // #325: el ÚNICO modal de texto libre que pasa por aquí. Los demás
        // los intercepta el run loop entero (raw chars) y devuelven `None`;
        // este solo intercepta teclear y borrar, y deja Enter/Esc a esta
        // allowlist — porque su Enter es una DECISIÓN (entregar el secreto)
        // y su Esc abandona una navegación suspendida, que es justo lo que
        // `cancel_prompt` se niega a hacer.
        Modal::AskSecret { input, .. } => {
            if !ALLOW_ASK_SECRET.contains(&cmd) {
                return None;
            }
            // Campo vacío = confirmar INERTE (ver [`ALLOW_ASK_SECRET`]).
            if cmd == "dialog.confirm" && input.is_empty() {
                return None;
            }
            Some(if cmd == "dialog.confirm" {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled
            })
        }
        // #103 T9: `MarkPattern` es texto libre, como el diálogo de
        // búsqueda — el run loop lo intercepta ANTES de llegar aquí (raw
        // chars, jamás el contexto `dialog`), igual que `TrustLuaInit`.
        // Ambos devuelven `None` siempre.
        Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::ProfileSaveAs { .. }
        | Modal::EditNew { .. }
        | Modal::CommandLine { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::RenameBatchPattern { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferDest { .. }
        // #132: los dos de escribir archivos, por lo mismo.
        | Modal::Pack { .. }
        | Modal::Split { .. }
        // #314: el de permisos también es texto libre — se teclea un modo.
        | Modal::Chmod { .. }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::modal::quit_needs_confirm;
    use crate::app::testutil::*;
    use crate::app::trail::Trail;

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
}
