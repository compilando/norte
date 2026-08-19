//! Los modales, sus límites de longitud y la decisión de si salir pide
//! confirmación.

use super::trail::Trail;
use norte_proto::VPath;

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
        /// ENTERO es revisable por scroll ([`crate::app::App::ai_plan_scroll`]) — sin
        /// esto, la cola de un plan > [`crate::app::AI_RENAME_PAIR_LIMIT`] se aplicaba
        /// sin poder verse.
        offset: usize,
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

/// Tope de caracteres del patrón de [`Modal::MarkPattern`] (#103 T9 review
/// MINOR): en `chars()`, no bytes — igual criterio que [`crate::app::DETAIL_MAX_CHARS`],
/// un carácter multibyte cuenta una vez.
pub const MARK_PATTERN_MAX_CHARS: usize = 256;

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
/// APARTE de [`MARK_PATTERN_MAX_CHARS`] y mucho mayor, porque lo que se mide
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
