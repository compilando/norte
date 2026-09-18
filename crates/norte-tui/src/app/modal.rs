//! Los modales, sus límites de longitud y la decisión de si salir pide
//! confirmación.

use super::trail::Trail;
use norte_i18n::ta;
use norte_proto::VPath;

/// Lo tecleado en [`Modal::AskSecret`]: una contraseña a medio escribir.
///
/// Reexportado del crate COMPARTIDO desde #327, cuando la ventana necesitó el
/// mismo campo. Es un tipo de SEGURIDAD —`Debug` que redacta, buffer pisado al
/// soltarlo, capacidad reservada de antemano— y dos implementaciones son dos
/// sitios donde alguna de las tres garantías se olvida. Se queda el nombre
/// aquí para no tocar los treinta call sites de este frontend.
pub use norte_frontend::secret::TypedSecret;

/// Tipo de transferencia pendiente de confirmación/colisión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Copia (F5).
    Copy,
    /// Movimiento (F6).
    Move,
}

/// Qué lote de sumas pidió el despacho (#311).
///
/// Dos formas y no un `bool`: calcular parte de una SELECCIÓN y comprobar parte
/// de UN fichero — y ese hay que leerlo antes de poder pedir nada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecksumRequest {
    /// Calcular el digest de estas rutas y enseñarlo.
    Compute {
        /// Lo marcado, o el cursor: el operando de siempre.
        paths: Vec<VPath>,
    },
    /// Leer este fichero de sumas y comprobar lo que lista.
    Verify {
        /// El fichero de sumas. Los nombres se resuelven contra SU directorio.
        sums: VPath,
    },
}

/// Una fila del modal de sumas (#311).
///
/// El nombre va en BYTES: un fichero de sumas nombra ficheros, y un nombre no
/// tiene por qué ser texto (regla 1). Lo pinta el saneado de siempre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumRow {
    /// El nombre, tal como está en el disco.
    pub name: Vec<u8>,
    /// Su digest, o `None` si no se pudo calcular.
    pub digest: Option<String>,
    /// El veredicto contra lo publicado. `None` cuando solo se calculó.
    pub verdict: Option<norte_frontend::checksums::Verdict>,
}

/// Diálogo modal activo. Sus teclas resuelven contra el contexto `dialog`
/// del keymap (H1, issue #24 — CERRADO): el run loop pasa la tecla por el
/// [`Resolver`](crate::keymap::Resolver) del efectivo `dialog` y el comando
/// resultante se filtra por el ALLOWLIST del modal concreto
/// ([`crate::app::dialog_action`]) — la semántica de SEGURIDAD (qué confirma, qué
/// deniega, qué es inerte) vive en código, jamás en el keymap; solo la
/// ASIGNACIÓN de tecla→comando es rebindeable. Única excepción:
/// `Modal::TrustLuaInit`, que el run loop intercepta ANTES (necesita el
/// `LuaHost`) y resuelve con [`crate::app::trust_lua_key`] — decisión 8 del plan H1, no
/// migrado.
///
/// Sin `Eq` (M4-IA-2): [`Modal::SemanticHits`] arrastra el `score: f64` de
/// [`norte_proto::methods::SemanticHit`], que es solo `PartialEq` — como su
/// tipo de proto.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Las propiedades de la entrada bajo el cursor (#139).
    ///
    /// Lo que enseña sale del LISTADO, que ya lo tiene: nombre, clase, tamaño,
    /// fecha y los atributos que el provider haya reportado. Abrirlo no pide
    /// nada — salvo una cosa, y es justo la que un listado no puede saber: lo
    /// que ocupa una carpeta. Eso se cuenta, y mientras se cuenta el diálogo lo
    /// dice.
    Properties {
        /// La entrada, tal como está en el listado.
        entry: Box<norte_proto::Entry>,
        /// El recuento en marcha, si se lanzó uno (solo para directorios).
        size_task: Option<norte_proto::TaskId>,
        /// `(bytes, entradas)` cuando el recuento terminó.
        size: Option<(u64, u64)>,
    },
    /// Conceder capabilities a una extensión (#280).
    ///
    /// Es LA decisión de seguridad del sistema de extensiones: lo que se
    /// concede es leer ficheros, correr programas o salir a la red en nombre
    /// del usuario. Aquí se aprobaba con una tecla y sin enumerar nada,
    /// mientras la ventana gráfica ya preguntaba. REVOCAR no pasa por aquí:
    /// va en la dirección segura.
    ConfirmPluginApproval {
        /// El id de la extensión, tal como el core la nombra.
        id: String,
        /// Su nombre, ya saneado para pintar.
        name: String,
        /// El nombre difiere del real y hay que marcarlo.
        name_hostile: bool,
        /// Las capabilities que se conceden, cada una enmascarada por su
        /// cuenta y con su bandera: pegarlas en una frase dejaría que una
        /// finja ser otra.
        caps: Vec<(String, bool)>,
        /// El ancla del manifiesto que se ESTÁ ENSEÑANDO (#282), si el core la
        /// manda. Viaja con el sí, y el core rehúsa si el `plugin.toml` cambió
        /// entre la pregunta y la respuesta.
        digest: Option<String>,
    },
    /// Confirmación de DESINSTALAR una extensión (ADR 0104): borra sus
    /// ficheros y retira su consentimiento, y no tiene vuelta —no hay
    /// `plugin.install` por el wire—. Pregunta por eso, y el cuerpo dice las
    /// dos cosas que se pierden: «¿desinstalar?» a secas se lee como
    /// «¿apagar del todo?».
    ConfirmPluginUninstall {
        /// El id de la extensión, tal como el core la nombra.
        id: String,
        /// Su nombre, ya saneado para pintar.
        name: String,
        /// El nombre difiere del real y hay que marcarlo.
        name_hostile: bool,
    },
    /// Revisión de un plan de ORGANIZAR (fase 8, `ai.organize_plan` /
    /// `plugin.organize_plan`).
    ///
    /// Se revisa como un ÁRBOL y no como una lista de parejas, que es la
    /// diferencia con [`Self::AiRenamePlan`]: lo que cambia es la forma del
    /// directorio, y cuarenta filas `a.pdf → facturas/2026/a.pdf` no dejan
    /// ver ni cuántas carpetas aparecen ni qué acaba en cada una.
    ///
    /// Lleva la MISMA disciplina de aprobación que el plan de renombrar:
    /// hasta que el lector no ha llegado al final, confirmar está mudo. Un
    /// plan de doscientos movimientos aprobado habiendo visto diez no es un
    /// plan revisado.
    OrganizePlan {
        /// Directorio sobre el que se aplica.
        dir: VPath,
        /// Los movimientos, tal cual los propuso el productor.
        moves: Vec<norte_proto::methods::OrganizeMove>,
        /// El árbol ya calculado, que es lo que se pinta.
        lines: Vec<norte_frontend::organize::TreeLine>,
        /// El token del plan que se revisó: lo único que `fs.organize`
        /// acepta.
        plan_hash: norte_proto::methods::PlanHash,
        /// Primera línea visible de la ventana.
        offset: usize,
        /// Hasta dónde ha llegado el lector alguna vez. Marca de agua ALTA y
        /// no la posición actual: volver arriba no deshace haber leído.
        seen: usize,
    },
    /// Confirmación de DESHACER hasta un punto de la línea de tiempo (fase
    /// 7, `journal.undo_after`).
    ///
    /// Pregunta porque revierte trabajo, y el cuerpo lleva el RECUENTO: lo
    /// que se va a deshacer, lo que se va a saltar y lo que no es del lector
    /// — tres números que no se suman, porque prometer uno solo sería
    /// prometer algo que no va a pasar. La regla de esta pantalla es que una
    /// confirmación que no dice cuánto no es una confirmación.
    ConfirmUndoAfter {
        /// El corte: se deshace lo del humano POSTERIOR a este `seq`, y la
        /// entrada que lo nombra se queda.
        seq: i64,
        /// Cuántas entradas se van a intentar deshacer.
        a_deshacer: usize,
        /// Cuántas se van a saltar (sin vuelta, ya deshechas, o
        /// compensaciones).
        irreversibles: usize,
        /// Cuántas hay por encima del corte que NO son del lector, y que por
        /// tanto este undo no toca.
        ajenas: usize,
    },
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
        /// El aviso de espacio, cuando lo hay (#149): lo escribe el run loop
        /// —preguntar por los volúmenes es I/O— y lo pinta este modal.
        ///
        /// `None` es el caso NORMAL, y significa las tres cosas honestas a la
        /// vez: cabe, o el destino no sabe decir cuánto le queda, o no se sabe
        /// cuánto se va a mover. Ninguna de las tres se anuncia.
        space: Option<String>,
        /// El aviso de confinamiento, cuando lo hay (#164): igual que
        /// [`Modal::ConfirmTransfer::space`], lo escribe el run loop y lo pinta
        /// este modal.
        ///
        /// `None` = este destino sabe sujetar sus escrituras, que es el caso
        /// normal en Linux y macOS y no se anuncia.
        confine: Option<String>,
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
    /// La conexión `conn` declara `secret = "prompt"` y ninguna de las tres
    /// fuentes de siempre lo tiene (#325): un `Error::SecretNeeded` al navegar
    /// a `dir`. Se teclea la contraseña, Enter la entrega
    /// (`connection.provide_secret`) y REINTENTA la navegación; Esc cancela.
    ///
    /// El molde es [`Modal::TrustHostKey`] —transporta `dir`/`pane`/`trail`
    /// por las mismas razones, y quien responde rebobina el rastro— con dos
    /// diferencias que vienen de que aquí se ESCRIBE un secreto:
    ///
    /// * Enter SÍ confirma. En el TOFU no, porque confirmar es una decisión
    ///   de seguridad que no debe dispararse sola; aquí Enter sobre un campo
    ///   vacío no entrega nada (no hay decisión que disparar), y sobre un
    ///   campo escrito es lo que el dedo del usuario ya iba a hacer.
    /// * Lo tecleado NO se pinta: el diálogo dibuja un punto por carácter.
    AskSecret {
        /// Nombre de la entrada de `connections.toml` que pide el secreto —
        /// la MISMA cadena que va en `connection.provide_secret`. Sale del
        /// error del core, no del servidor remoto.
        conn: String,
        /// A dónde se conecta, `scheme://host[:puerto]` y ya redactado por el
        /// core (sin userinfo). Solo para MOSTRAR, jamás se reparsea — pero
        /// obligatorio: sin él la pregunta no es contestable, porque el
        /// nombre de arriba lo eligió un fichero que puede haberse editado.
        endpoint: String,
        /// Lo tecleado hasta ahora. [`TypedSecret`] y no `String`: ni se
        /// imprime en un `Debug` ni se queda en el heap tras el drop.
        input: TypedSecret,
        /// La ruta remota a la que reintentar navegar tras entregarlo.
        dir: VPath,
        /// El pane que estaba navegando (ver [`Modal::TrustHostKey::pane`]).
        pane: usize,
        /// El sentido del rastro de esa navegación (ver
        /// [`Modal::TrustHostKey::trail`]).
        trail: Trail,
    },
    /// TOFU del `./.norte/init.lua` de PROYECTO (M4 Lua, ADR 0026): un repo
    /// AJENO trae un script que correría con los permisos del usuario —
    /// primer contacto pregunta. `y` confía y evalúa, `n`/Esc deniegan
    /// (persistido por (path, hash) hasta que el fichero cambie); Enter NO
    /// aprueba (decisión de seguridad, mismo principio que
    /// [`Modal::ApproveAgentOp`]). Los BYTES aprobados viven en
    /// [`crate::app::App::lua_pending_trust`] (anti-TOCTOU: lo aprobado = lo evaluado).
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
    /// [`crate::app::ALLOW_CONFIRM`]/`DialogHints::confirm` sin necesitar los suyos
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
        /// «No cabe» (#149), o `None` si cabe o no se sabe cuánto ocupa.
        ///
        /// Las MISMAS dos líneas que [`Self::ConfirmTransfer`], y por eso
        /// están aquí: este diálogo es el que sale al copiar UN fichero, y
        /// repartir los avisos por número de ítems hacía que copiar uno suelto
        /// no dijera nada (#343). El silencio del espacio significa «cabe o no
        /// lo sé»; el del confinamiento significa «este destino SUJETA sus
        /// escrituras», que es una afirmación, no una ausencia.
        space: Option<String>,
        /// «Este destino no puede confinar las escrituras» (#164, #219).
        confine: Option<String>,
    },
    /// Destino TECLEADO de una transferencia: F5/F6 cuando no hay «el otro
    /// panel» al que copiar.
    ///
    /// Con un solo listado —el preset `simple`— el rol `target` no tiene
    /// candidato, y la regla de L1 para eso es que la operación PREGUNTA en
    /// vez de fallar. Se teclea la dirección en su forma wire (la misma que
    /// escribes en `[[hotlist]]`), prellenada con la del propio panel: lo
    /// normal es editarle la cola, no escribirla entera.
    ///
    /// Texto libre como [`Modal::Mkdir`], y por el mismo motivo: lo tecleado
    /// se enmascara al pintarlo. Confirmar NO transfiere — abre el modal que
    /// habría abierto un F5 con dos paneles, que es donde vive la
    /// confirmación.
    TransferDest {
        /// Copy o Move.
        kind: TransferKind,
        /// Lo tecleado hasta ahora, en forma wire.
        input: String,
        /// Diagnóstico del último intento inválido, bajo el campo.
        error: Option<String>,
    },
    /// Empaquetar (#132). Texto libre: el NOMBRE del archivo que se va a
    /// crear, prellenado con el del directorio o la entrada de partida más la
    /// extensión de zip.
    ///
    /// El formato sale del nombre y se enseña en el propio diálogo: lo que
    /// viaja por el wire es la decisión ya tomada, no un nombre para que el
    /// servidor adivine (ver `ARCHIVE_PACK`).
    Pack {
        /// Lo tecleado hasta ahora.
        name: String,
        /// Diagnóstico del último intento inválido.
        error: Option<String>,
    },
    /// Cambiar los PERMISOS POSIX (#314). Texto libre: el modo en octal,
    /// prellenado con el que tiene lo que hay bajo el cursor.
    ///
    /// En octal y no con casillas `rwx` porque es lo que teclea quien sabe lo
    /// que quiere —`755`, `600`— y porque es la forma que el propio listado
    /// enseña. Un editor de casillas es otra superficie, y esta no la impide.
    Chmod {
        /// Lo tecleado hasta ahora.
        mode: String,
        /// Sobre qué se va a aplicar, resuelto al ABRIR: lo marcado, o lo que
        /// hay bajo el cursor. Se congela aquí porque entre abrir el diálogo y
        /// confirmarlo el listado puede refrescarse, y entonces «lo marcado»
        /// sería otra cosa.
        targets: Vec<VPath>,
        /// Diagnóstico del último intento inválido, bajo el campo.
        error: Option<String>,
    },
    /// Partir un fichero (#132). Texto libre: el tamaño de cada trozo, con
    /// sufijo (`10M`, `700M`, `4096`).
    Split {
        /// Lo tecleado hasta ahora.
        size: String,
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
    /// Guardar lo que hay en pantalla como un PERFIL nuevo (#306, ADR 0079).
    ///
    /// Mismo molde que [`Modal::Mkdir`]: un campo y un diagnóstico. Lo que se
    /// teclea es el nombre del perfil, que acaba siendo un DIRECTORIO
    /// (`profiles/<nombre>/`), así que pasa por `valid_profile_name` antes de
    /// tocar disco y el error se pinta bajo el campo en vez de rechazarse en
    /// silencio.
    ProfileSaveAs {
        /// Lo tecleado hasta ahora.
        name: String,
        /// Diagnóstico del último intento inválido.
        error: Option<String>,
    },
    /// Crear un fichero VACÍO (Shift+F4, #290). Mismo molde que
    /// [`Modal::Mkdir`] con la otra clase de nodo, y por el mismo motivo: el
    /// fichero lo crea el DAEMON (`fs.create`) y no el editor, así que hace
    /// falta un nombre antes de lanzar nada.
    ///
    /// El editor se abre DESPUÉS, sobre el fichero que ya existe. Dejárselo
    /// crear a él —lo que hacía esta tecla— saltaba el journal y la política:
    /// un `pane.edit-new` sobre un directorio donde la política prohíbe
    /// escribir creaba el fichero igualmente.
    EditNew {
        /// El directorio donde se crea, ATADO al abrir el modal.
        ///
        /// No se vuelve a preguntar al pane al confirmar, y es la misma
        /// decisión que toma la ventana (`Pendiente::CrearFichero { dir }`):
        /// entre abrir el diálogo y confirmarlo, el sitio bajo el pane puede
        /// haber cambiado, y crear en «donde esté el foco ahora» crea en un
        /// directorio que el lector no estaba mirando cuando tecleó el nombre.
        dir: VPath,
        /// Lo tecleado hasta ahora.
        name: String,
        /// Diagnóstico del último intento inválido.
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
    /// Las sumas de un lote, ya calculadas (#311): superficie de LECTURA.
    ///
    /// Dos caras del mismo modal, y por eso no son dos: calcular enseña la
    /// suma de cada fichero, y comprobar enseña además el veredicto contra lo
    /// que el fichero de sumas publicaba. La lista es la misma cosa.
    ///
    /// Confirmar COPIA las líneas al portapapeles en el formato que
    /// `sha256sum -c` lee; cancelar cierra. No hay nada que aplicar: aquí no
    /// se muta nada.
    Checksums {
        /// Qué se hizo: calcular o comprobar (clave Fluent del título).
        title_key: &'static str,
        /// Una fila por ruta, en el orden en que se pidieron.
        rows: Vec<ChecksumRow>,
        /// Primera fila visible: la lista se recorre entera, y sin esto la
        /// cola de un lote grande no se podría ver.
        offset: usize,
    },
    /// Prompt de la PLANTILLA del renombrado en lote (#310). Texto libre,
    /// molde [`Modal::AiRenameInstruction`] — y hermano suyo por diseño: los
    /// dos producen el MISMO plan revisable, y lo único que cambia es quién
    /// propone los nombres, un modelo o una plantilla que escribe el humano.
    RenameBatchPattern {
        /// La plantilla tecleada hasta ahora (`[N]`, `[E]`, `[C]`).
        pattern: String,
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
    /// Plan de rename revisable: superficie de DECISIÓN. Confirmar aplica
    /// (contenido revisado por el humano); Esc/cancel descarta.
    ///
    /// El nombre dice `Ai` por su origen (M4-IA) y ya no es solo suyo: desde
    /// #310 lo comparte el lote por PLANTILLA, que produce el mismo plan por
    /// el mismo camino. Lo que hace segura la operación no es de dónde
    /// salieron los nombres, así que la revisión es una y no dos.
    AiRenamePlan {
        /// Dir sobre el que se aplican los renames.
        dir: VPath,
        /// Parejas from→to del modelo (proto, UTF-8 garantizado).
        entries: Vec<norte_proto::methods::AiRenameEntry>,
        /// Primera pareja visible de la ventana (audit MAJOR-3): el plan
        /// ENTERO es revisable por scroll ([`crate::app::App::ai_plan_scroll`]) — sin
        /// esto, la cola de un plan > [`crate::app::AI_RENAME_PAIR_LIMIT`] se aplicaba
        /// sin poder verse.
        offset: usize,
        /// Hasta dónde ha llegado el lector alguna vez.
        ///
        /// Marca de agua ALTA y no la posición actual: volver arriba no
        /// des-lee lo que ya se leyó. Sin esto se podía aprobar un plan de
        /// doscientos renombrados habiendo visto los diez primeros, y los que
        /// importan pueden estar en la fila ciento ochenta. La ventana lo
        /// exigía y el terminal no: la misma pregunta con dos respuestas, en
        /// la superficie donde más caro sale (ADR 0077).
        seen: usize,
        /// El plan del LOTE que contestó `fs.rename_batch_plan` (spec §17,
        /// ADR 0042): veredictos, si es aplicable y el `plan_hash` que hay
        /// que devolver para ejecutar EXACTAMENTE lo que se enseñó.
        ///
        /// Nace [`norte_frontend::BatchPlan::Pending`] —el modal abre y se
        /// rellena cuando el core contesta— y sin un plan APLICABLE
        /// confirmar está DESHABILITADO ([`crate::app::dialog_action`]): no hay hash
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

/// Tope de caracteres de un campo de TEXTO de esta pantalla: el patrón de
/// [`Modal::MarkPattern`], un nombre, una instrucción, una plantilla.
///
/// En `chars()`, no bytes — igual criterio que
/// [`crate::app::DETAIL_MAX_CHARS`], un carácter multibyte cuenta una vez.
///
/// Se llamaba `TEXT_FIELD_MAX_CHARS` porque nació con el patrón de marcado
/// (#103), y para cuando lo compartían nueve modales el nombre decía de dónde
/// venía en vez de qué mide (#121).
pub const TEXT_FIELD_MAX_CHARS: usize = 256;

/// El tope de una contraseña es el mismo, y ahora se COMPRUEBA (#327).
///
/// Desde que `TypedSecret` vive en el crate compartido son dos constantes en
/// dos crates, y su rustdoc afirma que valen lo mismo. Una afirmación así, sin
/// nada que la ate, dura hasta que alguien mueve una: entonces el campo de
/// contraseña de la TUI frena a una longitud y el de la ventana a otra, y
/// ninguna prueba lo dice.
const _: () = assert!(
    TEXT_FIELD_MAX_CHARS == norte_frontend::secret::SECRET_MAX_CHARS,
    "el tope de un campo de texto y el de una contraseña se separaron"
);

/// Borra el último CARÁCTER de un texto en forma WIRE.
///
/// Un carácter puede ser hasta cuatro bytes y cada byte no ASCII viaja como
/// `%XX`, así que «borrar un carácter» son entre uno y doce caracteres del
/// texto. Se quitan los escapes de continuación (`%80`–`%BF`) y luego el de
/// cabeza; lo que no es un escape se borra como siempre.
pub(crate) fn pop_wire_char(s: &mut String) {
    /// El byte de un `%XX` al final, si lo hay.
    fn escape_final(s: &str) -> Option<u8> {
        let tail = s.get(s.len().checked_sub(3)?..)?;
        let rest = tail.strip_prefix('%')?;
        u8::from_str_radix(rest, 16).ok().filter(|_| {
            // `from_str_radix` acepta `+7f` y espacios; aquí solo hex.
            rest.len() == 2 && rest.bytes().all(|b| b.is_ascii_hexdigit())
        })
    }

    // Un carácter UTF-8 son como mucho cuatro bytes: tres continuaciones.
    for _ in 0..3 {
        match escape_final(s) {
            Some(b) if (0x80..=0xBF).contains(&b) => {
                s.truncate(s.len() - 3);
            }
            Some(_) => {
                s.truncate(s.len() - 3);
                return;
            }
            None => {
                s.pop();
                return;
            }
        }
    }
    // Solo continuaciones: la de cabeza, si está, se va con ellas.
    if escape_final(s).is_some() {
        s.truncate(s.len() - 3);
    }
}

/// Tope de caracteres del destino de [`Modal::TransferDest`].
///
/// APARTE de [`TEXT_FIELD_MAX_CHARS`] y mucho mayor, porque lo que se mide
/// aquí NO es un patrón sino una dirección en forma WIRE, que va
/// porcentualmente codificada: un byte inválido cuesta tres caracteres, así
/// que la fixture `name_max_255_invalid_tail` ocupa 765 en UN solo segmento y
/// un directorio hondo pasa de 256 él solo. Con el tope de los patrones, el
/// prompt podía ABRIR ya por encima del límite y entonces cada tecla era un
/// no-op mudo (#246 M3).
pub const TRANSFER_DEST_MAX_CHARS: usize = 8192;

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

/// Cuál de los diez prompts de texto libre está abierto.
///
/// Los métodos con nombre propio (`mkdir_push`, `pack_set_error`…) siguen
/// existiendo porque son lo que nombran las tablas de despacho; lo que
/// comparten es UNA implementación, y esta es la etiqueta con la que cada
/// uno dice de qué prompt habla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// [`Modal::MarkPattern`].
    MarkPattern,
    /// [`Modal::TransferName`].
    TransferName,
    /// [`Modal::TransferDest`].
    TransferDest,
    /// [`Modal::Pack`].
    Pack,
    /// [`Modal::Split`].
    Split,
    /// [`Modal::Chmod`].
    Chmod,
    /// [`Modal::Mkdir`].
    Mkdir,
    /// [`Modal::EditNew`].
    EditNew,
    /// [`Modal::ProfileSaveAs`].
    ProfileSaveAs,
    /// [`Modal::CommandLine`].
    CommandLine,
    /// [`Modal::AiRenameInstruction`].
    AiRename,
    /// [`Modal::RenameBatchPattern`].
    RenameBatch,
    /// [`Modal::SemanticQuery`].
    Semantic,
}

/// Qué hace un prompt cuando lo tecleado llega al tope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverLimit {
    /// Frena y calla: el campo se ve lleno y el usuario lo ve.
    Silent,
    /// Lo DICE (`modal-command-line-too-long`): en un prompt que puede ABRIR
    /// ya largo —una dirección wire, una línea de comandos— frenar en mudo
    /// convierte cada tecla en un no-op sin explicación (#246 M3).
    Say,
}

/// Qué borra el retroceso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopMode {
    /// Un carácter del texto.
    Char,
    /// Un carácter del NOMBRE, escape porcentual entero incluido
    /// ([`pop_wire_char`]).
    WireChar,
}

/// El campo de texto de un prompt abierto, con la política que lo gobierna.
///
/// Se pide a [`Modal::text_prompt`] y se consume en una operación: es un
/// préstamo mutable del modal, no un estado que se guarde.
pub struct TextPrompt<'a> {
    text: &'a mut String,
    error: &'a mut Option<String>,
    touched: Option<&'a mut bool>,
    limit: usize,
    over_limit: OverLimit,
    pop: PopMode,
}

impl TextPrompt<'_> {
    /// Lo tecleado hasta ahora.
    #[must_use]
    pub fn text(&self) -> &str {
        self.text
    }

    /// El diagnóstico que se pinta bajo el campo, si lo hay.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Añade un carácter. En el tope, frena — y lo dice o no según el prompt.
    pub fn push(self, c: char) {
        if self.text.chars().count() >= self.limit {
            if self.over_limit == OverLimit::Say {
                *self.error = Some(ta(
                    "modal-command-line-too-long",
                    &[("max", &self.limit.to_string())],
                ));
            }
            return;
        }
        self.text.push(c);
        if let Some(touched) = self.touched {
            *touched = true;
        }
        *self.error = None;
    }

    /// Borra hacia atrás.
    ///
    /// El diagnóstico se va aunque no haya nada que borrar: quien pulsa
    /// retroceso está corrigiendo, y el aviso del intento anterior ya no
    /// describe lo que hay. El `touched` del nombre editable NO: ese solo se
    /// fija si de verdad borró algo (#105 review MINOR-5, un pop vacío no
    /// debe estrechar la vía de los bytes originales).
    pub fn pop(self) {
        let borro = match self.pop {
            PopMode::Char => self.text.pop().is_some(),
            PopMode::WireChar => {
                let antes = self.text.len();
                pop_wire_char(self.text);
                self.text.len() != antes
            }
        };
        if borro && let Some(touched) = self.touched {
            *touched = true;
        }
        *self.error = None;
    }

    /// Deja el diagnóstico y CONSERVA lo tecleado: un submit que falla se
    /// corrige y se reintenta, no se vuelve a escribir.
    pub fn set_error(self, msg: String) {
        *self.error = Some(msg);
    }
}

impl Modal {
    /// Qué prompt de texto libre es este modal, o `None` si es de DECISIÓN.
    ///
    /// La frontera importa: un modal de decisión jamás se cierra por la vía
    /// de los prompts (`cancel_*`), tiene que denegar por `on_dialog_key`.
    #[must_use]
    pub const fn prompt_kind(&self) -> Option<PromptKind> {
        Some(match self {
            Self::MarkPattern { .. } => PromptKind::MarkPattern,
            Self::TransferName { .. } => PromptKind::TransferName,
            Self::TransferDest { .. } => PromptKind::TransferDest,
            Self::Pack { .. } => PromptKind::Pack,
            Self::Split { .. } => PromptKind::Split,
            Self::Chmod { .. } => PromptKind::Chmod,
            Self::Mkdir { .. } => PromptKind::Mkdir,
            Self::EditNew { .. } => PromptKind::EditNew,
            Self::ProfileSaveAs { .. } => PromptKind::ProfileSaveAs,
            Self::CommandLine { .. } => PromptKind::CommandLine,
            Self::AiRenameInstruction { .. } => PromptKind::AiRename,
            Self::RenameBatchPattern { .. } => PromptKind::RenameBatch,
            Self::SemanticQuery { .. } => PromptKind::Semantic,
            _ => return None,
        })
    }

    /// El campo de texto de este modal y su política, o `None` si no es un
    /// prompt.
    ///
    /// Aquí está, en un solo sitio, TODO lo que distingue a los diez: cómo
    /// se llama el campo, cuánto admite, si el tope se dice, qué borra el
    /// retroceso y si hay un `touched` que fijar.
    pub fn text_prompt(&mut self) -> Option<TextPrompt<'_>> {
        let (text, error, touched, limit, over_limit, pop) = match self {
            // La plantilla del lote comparte molde con el patrón de marcado:
            // texto libre, mismo tope y mismo borrado.
            Self::MarkPattern { pattern, error, .. }
            | Self::RenameBatchPattern { pattern, error } => (
                pattern,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::TransferName {
                name,
                touched,
                error,
                ..
            } => (
                name,
                error,
                Some(touched),
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::TransferDest { input, error, .. } => (
                input,
                error,
                None,
                TRANSFER_DEST_MAX_CHARS,
                OverLimit::Say,
                PopMode::WireChar,
            ),
            Self::Pack { name, error }
            | Self::Mkdir { name, error }
            | Self::ProfileSaveAs { name, error }
            | Self::EditNew { name, error, .. } => (
                name,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::Split { size, error } => (
                size,
                error,
                None,
                SPLIT_SIZE_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            // #314: cuatro dígitos octales y ni uno más. El tope frena y
            // calla, que es lo que un campo lleno ya dice por sí solo.
            Self::Chmod { mode, error, .. } => (
                mode,
                error,
                None,
                CHMOD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::CommandLine { command, error } => (
                command,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Say,
                PopMode::Char,
            ),
            Self::AiRenameInstruction { instruction, error } => (
                instruction,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            Self::SemanticQuery { query, error } => (
                query,
                error,
                None,
                TEXT_FIELD_MAX_CHARS,
                OverLimit::Silent,
                PopMode::Char,
            ),
            _ => return None,
        };
        Some(TextPrompt {
            text,
            error,
            touched,
            limit,
            over_limit,
            pop,
        })
    }
}

/// Tope de caracteres del tamaño de trozo de [`Modal::Split`]: corto a
/// propósito, porque lo que cabe ahí es `700M`, no una frase.
pub const SPLIT_SIZE_MAX_CHARS: usize = 32;

/// Tope del campo de [`Modal::Chmod`] (#314): cuatro dígitos octales — los
/// tres de siempre más el de setuid/setgid/sticky.
pub const CHMOD_MAX_CHARS: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn mark_pattern() -> Modal {
        Modal::MarkPattern {
            mark: true,
            pattern: String::new(),
            error: None,
        }
    }

    fn transfer_name() -> Modal {
        Modal::TransferName {
            kind: TransferKind::Move,
            from: VPath::root(norte_proto::Scheme::new("mem").unwrap(), None),
            to_dir: VPath::root(norte_proto::Scheme::new("mem").unwrap(), None),
            name: String::from("ab"),
            original: b"ab".to_vec(),
            touched: false,
            from_marks: false,
            enc: None,
            error: None,
            space: None,
            confine: None,
        }
    }

    fn los_diez() -> Vec<(PromptKind, Modal)> {
        vec![
            (PromptKind::MarkPattern, mark_pattern()),
            (PromptKind::TransferName, transfer_name()),
            (
                PromptKind::TransferDest,
                Modal::TransferDest {
                    kind: TransferKind::Copy,
                    input: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Pack,
                Modal::Pack {
                    name: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Split,
                Modal::Split {
                    size: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Chmod,
                Modal::Chmod {
                    mode: String::new(),
                    targets: Vec::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Mkdir,
                Modal::Mkdir {
                    name: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::EditNew,
                Modal::EditNew {
                    dir: VPath::parse("mem:///").unwrap_or_else(|_| unreachable!("wire de test")),
                    name: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::CommandLine,
                Modal::CommandLine {
                    command: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::AiRename,
                Modal::AiRenameInstruction {
                    instruction: String::new(),
                    error: None,
                },
            ),
            (
                PromptKind::Semantic,
                Modal::SemanticQuery {
                    query: String::new(),
                    error: None,
                },
            ),
        ]
    }

    /// Los diez prompts de texto se dicen prompts, y teclear llega al campo
    /// que cada uno llama de otra manera.
    #[test]
    fn los_diez_prompts_exponen_su_campo() {
        for (kind, mut m) in los_diez() {
            assert_eq!(m.prompt_kind(), Some(kind), "{kind:?} no se dice prompt");
            m.text_prompt().expect("campo de texto").push('x');
            let tp = m.text_prompt().expect("campo de texto");
            assert!(tp.text().ends_with('x'), "{kind:?} no recibió la tecla");
        }
    }

    /// Un modal de DECISIÓN no tiene campo que teclear.
    #[test]
    fn un_modal_de_decision_no_es_prompt() {
        let mut m = Modal::ConfirmQuit;
        assert_eq!(m.prompt_kind(), None);
        assert!(m.text_prompt().is_none());
    }

    /// El tope es por prompt, y el de partir es el corto.
    #[test]
    fn el_tope_de_partir_para_en_silencio() {
        let mut m = Modal::Split {
            size: "9".repeat(32),
            error: None,
        };
        m.text_prompt().expect("campo").push('9');
        let tp = m.text_prompt().expect("campo");
        assert_eq!(tp.text().chars().count(), 32, "el tope no frenó");
        assert!(tp.error().is_none(), "el tope de partir es mudo");
    }

    /// El de la línea de comandos SÍ lo dice (#246 M3).
    #[test]
    fn el_tope_de_la_linea_de_comandos_se_dice() {
        let mut m = Modal::CommandLine {
            command: "x".repeat(TEXT_FIELD_MAX_CHARS),
            error: None,
        };
        m.text_prompt().expect("campo").push('y');
        let tp = m.text_prompt().expect("campo");
        assert_eq!(tp.text().chars().count(), TEXT_FIELD_MAX_CHARS);
        assert!(tp.error().is_some(), "el tope de la línea se dice");
    }

    /// El retroceso del destino borra el ESCAPE entero, no un carácter del
    /// texto wire (#246 M3).
    #[test]
    fn el_retroceso_del_destino_borra_un_escape_entero() {
        let mut m = Modal::TransferDest {
            kind: TransferKind::Copy,
            input: String::from("mem:///caf%C3%A9"),
            error: None,
        };
        m.text_prompt().expect("campo").pop();
        let tp = m.text_prompt().expect("campo");
        assert_eq!(tp.text(), "mem:///caf");
    }

    /// El nombre editable marca `touched` solo si el retroceso borró algo
    /// (#105 review MINOR-5).
    #[test]
    fn el_nombre_editable_marca_touched_solo_si_borro() {
        let mut m = transfer_name();
        m.text_prompt().expect("campo").pop();
        assert!(
            matches!(m, Modal::TransferName { touched: true, .. }),
            "un pop que borra fija touched"
        );

        let mut vacio = transfer_name();
        if let Modal::TransferName { name, touched, .. } = &mut vacio {
            name.clear();
            *touched = false;
        }
        vacio.text_prompt().expect("campo").pop();
        assert!(
            matches!(vacio, Modal::TransferName { touched: false, .. }),
            "un pop vacío no estrecha la vía de bytes originales"
        );
    }

    /// El retroceso limpia el diagnóstico aunque no borre nada.
    #[test]
    fn el_retroceso_en_vacio_limpia_el_diagnostico() {
        let mut m = Modal::Mkdir {
            name: String::new(),
            error: Some(String::from("ya existe")),
        };
        m.text_prompt().expect("campo").pop();
        assert!(m.text_prompt().expect("campo").error().is_none());
    }
}
