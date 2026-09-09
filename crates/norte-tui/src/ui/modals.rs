//! El texto de cada modal, y el alto que hay que reservarle.
//!
//! Un `*_modal_text` no pinta: DEVUELVE el cuerpo ya compuesto, y por eso se
//! puede afirmar sobre él sin un backend de test. `modal_height` es la otra
//! mitad del contrato — si las dos se desincronizan, el modal se recorta.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::text::{badge_prefixed, clamp_chars, tail_window};
use super::{HOSTILE_BADGE, centered, clear_themed};
use crate::app::{AI_RENAME_PAIR_LIMIT, SEMANTIC_HIT_LIMIT, display_name};
use crate::theme::TuiTheme;
use norte_frontend::middle_ellipsis;
use norte_i18n::{t, ta};

/// Presupuesto en CHARS de una ruta dentro de un modal, antes de la elipsis
/// media. El mismo que ya usaban el modal de aprobación y el de colisión:
/// `modal_width` crece hasta el ancho del frame, así que el recorte lo pone
/// el contenido — jamás el borde de la caja, que corta a pelo.
pub(crate) const MODAL_PATH_CHARS: usize = 46;

/// Qué ES una línea del cuerpo de un modal, para poder pintarla distinto.
///
/// El cuerpo era UNA cadena y `draw_modal` la pintaba como un párrafo plano,
/// así que el campo editable, las rutas, la pista y las teclas salían todos
/// del mismo color y el mismo peso: un modal sin jerarquía, en el que lo
/// último que encuentras es lo único que puedes tocar.
///
/// Es SEMÁNTICO, no un color: lo que se declara aquí es el papel de la línea,
/// y el tema decide con qué se pinta. Un modal que no declare nada sigue
/// saliendo en texto plano, que es como salían los 56 — la migración es una
/// línea cada vez y no un big bang.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LineKind {
    /// Un dato normal.
    #[default]
    Plain,
    /// Una etiqueta, una pista o la fila de teclas: acompaña al dato y no es
    /// el dato. Se atenúa para que no compita con él.
    Dim,
    /// Lo que hay que leer antes de decir que sí: el destino de una copia.
    Strong,
    /// El campo EDITABLE. Es la única línea que el usuario puede cambiar, y
    /// hasta ahora no había forma de saberlo mirando.
    Field,
    /// Algo que hay que saber antes de confirmar y no impide confirmar.
    Warning,
    /// Por qué esto no se puede hacer todavía.
    Error,
}

/// Una línea del cuerpo de un modal.
#[derive(Debug, Clone)]
pub(crate) struct ModalLine {
    /// El texto, ya enmascarado y acotado por quien lo compuso. SIN la marca
    /// de nombre alterado: esa va aparte ([`Self::hostile`]).
    pub(crate) text: String,
    /// Qué papel juega.
    pub(crate) kind: LineKind,
    /// Lo que se pinta DIFIERE de los bytes reales, y la línea lleva delante
    /// la marca que lo dice (spec §6).
    ///
    /// Aparte del texto, y eso no es comodidad: la marca se pinta con
    /// `Role::HostileBadge`, que es el único rol de este modal con contraste
    /// GARANTIZADO (el gate de `norte-theme` lo mide a 4.5:1 en los ocho
    /// presets). Metida dentro del texto heredaba el estilo del papel de la
    /// línea, y una línea atenuada dejaba la señal de spec §6 a 2,3:1 sobre
    /// un tema claro — una advertencia que no se lee no es una advertencia.
    pub(crate) hostile: bool,
}

impl ModalLine {
    /// Una línea de un papel dado.
    pub(crate) fn new(text: impl Into<String>, kind: LineKind) -> Self {
        Self {
            text: text.into(),
            kind,
            hostile: false,
        }
    }

    /// La misma línea, marcada como alterada.
    pub(crate) fn hostile(mut self, hostile: bool) -> Self {
        self.hostile = hostile;
        self
    }

    /// Una línea sin papel declarado: texto plano, como salía todo antes.
    pub(crate) fn plain(text: impl Into<String>) -> Self {
        Self::new(text, LineKind::Plain)
    }

    /// Lo que ocupa al pintarse, en CELDAS: el texto más la marca, si la
    /// lleva. Es lo que mide `modal_width`, y por eso vive junto al modelo —
    /// una anchura que no cuente la marca deja la caja corta.
    pub(crate) fn width(&self) -> usize {
        self.text.as_str().width() + if self.hostile { badge_width() } else { 0 }
    }
}

/// Lo que ocupa la marca de nombre alterado, con su espacio.
fn badge_width() -> usize {
    HOSTILE_BADGE.width() + 1
}

/// El cuerpo de un modal: sus líneas, en orden.
pub(crate) type ModalBody = Vec<ModalLine>;

/// Un cuerpo compuesto como cadena se parte por líneas y sale plano.
///
/// Es lo que deja que las 55 variantes que aún componen `String` no se toquen:
/// declarar papeles es un cambio POR MODAL, no un requisito para compilar.
pub(crate) fn plain_body(cuerpo: &str) -> ModalBody {
    cuerpo.lines().map(ModalLine::plain).collect()
}

/// El estilo con el que se pinta cada papel.
///
/// Vive aquí y no en cada modal por lo de siempre: dos sitios que deciden qué
/// color lleva una pista acaban con dos pistas de colores distintos.
fn line_style(kind: LineKind, theme: &TuiTheme) -> ratatui::style::Style {
    use ratatui::style::Style;
    match kind {
        LineKind::Plain => Style::default(),
        // `Info`, NO `BorderUnfocused`. El de los bordes parecía lo natural
        // —«está, se lee, y no reclama la vista»— pero es un rol pensado para
        // CROMO: los temas claros lo ponen muy pálido y encima lleva `dim`.
        // Medido sobre el fondo de su propio tema da 2,30:1 en
        // `catppuccin-latte` y 2,45:1 en `gruvbox-light`, cuando WCAG AA pide
        // 4,5 para texto. `Info` da 4,34 y 5,82 en los mismos dos.
        LineKind::Dim => theme.role(Role::Info),
        LineKind::Strong => theme.role(Role::Title),
        // El fondo de una fila seleccionada: es exactamente lo que un campo
        // es —lo que tienes «cogido»— y ya significa eso en el listado.
        LineKind::Field => theme.role(Role::Selection),
        LineKind::Warning => theme.role(Role::Warning),
        LineKind::Error => theme.role(Role::Error),
    }
}

/// CELDAS de terminal (`UnicodeWidthStr::width`, mismo idioma que
/// [`draw_nav_popup`]/[`middle_ellipsis`]), no en `chars` — un cuerpo con
/// CJK (dos celdas por char, p. ej. un path con `日本語`) desbordaba la caja
/// con el conteo de chars antiguo.
pub(crate) fn modal_width(titulo: &str, cuerpo: &ModalBody, frame_width: u16) -> u16 {
    let content_max = cuerpo
        .iter()
        .map(ModalLine::width)
        .chain(std::iter::once(titulo.width() + 2))
        .max()
        .unwrap_or(0);
    u16::try_from(content_max + 4)
        .unwrap_or(u16::MAX)
        .clamp(60, frame_width.saturating_sub(4).max(60))
}

/// Si `modal` tiñe el borde de aviso (rol `warning`): un borrado PERMANENTE
/// o una decisión de seguridad (aprobar una op de agente, confiar en una
/// host key o en un `init.lua` de proyecto). Factorizado fuera de
/// `draw_modal` (clippy `too_many_lines`).
pub(crate) fn is_warning_modal(modal: &crate::app::Modal) -> bool {
    use crate::app::Modal;
    matches!(
        modal,
        Modal::ConfirmDelete {
            permanent: true,
            ..
        } | Modal::ApproveAgentOp { .. }
            | Modal::TrustHostKey { .. }
            | Modal::TrustLuaInit { .. }
    )
}

/// Caja centrada del modal.
/// `reinterpret` = enc del pane con FOCO al pintar: correcto para los
/// modales SÍNCRONOS (confirmar copy/move/delete se crea desde el pane con
/// foco y un modal abierto congela el foco — creación ≡ draw). Los ASYNC
/// (colisión) llevan su enc capturado al lanzar (`RetrySpec`, #98/M1). Los
/// paths de agentes (`ApproveAgentOp`) JAMÁS se reinterpretan: otra
/// frontera de confianza (van por `display_name` crudo a propósito). `hints`
/// (H1 T3, #24) trae los pies de página GENERADOS de cada modal — uno por
/// campo, ya resueltos del efectivo `dialog` vigente.
/// Título+cuerpo del modal activo, extraído de `draw_modal` (clippy
/// `too_many_lines` al crecer la familia de modales).
///
/// Y con S4 (#135) vuelve a pasarse del tope, esta vez sin sitio al que
/// extraer: lo que queda es una TABLA modal→texto, un brazo por variante y
/// exhaustiva a propósito (un modal nuevo no compila hasta que alguien decide
/// cómo se pinta). Partirla en dos mitades solo movería la frontera a un
/// punto arbitrario y haría más difícil ver que no falta ninguna. Mismo
/// criterio, y misma excepción, que la tabla de despacho de `main.rs`.
/// Título y cuerpo YA CON PAPELES de cada modal.
///
/// Dos puertas a propósito. Un modal que quiera jerarquía —etiquetas
/// atenuadas, un campo que se vea campo, el destino destacado— se atiende
/// aquí arriba y compone sus [`ModalLine`]. Todo lo demás sale de la tabla de
/// texto de siempre y se convierte a líneas planas, que es exactamente como
/// se pintaba antes.
///
/// Así declarar papeles es un cambio POR MODAL. La alternativa —tocar las 56
/// variantes de golpe— era un diff de miles de líneas para una mejora que se
/// aprecia en cinco.
pub(crate) fn modal_title_body(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, ModalBody) {
    use crate::app::Modal;
    if let Modal::ConfirmTransfer {
        kind,
        items,
        to,
        space,
        confine,
    } = modal
    {
        return confirm_transfer_modal(
            *kind,
            items,
            to,
            reinterpret,
            DestNotices {
                space: space.as_deref(),
                confine: confine.as_deref(),
            },
            &hints.confirm,
        );
    }
    if let Modal::TransferName {
        kind,
        from,
        to_dir,
        name,
        enc,
        error,
        space,
        confine,
        ..
    } = modal
    {
        return transfer_name_modal(
            *kind,
            from,
            to_dir,
            name,
            error.as_deref(),
            // La reinterpretación CAPTURADA al abrir, jamás la del pane al
            // pintar (#98/M1): es la misma que ya usaba la tabla de texto.
            *enc,
            DestNotices {
                space: space.as_deref(),
                confine: confine.as_deref(),
            },
        );
    }
    let (titulo, cuerpo) = modal_title_text(modal, reinterpret, hints);
    (titulo, plain_body(&cuerpo))
}

#[expect(clippy::too_many_lines, reason = "tabla modal→texto, no lógica")]
fn modal_title_text(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, String) {
    use crate::app::Modal;
    match modal {
        // #103 T10: el lote va como LISTA — una ruta por línea, saneada y
        // truncada por la política COMPARTIDA con la GUI
        // (`norte_frontend::item_lines_with`), jamás dos rutas en la misma
        // línea (un nombre hostil fabricaría una entrada de la lista).
        // Las capabilities van UNA POR LÍNEA, cada una con su bandera si su
        // texto difiere del real: son texto de un tercero, y una lista pegada
        // en una frase deja que una finja ser otra (#280).
        Modal::ConfirmPluginApproval {
            name,
            name_hostile,
            caps,
            ..
        } => (
            t("modal-plugin-approval-title"),
            [
                vec![format!(
                    "{name}{}",
                    if *name_hostile { HOSTILE_BADGE } else { "" }
                )],
                caps.iter()
                    .map(|(texto, hostil)| {
                        format!("  · {texto}{}", if *hostil { HOSTILE_BADGE } else { "" })
                    })
                    .collect(),
                vec![t("modal-plugin-approval-note"), hints.approval.clone()],
            ]
            .concat()
            .join("\n"),
        ),
        Modal::ConfirmDelete { items, permanent } => (
            if *permanent {
                t("modal-delete-permanent-title")
            } else {
                t("modal-trash-title")
            },
            [
                norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret),
                vec![
                    if *permanent {
                        t("modal-delete-permanent-warning")
                    } else {
                        t("modal-trash-note")
                    },
                    hints.confirm.clone(),
                ],
            ]
            .concat()
            .join("\n"),
        ),
        // #98/M1: la colisión llega ASYNC — usa el enc capturado al LANZAR
        // la operación (RetrySpec), jamás el del pane con foco al llegar.
        Modal::Collision { retry } => (
            t("modal-collision-title"),
            format!(
                "{}
{}
{}",
                t("modal-collision-body"),
                norte_frontend::path_display_with(&retry.to, retry.name_encoding).0,
                hints.collision
            ),
        ),
        Modal::ApproveAgentOp { req } => approval_modal_text(req, &hints.approval),
        Modal::TrustHostKey {
            host,
            port,
            algo,
            fingerprint,
            ..
        } => trust_host_modal_text(host, *port, algo, fingerprint, &hints.trust_host),
        // #325: el nombre de la conexión sale de `connections.toml` —lo
        // escribió el propio usuario, no un servidor—, pero se sanea igual:
        // un fichero de conexiones puede venir de un dotfile ajeno.
        Modal::AskSecret {
            conn,
            endpoint,
            input,
            ..
        } => ask_secret_modal_text(conn, endpoint, input, &hints.ask_secret),
        // TOFU Lua (M4): `path` viene YA saneado por el constructor del
        // modal (`detail_for_bar`); el cuerpo es un solo mensaje largo y el
        // Paragraph de este modal lleva wrap (abajo).
        Modal::TrustLuaInit { path, hash_abbrev } => (
            t("modal-lua-trust-title"),
            ta(
                "modal-lua-trust-body",
                &[("path", path.as_str()), ("hash", hash_abbrev.as_str())],
            ),
        ),
        // S2 (`[ui] confirm_quit`): sin datos propios — un título+cuerpo
        // fijos más el hint (`hints.confirm`, ALLOW_CONFIRM reutilizado).
        // #139: las propiedades salen del LISTADO —nada que pedir— salvo el
        // recuento de una carpeta, que es lo único que un listado no sabe.
        Modal::Properties {
            entry,
            size,
            size_task,
        } => properties_modal_text(entry, *size, size_task.is_some()),
        Modal::ConfirmQuit => (
            t("modal-confirm-quit-title"),
            format!("{}\n{}", t("modal-confirm-quit-body"), hints.confirm),
        ),
        // #311: una línea por fichero, con su suma y —al comprobar— su
        // veredicto. El nombre va por el saneado de siempre: un fichero de
        // sumas nombra ficheros, y un nombre puede traer bidi dentro.
        Modal::Checksums {
            title_key,
            rows,
            offset,
        } => checksums_modal_text(title_key, rows, *offset),
        // #103 T9: ver `mark_pattern_modal_text` (enmascarado, no un texto
        // fijo — el patrón/error son de usuario).
        Modal::MarkPattern {
            mark,
            pattern,
            error,
        } => mark_pattern_modal_text(*mark, pattern, error.as_deref()),
        // #104: mismo enmascarado que el patrón — nombre y error son de
        // usuario (paste con bidi/invisibles incluido).
        Modal::Mkdir { name, error } => {
            free_text_modal_text("modal-mkdir", "modal-mkdir-hint", name, error.as_deref())
        }
        // #306: el mismo molde para el nombre de un PERFIL. El pie dice que
        // se guarda lo que se ve, que es la pregunta que tiene quien lo abre.
        Modal::ProfileSaveAs { name, error } => free_text_modal_text(
            "modal-profile-save-as",
            "modal-profile-save-as-hint",
            name,
            error.as_deref(),
        ),
        // #290: el mismo molde con la otra clase de nodo. El nombre se pide
        // porque lo crea el daemon, no el editor.
        Modal::EditNew { name, error, .. } => free_text_modal_text(
            "modal-new-file",
            "modal-new-file-hint",
            name,
            error.as_deref(),
        ),
        // #132: mismo enmascarado y mismo molde. El pie del de empaquetar dice
        // qué formato sale del nombre TECLEADO, no del sugerido: es la única
        // forma de que el usuario vea la decisión antes de confirmarla.
        Modal::Pack { name, error } => free_text_modal_text(
            "modal-pack",
            match crate::app::format_by_name(name.as_bytes()) {
                Some(norte_proto::methods::ArchiveFormat::Zip) => "modal-pack-hint-zip",
                Some(norte_proto::methods::ArchiveFormat::Tar) => "modal-pack-hint-tar",
                Some(norte_proto::methods::ArchiveFormat::TarGz) => "modal-pack-hint-targz",
                None => "modal-pack-hint-unknown",
            },
            name,
            error.as_deref(),
        ),
        Modal::Split { size, error } => {
            free_text_modal_text("modal-split", "modal-split-hint", size, error.as_deref())
        }
        // #314: el modo en octal, con CUÁNTAS entradas va a cambiar en el
        // título. El número importa: teclear un modo con cincuenta ficheros
        // marcados y creyendo que va sobre uno es el error que este diálogo
        // tiene que hacer difícil.
        Modal::Chmod {
            mode,
            targets,
            error,
        } => {
            let (_, cuerpo) =
                free_text_modal_text("modal-chmod", "modal-chmod-hint", mode, error.as_deref());
            // El singular tiene su propio id: los args de i18n son cadenas, y
            // un selector de plural sobre una cadena no elige nunca.
            let titulo = if targets.len() == 1 {
                t("modal-chmod-one")
            } else {
                norte_i18n::ta("modal-chmod", &[("n", &targets.len().to_string())])
            };
            (titulo, cuerpo)
        }
        // Mismo enmascarado: la dirección tecleada y su diagnóstico son texto
        // de usuario, y una dirección llega por paste tan fácil como un nombre.
        Modal::TransferDest { kind, input, error } => free_text_modal_text(
            match kind {
                crate::app::TransferKind::Copy => "modal-transfer-dest-copy",
                crate::app::TransferKind::Move => "modal-transfer-dest-move",
            },
            "modal-transfer-dest-hint",
            input,
            error.as_deref(),
        ),
        // M4-IA: mismo enmascarado que mkdir — instrucción y error son texto
        // de usuario (paste con bidi/invisibles incluido).
        // #135: mismo enmascarado que la instrucción IA — la línea de
        // comandos y su diagnóstico son texto de usuario.
        Modal::CommandLine { command, error } => free_text_modal_text(
            "modal-command-line",
            "modal-command-line-hint",
            command,
            error.as_deref(),
        ),
        Modal::AiRenameInstruction { instruction, error } => free_text_modal_text(
            "modal-ai-rename",
            "modal-ai-rename-hint",
            instruction,
            error.as_deref(),
        ),
        // #310: la plantilla del lote. Mismo molde de texto libre y el mismo
        // enmascarado: lo tecleado puede llegar por paste con bidi dentro.
        Modal::RenameBatchPattern { pattern, error } => free_text_modal_text(
            "modal-rename-batch",
            "modal-rename-batch-hint",
            pattern,
            error.as_deref(),
        ),
        // M4-IA: dir objetivo + ventana de parejas from→to del plan
        // revisable (enmascarado defensivo, ver `ai_rename_plan_modal_text`).
        Modal::AiRenamePlan {
            dir,
            entries,
            offset,
            plan,
            // Lo visto NO cambia lo que se pinta: gatea el confirmar
            // (`dialog_action`) y lo dice el pie.
            seen: _,
        } => ai_rename_plan_modal_text(dir, entries, *offset, hints, plan),
        // M4-IA-2: mismo enmascarado que la instrucción IA — consulta y
        // error son texto de usuario.
        Modal::SemanticQuery { query, error } => free_text_modal_text(
            "modal-semantic",
            "modal-semantic-hint",
            query,
            error.as_deref(),
        ),
        // M4-IA-2: ventana de hits con cursor (enmascarado defensivo, ver
        // `semantic_hits_modal_text`).
        Modal::SemanticHits {
            hits,
            offset,
            cursor,
        } => semantic_hits_modal_text(hits, *offset, *cursor, hints),
        // Los YA MIGRADOS: los atiende `modal_title_body`, que compone sus
        // líneas CON PAPELES. El brazo queda porque el `match` es exhaustivo a
        // propósito —un modal nuevo no compila hasta que alguien decide cómo
        // se pinta— y esa red no se pierde por migrar uno.
        //
        // El cuerpo vacío es inalcanzable por el único llamante, que
        // desvía estas dos variantes antes. Un `unreachable!` sería un panic
        // en release (regla 6); el `debug_assert` lo pone rojo en los tests si
        // alguien añade un segundo llamante y se salta el desvío.
        Modal::TransferName { .. } | Modal::ConfirmTransfer { .. } => {
            debug_assert!(
                false,
                "modal con papeles pedido a la tabla de texto: se atiende en \
                 `modal_title_body`"
            );
            (String::new(), String::new())
        }
    }
}

/// Alto del modal por variante (líneas de contenido + bordes).
pub(crate) fn modal_height(modal: &crate::app::Modal) -> u16 {
    use crate::app::Modal;
    match modal {
        // Review H3c MINOR-5: cabecera + la VENTANA de rutas (más el resumen,
        // si el lote no cabe entero) + el pie, más los bordes — el MISMO
        // cómputo acotado que ConfirmDelete, y por la misma razón: cuántas
        // rutas trae la petición lo elige el AGENTE, y `centered` recorta
        // contra el frame, así que un alto sin tope dejaba las últimas líneas
        // sin pintar. La última es el aviso de que las teclas están inertes.
        Modal::ApproveAgentOp { req } => {
            // Mismo cómputo que `approval_modal_text`: la línea de resumen
            // aparece cuando la DECISIÓN cubre más rutas de las que se pintan,
            // aunque el recorte lo haya hecho el server (`paths_total`).
            let shown = req.paths.len().min(norte_frontend::MODAL_ITEM_LIMIT);
            let total = usize::try_from(req.paths_total)
                .unwrap_or(usize::MAX)
                .max(req.paths.len());
            // Cuerpo + plazo + las opcionales del detalle + las rutas +
            // resumen + teclas. El plazo va SIEMPRE (desconocido también se
            // dice), y las de modo y alcance son las que este cómputo se
            // dejaba: un `set-mode` recursivo perdía su última línea.
            let lines = 1
                + 1
                + usize::from(req.detail.mode.is_some())
                + usize::from(req.detail.recursive)
                + shown
                + usize::from(total > shown)
                + 1;
            // `+ 2` (los bordes), no el `+ 3` de ConfirmDelete: este modal
            // siempre ajustó exacto y acotar la lista no es motivo para
            // moverle la caja una fila.
            u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(2)
        }
        // #103 T10: una línea POR ítem listado (más la de resumen, si el
        // lote no cabe entero), más las dos fijas (destino/modo + teclas) y
        // los bordes — el mismo `body_lines + 3` que el resto. `centered`
        // recorta contra el frame: en un terminal enano el lote se ve a
        // medias, nunca desborda.
        Modal::ConfirmDelete { items, .. } | Modal::ConfirmTransfer { items, .. } => {
            let listed = items.len().min(norte_frontend::MODAL_ITEM_LIMIT)
                + usize::from(items.len() > norte_frontend::MODAL_ITEM_LIMIT);
            u16::try_from(listed).unwrap_or(u16::MAX).saturating_add(5)
        }
        // TrustHostKey: host + algo + fingerprint + nota + teclas (5 líneas)
        // + bordes.
        Modal::TrustHostKey { .. } => 9,
        // TransferName: origen + destino + blanco + etiqueta + campo + blanco
        // + teclas son 7 líneas, +3; y una más por cada aviso del destino y
        // por el error. SE CUENTAN en vez de ir fijas porque los avisos
        // aparecen y desaparecen (#343): un alto fijo dejaba la última línea
        // —la de las teclas, o el propio aviso— fuera de la caja.
        //
        // Que este número case con lo que compone `transfer_name_modal` lo
        // ata un test (`el_alto_declarado_cubre_el_cuerpo`), que es lo que
        // faltaba: el rustdoc de este módulo lleva desde el principio diciendo
        // que las dos mitades se desincronizan y nadie lo comprobaba.
        Modal::TransferName {
            error,
            space,
            confine,
            ..
        } => {
            let extras = usize::from(error.is_some())
                + usize::from(space.is_some())
                + usize::from(confine.is_some());
            u16::try_from(extras).unwrap_or(u16::MAX).saturating_add(10)
        }
        // TrustLuaInit: un mensaje largo con wrap (~4 líneas a 58 cols) +
        // bordes.
        // #325 `AskSecret`: conexión + destino + campo de puntos + nota +
        // teclas son 5 líneas, +3. La caja NO cambia de alto al teclear — el
        // campo pinta siempre una línea, llena o vacía.
        Modal::TrustLuaInit { .. } | Modal::AskSecret { .. } => 8,
        // Patrón/mkdir + hint + teclas (3 líneas) o + la línea de error (4),
        // más bordes (#103 T9: mismo cómputo `body_lines + 3` que el resto).
        // Sin error caen al comodín `6` de abajo (match_same_arms).
        Modal::MarkPattern { error: Some(_), .. }
        | Modal::Mkdir { error: Some(_), .. }
        | Modal::EditNew { error: Some(_), .. }
        | Modal::TransferDest { error: Some(_), .. }
        | Modal::CommandLine { error: Some(_), .. }
        | Modal::AiRenameInstruction { error: Some(_), .. }
        | Modal::RenameBatchPattern { error: Some(_), .. }
        | Modal::SemanticQuery { error: Some(_), .. } => 7,
        // M4-IA: la línea del dir (audit MAJOR-1) + el veredicto del LOTE
        // (§17) + dos por pareja de la VENTANA + el indicador (si el plan no
        // cabe entero) + el detalle del lote (contado por
        // `rename_batch_detail_lines` — la MISMA función que lo pinta, no una
        // fórmula paralela que se desincronice) + el hint, más bordes — mismo
        // cómputo dinámico `body_lines + 3` que
        // ConfirmDelete/ConfirmTransfer. Estable al scroll: la ventana
        // clampada siempre pinta `min(len, LIMIT)` parejas.
        // #139: nombre, clase, tamaño, fecha y ruta, más un atributo por línea
        // y la línea del recuento cuando la entrada es una carpeta.
        Modal::Properties { entry, size, .. } => {
            let lines = 5
                + entry.attrs.len()
                + usize::from(entry.kind == norte_proto::EntryKind::Dir || size.is_some());
            u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(3)
        }
        // #311: una línea por fila de la ventana + el indicador de que hay más
        // + el hint.
        Modal::Checksums { rows, offset, .. } => {
            // Con la ventana desplazada, «hay más» puede ser falso aunque la
            // lista sea larga: la caja se mide con lo que se va a PINTAR.
            let visibles = rows.len().saturating_sub(*offset).min(AI_RENAME_PAIR_LIMIT);
            let hay_mas = rows.len().saturating_sub(offset + AI_RENAME_PAIR_LIMIT) > 0;
            let lines = visibles + usize::from(hay_mas) + 1;
            u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(3)
        }
        Modal::AiRenamePlan { entries, plan, .. } => {
            let lines = 2
                + 2 * entries.len().min(AI_RENAME_PAIR_LIMIT)
                + usize::from(entries.len() > AI_RENAME_PAIR_LIMIT)
                + plan.detail_line_count()
                + 1;
            u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(3)
        }
        // M4-IA-2: un hit POR LÍNEA de la ventana + el indicador (si el
        // lote no cabe entero) + el hint — mismo cómputo dinámico
        // `body_lines + 3` que el plan IA. Estable al scroll.
        Modal::SemanticHits { hits, .. } => {
            let lines = hits.len().min(SEMANTIC_HIT_LIMIT)
                + usize::from(hits.len() > SEMANTIC_HIT_LIMIT)
                + 1;
            u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(3)
        }
        _ => 6,
    }
}

/// Pinta el modal activo: borde (de aviso en las superficies de decisión
/// duras), título y cuerpo de `modal_title_body`. `reinterpret` es la
/// reinterpretación del pane con foco AL PINTAR — los modales que capturan
/// la suya al abrir (`Collision` #98/M1, `TransferName` #105) la ignoran a
/// favor de la capturada.
pub(crate) fn draw_modal(
    frame: &mut Frame<'_>,
    modal: &crate::app::Modal,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) {
    use crate::app::Modal;
    let (title, body) = modal_title_body(modal, reinterpret, hints);
    // Un borrado PERMANENTE (o aprobar una mutación de agente) tiñe el borde
    // de aviso (rol `warning`).
    let border = if is_warning_modal(modal) {
        theme.role(Role::Warning)
    } else {
        theme.role(Role::ModalBorder)
    };
    // Altura: fija salvo la aprobación (una línea POR ruta, H2 del auditor).
    let height = modal_height(modal);
    let area = centered(
        frame.area(),
        modal_width(&title, &body, frame.area().width),
        height,
    );
    clear_themed(frame, area, theme);
    // Cada línea con el estilo de SU papel. Una línea sin papel declarado sale
    // en el color de siempre, que es lo que hace que los modales aún no
    // migrados se pinten exactamente igual que antes.
    // El interior de la caja: el ancho menos los dos bordes. Es lo que un
    // CAMPO tiene que ocupar entero.
    let interior = usize::from(area.width.saturating_sub(2));
    let lineas: Vec<ratatui::text::Line<'_>> = body
        .iter()
        .map(|l| {
            // Un campo se rellena hasta el borde. Con el fondo acabando donde
            // acaba el texto parece texto RESALTADO, no un sitio donde
            // escribir — y además no se ve cuánto cabe. Es una decisión de
            // pintado y por eso vive aquí: quien compone el cuerpo no sabe
            // todavía cómo de ancha va a salir la caja.
            let texto = if l.kind == LineKind::Field {
                let hueco = interior.saturating_sub(l.width());
                format!("{}{}", l.text, " ".repeat(hueco))
            } else {
                l.text.clone()
            };
            let estilo = line_style(l.kind, theme);
            // La marca de nombre alterado, en su propio tramo y con SU rol:
            // es la única señal de esta caja cuyo contraste está garantizado
            // (spec §6), y heredar el estilo de una línea atenuada la dejaba
            // ilegible justo donde más falta hace.
            if l.hostile {
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::styled(
                        format!("{HOSTILE_BADGE} "),
                        theme.role(Role::HostileBadge),
                    ),
                    ratatui::text::Span::styled(texto, estilo),
                ])
            } else {
                ratatui::text::Line::styled(texto, estilo)
            }
        })
        .collect();
    let mut body = Paragraph::new(lineas).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(theme.role(Role::Title))
            .border_style(border),
    );
    // Solo este modal envuelve: su cuerpo es UN mensaje largo; el resto ya
    // viene troceado por líneas (y el wrap podría partir un path por
    // cualquier char, cosa que los modales de rutas evitan con elipsis).
    if matches!(modal, Modal::TrustLuaInit { .. }) {
        body = body.wrap(ratatui::widgets::Wrap { trim: false });
    }
    frame.render_widget(body, area);
}

/// Bytes de la sesión para display; `None` = `?`. Sin colisión con una sesión
/// literal `"?"`: el daemon valida el charset `[A-Za-z0-9._-]` en el
/// handshake, así que `?` no es un id alcanzable.
pub(crate) fn session_bytes(req: &norte_proto::methods::PolicyApprovalRequired) -> &[u8] {
    req.session.as_deref().map_or(b"?", str::as_bytes)
}

/// Recorta a `max` CHARS (no bytes) con `…` final. Para strings ya
/// enmascarados que aún podrían ser kilométricos (clamp de layout, H1).
/// Texto `(título, cuerpo)` del modal de aprobación de agente (M3-3b T5).
/// TODO lo interpolado lo controla el AGENTE (encoding-auditor H1/H2/H3) y
/// esto es una decisión humana de seguridad: session y rutas pasan por el
/// MISMO enmascarado que los nombres de pane (controles/bidi/invisibles → �)
/// MÁS clamp; cada ruta va en SU línea con etiqueta fuera de banda (jamás un
/// joiner in-band que un nombre pueda imitar) y elipsis media (un `from`
/// kilométrico no expulsa el destino de la caja); el enmascarado se MARCA con
/// el badge (spec §6).
///
/// La lista se ENVENTANA en [`norte_frontend::MODAL_ITEM_LIMIT`] rutas más una
/// línea de resumen, como `ConfirmDelete`/`ConfirmTransfer` (review H3c
/// MINOR-5). Cuántas rutas trae la petición lo elige el AGENTE, y sin tope el
/// alto crecía con ellas: `centered` recorta contra el frame, así que las
/// líneas de sobra no se pintaban — incluida la ÚLTIMA, que bajo H3c es la
/// única explicación de por qué las teclas del modal no responden. El resumen
/// lleva badge si alguna ruta OCULTA es hostil (misma doctrina que el plan IA y
/// los hits semánticos: lo escondido jamás se cuela "limpio").
pub(crate) fn approval_modal_text(
    req: &norte_proto::methods::PolicyApprovalRequired,
    hint: &str,
) -> (String, String) {
    let session = clamp_chars(&display_name(session_bytes(req)).0, 40);
    let op = clamp_chars(&display_name(req.op.as_bytes()).0, 16);
    let mut lines = vec![ta(
        "modal-approval-body",
        &[("session", &session), ("op", &op)],
    )];
    // Cuánto le queda. Una decisión con fecha de caducidad que no la enseña se
    // lee como una que espera para siempre, y quien vuelve al rato pulsa
    // aprobar sobre algo que el daemon ya denegó. La ventana lo decía y este
    // terminal no, con las mismas claves delante.
    //
    // `ttl_ms == 0` es DESCONOCIDO —una pendiente reconstruida por el resync
    // de `policy.pending` no transporta el plazo restante— y se dice, en vez
    // de callar: callar deja el diálogo delante invitando a aprobar sobre un
    // id que el daemon puede haber reapado hace rato.
    lines.push(if req.ttl_ms > 0 {
        ta(
            "modal-approval-ttl",
            &[("s", &req.ttl_ms.div_ceil(1000).to_string())],
        )
    } else {
        t("modal-approval-ttl-unknown")
    });
    // #314: lo que la op AÑADE a la pregunta. Para todas menos una no hay
    // nada: la op y las rutas son la decisión. Un `set-mode` sí, porque dos
    // con las mismas rutas y modos distintos significan cosas opuestas, y sin
    // esta línea el humano no sabía si decía que sí a `0600` o a `4777`.
    if let Some(mode) = req.detail.mode {
        lines.push(ta(
            "modal-approval-mode",
            &[("mode", &norte_frontend::chmod::format_mode(mode))],
        ));
    }
    // #315: y el ALCANCE, que sin esta línea el humano tampoco veía. Un
    // recursivo sobre una raíz se preguntaba como «1 ruta», y lo que se
    // aprobaba eran todos sus descendientes — el mismo agujero que el modo
    // vino a cerrar, una talla más grande.
    if req.detail.recursive {
        lines.push(match req.detail.dir_mode {
            Some(dir) => ta(
                "modal-approval-recursive-dirs",
                &[("mode", &norte_frontend::chmod::format_mode(dir))],
            ),
            None => t("modal-approval-recursive"),
        });
    }
    let limit = norte_frontend::MODAL_ITEM_LIMIT;
    for (i, p) in req.paths.iter().take(limit).enumerate() {
        let (text, hostile) = display_name(p.as_bytes());
        lines.push(ta(
            "modal-approval-path",
            &[
                ("badge", if hostile { HOSTILE_BADGE } else { "" }),
                ("n", &(i + 1).to_string()),
                ("path", &middle_ellipsis(&text, 46)),
            ],
        ));
    }
    // Cuántas cubre la DECISIÓN, no cuántas llegaron: el server recorta la
    // notificación (un lote de renames gatea miles de rutas) y sin
    // `paths_total` el modal enseñaría 32 rutas inocentes como si fueran todas
    // — que es aprobar a ciegas creyendo que se aprueba a la vista. `0` =
    // server N-1 que no lo mandaba: entonces lo recibido ES todo lo que hubo.
    let total = usize::try_from(req.paths_total)
        .unwrap_or(usize::MAX)
        .max(req.paths.len());
    let shown = req.paths.len().min(limit);
    if total > shown {
        // El badge solo puede hablar de lo que se PUEDE mirar: las rutas que
        // el server recortó no están aquí para inspeccionarlas. Lo que no se
        // calla es el NÚMERO, que es lo que decide el consentimiento.
        //
        // La pregunta la contesta el crate COMPARTIDO: la ventana no la hacía
        // sobre las mismas rutas, y una decisión de seguridad escrita en un
        // solo frontend es la mitad del producto sin ella (ADR 0077).
        let hidden_hostile = norte_frontend::overflow_hostile_redacted(&req.paths, shown);
        // Clave COMPARTIDA con `item_lines_with` (la de ConfirmDelete): el
        // resumen dice lo mismo en los dos sitios o el lector aprende dos
        // frases para un solo hecho.
        lines.push(badge_prefixed(
            hidden_hostile,
            ta("gui-modal-more", &[("n", &(total - shown).to_string())]),
        ));
    }
    lines.push(hint.to_owned());
    (t("modal-approval-title"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::MarkPattern` (#103 T9), factorizado fuera de
/// `draw_modal` (clippy `too_many_lines`). Texto libre, NO una superficie de
/// decisión de seguridad — sigue la MISMA disciplina que el resto
/// (enmascarado con `display_name`, jamás crudo): un patrón llega por paste
/// tan fácil como tecleado, y `PatternError` EMBEBE el patrón verbatim en su
/// mensaje (rustdoc de `PatternError::Glob`) — el enmascarado alcanza
/// también a la línea de error.
/// El texto del diálogo de propiedades (#139).
///
/// El nombre y los valores de atributo son datos de FICHERO, así que van
/// enmascarados con el mismo `display_name` que el listado: un nombre con bidi
/// o invisibles no reordena este diálogo.
/// El cuerpo del modal de sumas (#311): una línea por fichero.
///
/// El nombre pasa por [`display_name`] como cualquier otro que se pinte —un
/// fichero de sumas es texto de FUERA y puede nombrar cosas con bidi dentro— y
/// el digest se recorta a doce caracteres: lo que cabe en una caja modal no es
/// una línea de 64, y quien quiera el hash entero lo copia con `Enter`.
pub(crate) fn checksums_modal_text(
    title_key: &str,
    rows: &[crate::app::ChecksumRow],
    offset: usize,
) -> (String, String) {
    /// Lo que cabe de un nombre en la caja. El modal no envuelve, así que sin
    /// esto `ratatui` recorta por la derecha SIN MARCA: dos nombres largos con
    /// el mismo principio se pintan idénticos, y la fila que estás leyendo
    /// para decidir si un ISO es el bueno no dice cuál es.
    const NOMBRE_MAX: usize = 44;

    let mut lines = Vec::with_capacity(rows.len().min(AI_RENAME_PAIR_LIMIT) + 2);
    for row in rows.iter().skip(offset).take(AI_RENAME_PAIR_LIMIT) {
        let (name, hostile) = display_name(&row.name);
        let name = norte_frontend::middle_ellipsis(&name, NOMBRE_MAX);
        let name = if hostile {
            format!("{HOSTILE_BADGE} {name}")
        } else {
            name
        };
        let estado = match (row.verdict, &row.digest) {
            (Some(v), _) => t(v.label_key()),
            (None, Some(d)) => d.chars().take(12).collect::<String>(),
            (None, None) => t("checksum-unreadable"),
        };
        lines.push(format!("{estado}  {name}"));
    }
    // Lo que queda POR DEBAJO de la ventana, que con `offset` no es lo mismo
    // que «las que no caben»: bajando, este número tiene que bajar con él.
    let restantes = rows.len().saturating_sub(offset + AI_RENAME_PAIR_LIMIT);
    if restantes > 0 {
        lines.push(norte_i18n::ta(
            "modal-checksums-more",
            &[("n", &restantes.to_string())],
        ));
    }
    // Copiar solo se ofrece si hay algo que copiar: una comprobación trae
    // veredictos y ningún digest, y el `sha256sum -c` que saldría de ahí sería
    // un fichero vacío. Prometer la tecla igualmente acababa en «nada que
    // copiar», que es un diálogo enseñando una tecla que no hace nada.
    let copiable = rows.iter().any(|r| r.digest.is_some());
    lines.push(t(if copiable {
        "modal-checksums-hint"
    } else {
        "modal-checksums-hint-verify"
    }));
    (t(title_key), lines.join("\n"))
}

pub(crate) fn properties_modal_text(
    entry: &norte_proto::Entry,
    size: Option<(u64, u64)>,
    contando: bool,
) -> (String, String) {
    use norte_proto::EntryKind;

    let (name, hostile) = display_name(
        entry
            .path
            .file_name()
            .map_or(b"".as_slice(), norte_proto::Segment::as_bytes),
    );
    let title = if hostile {
        format!("{HOSTILE_BADGE} {name}")
    } else {
        name
    };
    let class = match entry.kind {
        EntryKind::Dir => t("props-kind-dir"),
        EntryKind::File => t("props-kind-file"),
        EntryKind::Symlink => t("props-kind-symlink"),
        EntryKind::Other => t("props-kind-other"),
    };
    let mut lines = vec![format!("{}: {}", t("props-kind"), class)];
    // El tamaño de una CARPETA no sale del listado: o se ha contado, o se está
    // contando, o —si nadie lo pidió— se dice que se puede pedir. Fingir un
    // cero sería la única respuesta claramente falsa.
    let tamano = match (entry.kind, size, contando) {
        (_, Some((bytes, entradas)), _) => format!(
            "{} ({})",
            norte_frontend::human_bytes(bytes),
            ta("props-entries", &[("count", &entradas.to_string())])
        ),
        (EntryKind::Dir, None, true) => t("props-counting"),
        (EntryKind::Dir, None, false) => t("props-count-hint"),
        (_, None, _) => entry
            .size
            .map_or_else(|| t("props-size-unknown"), norte_frontend::human_bytes),
    };
    lines.push(format!("{}: {}", t("props-size"), tamano));
    lines.push(format!(
        "{}: {}",
        t("props-modified"),
        entry.mtime_ms.map_or_else(
            || t("props-mtime-unknown"),
            |ms| norte_frontend::columns::format_mtime(
                ms,
                norte_frontend::columns::TimeFormat::Iso,
                0
            )
        )
    ));
    let (ruta, path_hostile) = display_name(entry.path.to_wire().as_bytes());
    lines.push(format!(
        "{}: {}{}",
        t("props-path"),
        if path_hostile {
            format!("{HOSTILE_BADGE} ")
        } else {
            String::new()
        },
        ruta
    ));
    // Los atributos que el provider haya reportado, tal cual: los pinta quien
    // los pidió, y esta ventana no pide ninguno de más.
    for (id, value) in &entry.attrs {
        let (v, v_hostile) = attr_text(value);
        lines.push(format!(
            "{id}: {}{v}",
            if v_hostile {
                format!("{HOSTILE_BADGE} ")
            } else {
                String::new()
            }
        ));
    }
    lines.push(t("props-hint"));
    (title, lines.join("\n"))
}

/// El valor de un atributo, listo para pintar, y si hubo que enmascararlo.
///
/// Los dos de TERCEROS —texto y bytes— pasan por `display_name`, el mismo
/// camino lossy-con-badge que un nombre de fichero: un `owner` con bidi no
/// reordena este diálogo, y los bytes originales no se tocan (regla 1).
pub(crate) fn attr_text(v: &norte_proto::AttrValue) -> (String, bool) {
    use norte_proto::AttrValue;
    match v {
        AttrValue::Uint(n) => (n.to_string(), false),
        AttrValue::Int(i) => (i.to_string(), false),
        AttrValue::TimeMs(ms) => (
            norte_frontend::columns::format_mtime(*ms, norte_frontend::columns::TimeFormat::Iso, 0),
            false,
        ),
        AttrValue::Bool(b) => (t(if *b { "col-cell-yes" } else { "col-cell-no" }), false),
        AttrValue::Text(s) => display_name(s.as_bytes()),
        AttrValue::Bytes(b) => display_name(b),
        // Presente-pero-impintable: «?» visible. El blanco queda reservado
        // para AUSENTE, como en las celdas del listado.
        AttrValue::Unknown => ("?".to_owned(), false),
    }
}

pub(crate) fn mark_pattern_modal_text(
    mark: bool,
    pattern: &str,
    error: Option<&str>,
) -> (String, String) {
    let (masked, hostile) = display_name(pattern.as_bytes());
    // #103 T9 review MINOR: `PaneState::mark_glob` compila el patrón CRUDO,
    // no el enmascarado — aquí el display difiere de verdad de lo que
    // decide el match, así que un patrón hostil lleva el mismo badge que un
    // nombre de fichero hostil (mismo idioma que `draw_search_dialog`'s
    // root line).
    let field = if hostile {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // #103 T9 review MINOR: este modal no pasa por `DialogHints` (texto
    // libre, sin ALLOWLIST que generar un pie de página) — como
    // `search-hint`/`palette-hint`, sus teclas van fijas en Fluent.
    let mut lines = vec![
        field,
        t("modal-mark-pattern-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    let title = if mark {
        t("modal-mark-pattern-add")
    } else {
        t("modal-mark-pattern-remove")
    };
    (title, lines.join("\n"))
}

/// Título+cuerpo de CUALQUIER prompt de texto libre de una sola línea:
/// campo enmascarado + hint + la línea de teclas compartida + el diagnóstico
/// si lo hay.
///
/// Los tres prompts que había (`Mkdir` #104, `AiRenameInstruction` M4-IA,
/// `SemanticQuery` M4-IA-2) eran ya LA MISMA función con ids distintos, y S4
/// (#135) traía un cuarto: cuatro copias son cuatro sitios donde olvidar el
/// enmascarado, que es lo único que aquí importa (el campo y el diagnóstico
/// son texto de USUARIO — un paste con bidi/invisibles llega tan fácil a una
/// consulta como a un nombre, y el error del engine puede embeber el nombre).
/// La línea de teclas es compartida a propósito (FIX-A de la review T4): así
/// los cuatro cuadran con el brazo de altura conjunto (7 con error / 6 sin
/// él) en vez de pintar uno una línea menos.
pub(crate) fn free_text_modal_text(
    title_id: &str,
    hint_id: &str,
    value: &str,
    error: Option<&str>,
) -> (String, String) {
    let (masked, hostile) = display_name(value.as_bytes());
    // Ventana anclada a la DERECHA (review de S4, M4): el cuerpo del modal es
    // un `Paragraph` sin wrap y de ancho acotado, así que un valor largo
    // pintaba solo su cabeza y dejaba el cursor `_` fuera de pantalla — con
    // una línea de comandos eso es pulsar Enter sin ver lo que se ejecuta.
    // Se recorta por delante, marcando el corte, que es lo que hace cualquier
    // editor de una línea.
    let visible = tail_window(&masked, FREE_TEXT_FIELD_MAX);
    let field = if hostile {
        format!("{HOSTILE_BADGE} {visible}_")
    } else {
        format!("{visible}_")
    };
    let mut lines = vec![field, t(hint_id), t("modal-mark-pattern-keys")];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    (t(title_id), lines.join("\n"))
}

/// Chars visibles del campo de un prompt de texto libre. Mismo presupuesto
/// que [`MODAL_PATH_CHARS`] (la caja del modal mide 60 y los bordes se llevan
/// cuatro columnas), con holgura para el badge y la marca de corte.
pub(crate) const FREE_TEXT_FIELD_MAX: usize = 50;

/// Título+cuerpo de `Modal::AiRenamePlan` (M4-IA, doctrina encoding-auditor):
/// primera línea = el dir OBJETIVO etiquetado fuera de banda (audit MAJOR-1
/// — el humano decide sabiendo DÓNDE aterriza el plan); después la VENTANA
/// de [`AI_RENAME_PAIR_LIMIT`] parejas desde `offset` (audit MAJOR-3: el
/// plan entero es revisable por scroll). Cada nombre en SU línea — el `from`
/// con etiqueta numerada ABSOLUTA fuera de banda (audit MINOR-4, corpus
/// `arrow_join_spoof`: un nombre puede imitar la flecha, no el `n.` al
/// margen), el `→` del destino al INICIO de su línea — elipsis media (un
/// `from` kilométrico no expulsa el `to` de la caja) y enmascarado MARCADO
/// con badge ([`badge_prefixed`], Rust-side). El indicador de desbordamiento
/// lleva badge si alguna pareja OCULTA es hostil (lo escondido no se cuela
/// limpio). Aunque el engine garantiza UTF-8 en el wire, un daemon
/// N+1/comprometido podría mandar cualquier cosa — se pinta a la defensiva
/// SIEMPRE, como el modal de aprobación.
///
/// Bajo las parejas va el veredicto del LOTE (spec §17): el estado del plan
/// que contestó `fs.rename_batch_plan` (en vuelo / aplicable / no
/// aplicable), cuántos pasos son maquinaria del planificador —el NÚMERO, no
/// los nombres `.norte-rename-…`, que nadie pidió— y las colisiones, UNA POR
/// LÍNEA con el nombre ofensor el ÚLTIMO campo (un recorte jamás puede
/// comerse el veredicto) y su índice de pareja ABSOLUTO fuera de banda, que
/// es lo que hace señalable la fila culpable. Un veredicto de un daemon más
/// nuevo degrada ESA línea a una etiqueta genérica, jamás el modal entero.
pub(crate) fn ai_rename_plan_modal_text(
    dir: &norte_proto::VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
    offset: usize,
    dialog_hints: &crate::hints::DialogHints,
    plan: &norte_frontend::BatchPlan,
) -> (String, String) {
    // Cinturón de render: el clamp vive en `App::ai_plan_scroll`, pero un
    // offset fuera de rango jamás debe pintar una ventana vacía.
    let offset = offset.min(entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT));
    let last = (offset + AI_RENAME_PAIR_LIMIT).min(entries.len());
    let (dir_txt, dir_hostile) = norte_frontend::path_display(dir);
    let mut lines = vec![badge_prefixed(
        dir_hostile,
        ta(
            "modal-ai-rename-dir",
            &[("dir", &middle_ellipsis(&dir_txt, 46))],
        ),
    )];
    // El VEREDICTO del lote va arriba, pegado al dir y ANTES de las parejas
    // (§17): un modal más alto que el terminal lo recorta `centered` por
    // ABAJO, y de todas las líneas del cuerpo esta es la que no puede
    // perderse — es la que dice si esto va a renombrar algo.
    lines.push(t(plan.status_key()));
    for (i, e) in entries.iter().enumerate().take(last).skip(offset) {
        let (from, from_hostile) = display_name(e.from.as_bytes());
        let (to, to_hostile) = display_name(e.to.as_bytes());
        lines.push(badge_prefixed(
            from_hostile,
            ta(
                "modal-ai-rename-pair-from",
                &[
                    ("n", &(i + 1).to_string()),
                    ("from", &middle_ellipsis(&from, 46)),
                ],
            ),
        ));
        lines.push(badge_prefixed(
            to_hostile,
            ta(
                "modal-ai-rename-pair-to",
                &[("to", &middle_ellipsis(&to, 44))],
            ),
        ));
    }
    if entries.len() > AI_RENAME_PAIR_LIMIT {
        let hidden_hostile = entries.iter().enumerate().any(|(i, e)| {
            (i < offset || i >= last)
                && (display_name(e.from.as_bytes()).1 || display_name(e.to.as_bytes()).1)
        });
        lines.push(badge_prefixed(
            hidden_hostile,
            ta(
                "modal-ai-rename-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &entries.len().to_string()),
                ],
            ),
        ));
    }
    // El saneado del detalle (enmascarado, elipsis, índice de pareja, tope
    // de colisiones) vive en `norte-frontend` para que lo compartan TODAS las
    // superficies: son nombres que controla un atacante, y una política
    // duplicada por frontend se desvía en uno de ellos sin que nada avise.
    // Este frontend solo pone SU badge.
    // En PARTES, y el nombre en su propia línea (#273): componer
    // `✗ 3. ya existe: <nombre>` dejaba que un fichero llamado
    // `✗ 4. ya existe: otro.txt` fabricara una entrada de la lista. Aquí no
    // hay elementos hermanos que separen la causa del nombre, así que los
    // separa el SALTO DE LÍNEA, y solo el nombre lleva el badge.
    for parte in plan.detail_parts(entries.len(), norte_i18n::active()) {
        match parte {
            norte_frontend::DetailPart::Temp { count } => {
                lines.push(ta("modal-rename-batch-temp", &[("n", &count.to_string())]));
            }
            norte_frontend::DetailPart::Collision {
                index,
                kind_key,
                name,
                hostile,
            } => {
                let kind = t(kind_key);
                lines.push(match index {
                    Some(n) => ta(
                        "modal-rename-batch-collision-prefix",
                        &[("n", &n.to_string()), ("kind", &kind)],
                    ),
                    None => ta(
                        "modal-rename-batch-collision-prefix-unindexed",
                        &[("kind", &kind)],
                    ),
                });
                lines.push(format!("  {}", badge_prefixed(hostile, name)));
            }
            norte_frontend::DetailPart::More {
                shown,
                total,
                hostile,
            } => lines.push(badge_prefixed(
                hostile,
                ta(
                    "modal-rename-batch-collision-more",
                    &[("shown", &shown.to_string()), ("total", &total.to_string())],
                ),
            )),
        }
    }
    // H3c: con una ayuda encima, `y`/`n` no responden — el pie dice eso en
    // vez de ofrecerlos (gemelo de `DialogHints::with_modals_inert`, para los
    // dos modales cuya pista es prosa y no hint generado).
    //
    // Sin ayuda encima el pie sigue al gate de `dialog_action`: con un plan
    // que no se puede aplicar, confirmar está mudo y ofrecerlo sería un pie
    // que miente (misma doctrina que `modals_inert`).
    lines.push(if dialog_hints.modals_inert {
        t("modal-hint-help-open")
    } else if plan.confirmable() {
        t("modal-ai-rename-plan-hint")
    } else {
        t("modal-rename-batch-plan-hint-blocked")
    });
    (t("modal-ai-rename-plan"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::SemanticHits` (M4-IA-2, doctrina
/// encoding-auditor, molde `ai_rename_plan_modal_text`): la VENTANA de
/// [`SEMANTIC_HIT_LIMIT`] hits desde `offset`, un hit POR LÍNEA con marcador
/// de cursor (`>`) y etiqueta numerada ABSOLUTA fuera de banda, path por
/// `norte_frontend::path_display` (mask + flag hostil) con badge Rust-side
/// ([`badge_prefixed`]) y elipsis media (un path kilométrico no expulsa el
/// score de la caja); el score `{:.2}` al final. El indicador de
/// desbordamiento lleva badge si algún hit OCULTO es hostil (lo escondido no
/// se cuela limpio). Aunque el engine garantiza el wire, un daemon
/// N+1/comprometido podría mandar cualquier cosa — se pinta a la defensiva
/// SIEMPRE.
pub(crate) fn semantic_hits_modal_text(
    hits: &[norte_proto::methods::SemanticHit],
    offset: usize,
    cursor: usize,
    dialog_hints: &crate::hints::DialogHints,
) -> (String, String) {
    // Cinturón de render: el clamp vive en `App::semantic_cursor`, pero un
    // offset fuera de rango jamás debe pintar una ventana vacía.
    let offset = offset.min(hits.len().saturating_sub(SEMANTIC_HIT_LIMIT));
    let last = (offset + SEMANTIC_HIT_LIMIT).min(hits.len());
    let mut lines = Vec::new();
    for (i, h) in hits.iter().enumerate().take(last).skip(offset) {
        let (path, hostile) = norte_frontend::path_display(&h.path);
        let line = badge_prefixed(
            hostile,
            ta(
                "modal-semantic-hit",
                &[
                    ("n", &(i + 1).to_string()),
                    ("path", &middle_ellipsis(&path, 44)),
                    ("score", &format!("{:.2}", h.score)),
                ],
            ),
        );
        // Marcador de cursor FUERA de banda, en columna fija ANTES del badge
        // (un path no puede imitarlo: va enmascarado y tras la etiqueta).
        lines.push(if i == cursor {
            format!("> {line}")
        } else {
            format!("  {line}")
        });
    }
    if hits.len() > SEMANTIC_HIT_LIMIT {
        let hidden_hostile = hits
            .iter()
            .enumerate()
            .any(|(i, h)| (i < offset || i >= last) && norte_frontend::path_display(&h.path).1);
        lines.push(badge_prefixed(
            hidden_hostile,
            ta(
                "modal-semantic-more",
                &[
                    ("shown", &last.to_string()),
                    ("total", &hits.len().to_string()),
                ],
            ),
        ));
    }
    // H3c: ver `ai_rename_plan_modal_text` — misma razón, misma cadena.
    lines.push(if dialog_hints.modals_inert {
        t("modal-hint-help-open")
    } else {
        t("modal-semantic-hits-hint")
    });
    (t("modal-semantic-hits"), lines.join("\n"))
}

/// Título+cuerpo de `Modal::TransferName` (#105): mismo contrato de
/// enmascarado que `mkdir_modal_text` — el dir destino, el nombre y el
/// diagnóstico son texto/bytes de usuario. El dir va en su propia línea
/// (jamás un joiner in-band con el nombre — disciplina de los modales de
/// #103).
/// Lo que se sabe del DESTINO de una transferencia, para pintarlo.
///
/// Las dos juntas porque son la misma clase de línea —un hecho del destino que
/// conviene saber antes de decir que sí— y porque van seguidas, en ese orden.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DestNotices<'a> {
    /// «No cabe» (#149). `None` = cabe, o no se sabe cuánto ocupa.
    pub space: Option<&'a str>,
    /// «Este destino no puede confinar las escrituras» (#164, #219).
    pub confine: Option<&'a str>,
}

/// Título y cuerpo de `Modal::ConfirmTransfer`, con papeles.
///
/// **Por qué el destino deja de marcarse con una flecha.** Su línea era
/// `→ ⟨file⟩/otro/ruta`, encima de una lista de NOMBRES ajenos, y el comentario
/// afirmaba que ningún nombre podía imitarla. Se apoyaba en dos cosas: que un
/// nombre no puede llevar `/` —cierto en los tres SO— y en el prefijo del
/// esquema. Lo segundo ya no está para lo local, y lo primero tiene
/// homóglifos: `∕` (U+2215), `⁄` (U+2044) y `／` (U+FF0F) son legales en ext4,
/// APFS y NTFS, no son peligro de terminal, no se enmascaran y no marcan. Un
/// fichero llamado `→ ∕srv∕publico` fabrica esa línea entera.
///
/// El papel va ahora en el ESTILO —el destino es `Strong`, las filas de la
/// lista son `Plain`— y eso un nombre no lo puede fabricar, porque no lo
/// escribe él. La etiqueta es la misma que usa `transfer_name_modal`.
fn confirm_transfer_modal(
    kind: crate::app::TransferKind,
    items: &[norte_proto::VPath],
    to: &norte_proto::VPath,
    reinterpret: Option<norte_encoding::NameEncoding>,
    destino: DestNotices<'_>,
    teclas: &str,
) -> (String, ModalBody) {
    let title = match kind {
        crate::app::TransferKind::Copy => t("modal-copy-title"),
        crate::app::TransferKind::Move => t("modal-move-title"),
    };
    let mut lines: ModalBody = norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret)
        .into_iter()
        .map(ModalLine::plain)
        .collect();
    let (ruta, hostil) = norte_frontend::path_display_with(to, reinterpret);
    lines.push(
        ModalLine::new(
            format!("{}  {}", t("modal-transfer-to"), ruta),
            LineKind::Strong,
        )
        .hostile(hostil),
    );
    // #149/#164: los dos hechos del destino, debajo de él y encima de las
    // teclas — lo último que se lee antes de decidir. Ninguno bloquea nada.
    lines.extend(destino.space.map(|s| ModalLine::new(s, LineKind::Warning)));
    lines.extend(
        destino
            .confine
            .map(|s| ModalLine::new(s, LineKind::Warning)),
    );
    lines.push(ModalLine::new(teclas, LineKind::Dim));
    (title, lines)
}

/// Título y cuerpo de `Modal::TransferName` (#105), con papeles.
///
/// **Qué se lee y en qué orden**, que es lo que este modal hacía mal: el campo
/// editable iba el tercero, sin nada que lo distinguiera del texto de al lado,
/// y su etiqueta DEBAJO — o sea que leías un nombre, y después te enterabas de
/// que era lo único que podías cambiar. Ahora va lo de dónde a dónde, luego la
/// etiqueta, luego el campo, y las teclas al final.
///
/// **El origen enseña el DIRECTORIO, no el fichero.** El nombre estaba dos
/// veces —truncado arriba y entero en el campo—, y en un cuerpo de cinco
/// líneas eso era repetir el 40%. El nombre vive en el campo, que es donde se
/// edita.
///
/// **Etiquetas fuera de banda, sin la flecha.** El destino se marcaba con un
/// `→` al principio de su línea; `→` (U+2192) es legítimo en un nombre, no es
/// peligro de terminal y por tanto no se enmascara ni se marca, así que un
/// directorio llamado `docs → /casa/BORRAR` fabricaba una línea que se lee
/// como dos rutas. Es lo que dice la fixture `arrow_join_spoof` del corpus y
/// lo que el host ya hacía en `DialogView::destination`: la etiqueta va en su
/// columna, jamás dentro del texto.
///
/// Mismo contrato de enmascarado que `mkdir_modal_text` — el dir destino, el
/// nombre y el diagnóstico son texto/bytes de usuario.
pub(crate) fn transfer_name_modal(
    kind: crate::app::TransferKind,
    from: &norte_proto::VPath,
    to_dir: &norte_proto::VPath,
    name: &str,
    error: Option<&str>,
    enc: Option<norte_encoding::NameEncoding>,
    destino: DestNotices<'_>,
) -> (String, ModalBody) {
    let (masked, hostile) = display_name(name.as_bytes());
    // La COLA, con la marca del corte: es lo mismo que hace todo prompt de
    // texto libre (`free_text_modal_text`) y por el mismo motivo, que este
    // modal nunca recibió. Sin acotar, un nombre más ancho que la caja se
    // cortaba contra el borde a pelo: se perdía la cola, se perdía el cursor,
    // y a partir de ahí teclear no cambiaba nada en pantalla — o sea confirmar
    // un nombre que no se ve. El fondo del campo, que ahora llega al borde, lo
    // disimulaba aún mejor.
    let visible = tail_window(&masked, FREE_TEXT_FIELD_MAX);
    // `_` marca dónde acaba lo tecleado. Se probó `▏` (U+258F) por no
    // confundirse con un guion bajo del propio nombre, y fue peor: es
    // East_Asian_Width=Ambiguous, así que en un terminal CJK con
    // ambiguous-width doble ocupa DOS celdas, `modal_width` presupuesta una y
    // la marca de fin de campo es lo primero que se recorta. `_` es `Na`,
    // inequívoco, y es el que usan los otros cinco campos.
    //
    // La marca de alterado NO se mete aquí dentro: viaja en la línea, que la
    // pinta con su propio rol y su contraste garantizado.
    let field = format!("{visible}_");
    // La ruta va con ELIPSIS MEDIA, como en el modal de aprobación y el de
    // colisión: `modal_width` topa contra el ancho del frame y el `Paragraph`
    // de `draw_modal` no envuelve, así que una ruta honda se cortaba a pelo
    // contra el borde y expulsaba de la caja la COLA del destino — justo lo
    // que el usuario necesita ver para saber dónde aterriza la copia — sin ni
    // un `…` que lo delatara. Bajo la reinterpretación CAPTURADA al abrir
    // (#98/M1), jamás la del pane al pintar.
    let etiqueta_de = t("modal-transfer-from");
    let etiqueta_a = t("modal-transfer-to");
    let ancho = etiqueta_de.width().max(etiqueta_a.width());
    // La marca sale APARTE del texto: la línea la lleva en su campo y se pinta
    // con `Role::HostileBadge`. Dentro del texto heredaba el estilo del papel,
    // y con la línea de origen atenuada la señal de spec §6 quedaba a 2,3:1
    // sobre un tema claro.
    let con_etiqueta = |etiqueta: &str, p: &norte_proto::VPath| {
        let (linea, hostil) = norte_frontend::path_display_with(p, enc);
        let linea = middle_ellipsis(&linea, MODAL_PATH_CHARS);
        let pad = " ".repeat(ancho.saturating_sub(etiqueta.width()));
        (format!("{etiqueta}{pad}  {linea}"), hostil)
    };
    // De DÓNDE sale. El DIRECTORIO cuando el nombre está en el campo, porque
    // entonces enseñar la ruta entera lo repetía. Pero un RENAME abre con
    // `to_dir = from.parent()` —es la misma carpeta, no se mueve nada— así que
    // ahí el directorio no nombra nada: en cuanto el usuario teclea, el campo
    // pasa a ser el nombre NUEVO y el que se está renombrando no aparece en
    // ninguna parte de la pantalla. Eso es confirmar a ciegas una mutación
    // cuyo operando no se ve, que es exactamente lo que prohíbe la ADR 0070.
    // Con el origen y el destino en la misma carpeta, la línea «De» lleva la
    // ruta ENTERA.
    let renombra = from.parent().as_ref() == Some(to_dir);
    let origen = if renombra {
        from.clone()
    } else {
        from.parent().unwrap_or_else(|| from.clone())
    };
    let (texto_de, de_hostil) = con_etiqueta(&etiqueta_de, &origen);
    let (texto_a, a_hostil) = con_etiqueta(&etiqueta_a, to_dir);
    let mut lines = vec![
        ModalLine::new(texto_de, LineKind::Dim).hostile(de_hostil),
        // El destino es lo que hay que leer antes de decir que sí.
        ModalLine::new(texto_a, LineKind::Strong).hostile(a_hostil),
        ModalLine::plain(""),
        ModalLine::new(t("modal-transfer-name-hint"), LineKind::Dim),
        ModalLine::new(field, LineKind::Field).hostile(hostile),
        ModalLine::plain(""),
    ];
    // Los dos avisos del destino, en el mismo sitio y en el mismo orden que en
    // `ConfirmTransfer`: debajo del destino y encima de las teclas, que es lo
    // último que se lee antes de decidir (#149, #164). Copiar UN fichero no
    // los tenía, y por eso una hoja suelta se copiaba sin saber si el destino
    // sujeta sus escrituras (#343).
    lines.extend(destino.space.map(|s| ModalLine::new(s, LineKind::Warning)));
    lines.extend(
        destino
            .confine
            .map(|s| ModalLine::new(s, LineKind::Warning)),
    );
    lines.push(ModalLine::new(t("modal-mark-pattern-keys"), LineKind::Dim));
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(ModalLine::new(masked_err, LineKind::Error));
    }
    let title = match kind {
        crate::app::TransferKind::Copy => t("modal-transfer-name-copy"),
        crate::app::TransferKind::Move => t("modal-transfer-name-move"),
    };
    (title, lines)
}

#[cfg(test)]
mod transfer_name_modal_text_tests {
    use super::{LineKind, ModalBody, transfer_name_modal};
    use crate::app::TransferKind;
    use norte_proto::VPath;

    /// El cuerpo como una sola cadena, para las aserciones de enmascarado.
    fn texto(body: &ModalBody) -> String {
        body.iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #105 review MINOR-2 (misma clase que el M4 del patrón): fn PURA — un
    /// RLO crudo en nombre y error sale enmascarado, y un byte hostil en el
    /// ORIGEN y el dir destino jamás llega crudo (`path_display` los enmascara
    /// y llevan badge).
    #[test]
    fn masks_every_user_surface() {
        let hostile = "abc\u{202E}rid";
        // El byte hostil va en el DIRECTORIO de origen, que es lo que este
        // modal enseña ahora: el nombre del fichero vive en el campo.
        let from = VPath::parse("mem:///src%FF/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst%FE").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Move,
            &from,
            &to_dir,
            hostile,
            Some(hostile),
            None,
            super::DestNotices::default(),
        );
        let body = texto(&body);
        assert!(!body.contains('\u{202E}'), "{body:?}");
        assert!(
            body.matches('\u{FFFD}').count() >= 4,
            "nombre + error (RLO) y origen + destino (bytes): {body:?}"
        );
    }

    /// La marca de alterado va en el CAMPO de la línea, no dentro del texto:
    /// se pinta con `Role::HostileBadge`, cuyo contraste el gate de
    /// `norte-theme` garantiza en los ocho presets. Dentro del texto heredaba
    /// el estilo del papel de su línea, y una línea atenuada dejaba la señal
    /// de spec §6 a 2,3:1 sobre un tema claro.
    #[test]
    fn la_marca_de_alterado_va_aparte_del_texto() {
        let hostile = "abc\u{202E}rid";
        let from = VPath::parse("mem:///src%FF/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst%FE").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Move,
            &from,
            &to_dir,
            hostile,
            None,
            None,
            super::DestNotices::default(),
        );
        assert!(
            !texto(&body).contains(super::HOSTILE_BADGE),
            "la marca no se mete en el texto: {:?}",
            texto(&body)
        );
        // Origen, destino y campo: los tres llevan bytes alterados y los tres
        // lo dicen.
        assert_eq!(body.iter().filter(|l| l.hostile).count(), 3, "{body:?}");
        // Y la anchura de la línea cuenta la marca: si no, la caja sale corta
        // justo en las líneas que llevan una.
        let marcada = body.iter().find(|l| l.hostile).expect("hay marcada");
        assert!(marcada.width() > unicode_width::UnicodeWidthStr::width(marcada.text.as_str()));
    }

    /// **El campo se declara CAMPO, y una sola línea lo es.**
    ///
    /// Es lo único que el usuario puede cambiar, y sin papel salía del mismo
    /// color que el texto de al lado: leías un nombre y no había nada que
    /// dijera que era editable. Su etiqueta va JUSTO ENCIMA — estaba debajo,
    /// así que se leía el valor antes de saber qué era.
    #[test]
    fn el_campo_se_declara_campo_y_su_etiqueta_va_encima() {
        let from = VPath::parse("mem:///src/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            "a.txt",
            None,
            None,
            super::DestNotices::default(),
        );

        let campos: Vec<usize> = body
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == LineKind::Field)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(campos.len(), 1, "exactamente una línea es el campo");
        let i = campos[0];
        assert!(body[i].text.contains("a.txt"));
        assert_eq!(
            body[i - 1].kind,
            LineKind::Dim,
            "y justo encima va su etiqueta, atenuada"
        );

        // El destino se destaca; el origen no compite con él.
        assert_eq!(body[0].kind, LineKind::Dim, "de dónde sale");
        assert_eq!(body[1].kind, LineKind::Strong, "a dónde va");
    }

    /// **El nombre del fichero NO se repite**: arriba va el DIRECTORIO de
    /// origen, porque el nombre está en el campo. Salía en las dos, y en un
    /// cuerpo de cinco líneas eso era repetir el 40%.
    #[test]
    fn el_origen_ensena_el_directorio_no_el_fichero() {
        let from = VPath::parse("mem:///src/unico.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            "unico.txt",
            None,
            None,
            super::DestNotices::default(),
        );
        assert_eq!(
            texto(&body).matches("unico.txt").count(),
            1,
            "una vez, en el campo: {:?}",
            texto(&body)
        );
        assert!(body[0].text.contains("/src"), "{:?}", body[0].text);
    }

    /// **Un rename NOMBRA el fichero que renombra** (ADR 0070).
    ///
    /// Un rename abre con `to_dir = from.parent()`: la misma carpeta, no se
    /// mueve nada. Enseñando solo el directorio, «De» y «A» salían idénticos y
    /// el nombre que se está cambiando no aparecía en NINGUNA parte en cuanto
    /// el usuario tecleaba — el campo pasa a ser el nombre nuevo. Eso es
    /// confirmar a ciegas una mutación cuyo operando no se ve, y con la marca
    /// consumida al enviar.
    #[test]
    fn un_rename_nombra_el_fichero_que_renombra() {
        let from = VPath::parse("mem:///casa/docs/contrato-final.pdf").unwrap();
        let to_dir = from.parent().expect("padre");
        let (_, body) = transfer_name_modal(
            TransferKind::Move,
            &from,
            &to_dir,
            // Ya tecleado: el campo es el nombre NUEVO.
            "contrato-v2.pdf",
            None,
            None,
            super::DestNotices::default(),
        );
        assert!(
            body[0].text.contains("contrato-final.pdf"),
            "el operando tiene que estar escrito: {:?}",
            body[0].text
        );
        assert_ne!(
            body[0].text, body[1].text,
            "y las dos líneas no pueden decir lo mismo"
        );
    }

    /// **Ni una flecha dentro del texto.** `→` es legítimo en un nombre y no
    /// se enmascara, así que marcaba el destino con un carácter que un
    /// directorio puede llevar: `docs → /casa/BORRAR` fabricaba una línea que
    /// se lee como dos rutas (fixture `arrow_join_spoof`). La etiqueta va en
    /// su columna.
    #[test]
    fn la_etiqueta_del_destino_va_fuera_de_banda() {
        let from = VPath::parse("mem:///src/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            "a.txt",
            None,
            None,
            super::DestNotices::default(),
        );
        assert!(!texto(&body).contains('→'), "{:?}", texto(&body));
    }

    /// **Un nombre más ancho que la caja enseña su COLA, con la marca.**
    ///
    /// Sin acotar se cortaba contra el borde a pelo: se perdía la cola, se
    /// perdía el `_` que dice dónde estás escribiendo, y a partir de ahí
    /// teclear no cambiaba nada en pantalla. El relleno del fondo hasta el
    /// borde lo disimulaba todavía mejor — la caja se ve completa y lo que hay
    /// dentro no es el nombre. Es el mismo arreglo que ya tenían los otros
    /// cinco campos (`tail_window`), y este no lo había recibido.
    #[test]
    fn un_nombre_mas_ancho_que_la_caja_ensena_su_cola() {
        let largo = "a".repeat(101);
        let from = VPath::parse("mem:///src/x").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        let (_, body) = transfer_name_modal(
            TransferKind::Copy,
            &from,
            &to_dir,
            &largo,
            None,
            None,
            super::DestNotices::default(),
        );
        let campo = body
            .iter()
            .find(|l| l.kind == LineKind::Field)
            .expect("hay campo");
        assert!(
            campo.text.contains('…'),
            "el corte se DICE: {:?}",
            campo.text
        );
        assert!(
            campo.text.ends_with('_'),
            "y el cursor sobrevive al recorte: {:?}",
            campo.text
        );
        assert!(
            campo.width() <= super::FREE_TEXT_FIELD_MAX + 2,
            "acotado como los otros cinco campos: {}",
            campo.width()
        );
    }

    /// **El alto declarado cubre el cuerpo que se compone.**
    ///
    /// `modal_height` es una segunda fuente de verdad —el rustdoc del módulo
    /// lleva desde el principio diciendo que si las dos se desincronizan el
    /// modal se recorta— y no había un solo test que las atara. Se comprueba
    /// con y sin cada línea opcional, que es justo donde se desincronizan.
    #[test]
    fn el_alto_declarado_cubre_el_cuerpo() {
        use crate::app::Modal;
        let from = VPath::parse("mem:///src/a.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst").unwrap();
        for error in [None, Some("mal".to_owned())] {
            for space in [None, Some("no cabe".to_owned())] {
                for confine in [None, Some("no confina".to_owned())] {
                    let modal = Modal::TransferName {
                        kind: TransferKind::Copy,
                        from: from.clone(),
                        to_dir: to_dir.clone(),
                        name: "a.txt".to_owned(),
                        original: b"a.txt".to_vec(),
                        touched: false,
                        from_marks: false,
                        enc: None,
                        error: error.clone(),
                        space: space.clone(),
                        confine: confine.clone(),
                    };
                    let (_, body) = super::modal_title_body(
                        &modal,
                        None,
                        &crate::hints::DialogHints::default(),
                    );
                    let alto = super::modal_height(&modal);
                    assert!(
                        usize::from(alto) >= body.len() + 2,
                        "el cuerpo ({} líneas) no cabe en {alto} filas con sus \
                         bordes: la última se recorta sin decirlo",
                        body.len()
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod free_text_modal_text_tests {
    use super::{free_text_modal_text, tail_window};

    /// Cada prompt de texto libre usa SUS ids y los cuatro existen en ambos
    /// locales (review de S4, m5). Sin esto, un id con typo se pintaría tal
    /// cual en pantalla —Fluent cae al propio id— con el gate en verde.
    #[test]
    fn cada_prompt_resuelve_su_titulo_y_su_hint() {
        let cases = [
            ("modal-mkdir", "modal-mkdir-hint"),
            ("modal-command-line", "modal-command-line-hint"),
            ("modal-ai-rename", "modal-ai-rename-hint"),
            ("modal-semantic", "modal-semantic-hint"),
        ];
        for (titulo, hint) in cases {
            let (t, body) = free_text_modal_text(titulo, hint, "x", None);
            assert_ne!(t, titulo, "{titulo} sin traducción: sale el id crudo");
            let hint_line = body.lines().nth(1).expect("hint");
            assert_ne!(hint_line, hint, "{hint} sin traducción: sale el id crudo");
        }
    }

    /// El campo enseña la COLA, con la marca del corte, para que el cursor
    /// esté siempre a la vista: un comando cuyo final no se ve es un comando
    /// que se ejecuta a ciegas.
    #[test]
    fn un_valor_largo_ensena_su_cola_y_marca_el_corte() {
        let long = "a".repeat(300);
        let (_, body) =
            free_text_modal_text("modal-command-line", "modal-command-line-hint", &long, None);
        let field = body.lines().next().expect("campo");
        assert!(field.starts_with('…'), "el corte se marca: {field:?}");
        assert!(field.ends_with('_'), "y el cursor se ve: {field:?}");
        assert!(
            field.chars().count() <= 52,
            "acotado: {}",
            field.chars().count()
        );
    }

    /// **`tail_window` cuenta CELDAS de terminal.**
    ///
    /// Contaba chars, y para lo que este presupuesto protege —que el campo
    /// quepa en su caja— esa es la medida equivocada: cincuenta chars de CJK
    /// son CIEN celdas, así que un nombre japonés desbordaba igual y el
    /// recorte contra el borde se llevaba el cursor del final. Lo destapó la
    /// primera foto del modal de transferencia.
    #[test]
    fn la_ventana_de_cola_cuenta_celdas() {
        assert_eq!(tail_window("abc", 10), "abc");
        assert_eq!(tail_window("abcdef", 3), "…ef");

        // Ocho ideogramas son DIECISÉIS celdas: con cuatro de presupuesto
        // caben el `…` y uno solo, no cuatro.
        let cjk = "日本語のファイル";
        let w = tail_window(cjk, 4);
        assert!(w.starts_with('…'), "{w:?}");
        assert!(
            norte_frontend::cells(&w) <= 4,
            "{w:?} mide {} celdas",
            norte_frontend::cells(&w)
        );

        // Y lo que cabe entero no se toca, se mida como se mida.
        assert_eq!(tail_window(cjk, 16), cjk);
    }

    /// Mismo pin que el del patrón (#103 M4): fn PURA — un RLO crudo en el
    /// nombre Y en el error sale enmascarado en AMBAS líneas.
    #[test]
    fn masks_a_raw_rtl_override_in_name_and_error() {
        let hostile = "abc\u{202E}rid";
        let (_, body) =
            free_text_modal_text("modal-mkdir", "modal-mkdir-hint", hostile, Some(hostile));
        assert!(!body.contains('\u{202E}'), "{body:?}");
        assert_eq!(body.matches('\u{FFFD}').count(), 2, "{body:?}");
    }
}

#[cfg(test)]
mod mark_pattern_modal_text_tests {
    use super::mark_pattern_modal_text;

    /// Review MAJOR M4: `mark_pattern_modal_text` es pura — testear el
    /// enmascarado directamente en vez de a través de un buffer
    /// `TestBackend`, donde el renderer de párrafo de ratatui se COME los
    /// grafemas de ancho cero: U+202E jamás sobrevive AHÍ, enmascarado o
    /// no, así que una aserción de test de render contra él no puede fallar
    /// nunca (la clase de bug que motivó este test). Un patrón Y un error
    /// que llevan un RLO crudo deben salir enmascarados los DOS: ni un
    /// U+202E sobrevive, y U+FFFD aparece exactamente dos veces — una por
    /// línea enmascarada.
    #[test]
    fn masks_a_raw_rtl_override_in_both_the_pattern_and_the_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, body) = mark_pattern_modal_text(true, hostile, Some(hostile));
        assert!(
            !body.contains('\u{202E}'),
            "raw RTL override must not survive: {body:?}"
        );
        assert_eq!(
            body.matches('\u{FFFD}').count(),
            2,
            "one U+FFFD per masked line (pattern + error): {body:?}"
        );
    }

    /// Sin error, solo la línea del patrón se enmascara: un solo U+FFFD.
    #[test]
    fn masks_only_the_pattern_line_when_there_is_no_error() {
        let hostile = "abc\u{202E}gpj.exe";
        let (_, body) = mark_pattern_modal_text(true, hostile, None);
        assert_eq!(body.matches('\u{FFFD}').count(), 1);
    }
}

/// Texto `(título, cuerpo)` del modal TOFU (#45). host/algo/fingerprint
/// vienen del SERVIDOR REMOTO (no confiable) y esto es una decisión de
/// seguridad: mismo enmascarado que las rutas de agente (controles/bidi/
/// invisibles → �) + clamp. El fingerprint legítimo es ASCII
/// (`SHA256:<base64>`), así que el enmascarado es un no-op salvo que el
/// server intente ocultar caracteres — en cuyo caso el � DELATA la
/// manipulación.
pub(crate) fn trust_host_modal_text(
    host: &str,
    port: Option<u16>,
    algo: &str,
    fingerprint: &str,
    hint: &str,
) -> (String, String) {
    let (host_txt, host_hostile) = display_name(host.as_bytes());
    let hostport = match port {
        Some(p) => format!("{}:{p}", clamp_chars(&host_txt, 48)),
        None => clamp_chars(&host_txt, 48),
    };
    let (algo_disp, algo_hostile) = display_name(algo.as_bytes());
    let algo_txt = clamp_chars(&algo_disp, 24);
    let (fp_txt, fp_hostile) = display_name(fingerprint.as_bytes());
    let lines = [
        ta(
            "modal-trust-host-host",
            &[
                ("badge", if host_hostile { HOSTILE_BADGE } else { "" }),
                ("host", &hostport),
            ],
        ),
        ta(
            "modal-trust-host-algo",
            &[
                ("badge", if algo_hostile { HOSTILE_BADGE } else { "" }),
                ("algo", &algo_txt),
            ],
        ),
        ta(
            "modal-trust-host-fp",
            &[
                ("badge", if fp_hostile { HOSTILE_BADGE } else { "" }),
                ("fingerprint", &clamp_chars(&fp_txt, 52)),
            ],
        ),
        t("modal-trust-host-note"),
        hint.to_owned(),
    ];
    (t("modal-trust-host-title"), lines.join("\n"))
}

/// Cuántos puntos como mucho pinta el campo de [`crate::app::Modal::AskSecret`].
///
/// Un tope y no el largo real porque una passphrase de 200 caracteres
/// desbordaría la caja. **No esconde la longitud**: por debajo del tope hay un
/// punto por carácter, que es exactamente el largo — y se queda así a
/// propósito, porque ver aparecer un punto es la única confirmación de que la
/// tecla entró en un campo que no enseña nada.
const SECRET_DOTS_MAX: usize = 32;

/// Título y cuerpo del diálogo de contraseña (#325).
///
/// Pinta la conexión, **a dónde se conecta** y un punto por carácter tecleado
/// (hasta [`SECRET_DOTS_MAX`]). Es la única función de esta familia que recibe
/// un secreto, y lo único que hace con él es contarlo.
///
/// El endpoint no es decoración: un diálogo de contraseña que solo dice
/// `conexión: trabajo` no se puede contestar con criterio — el nombre lo eligió
/// `connections.toml`, que puede venir de un dotfiles ajeno o de una línea
/// editada, y `trabajo` no dice si esa entrada apunta hoy donde apuntaba ayer.
/// Es la misma razón por la que el TOFU de host key enseña la huella. Viene
/// del core ya redactado (sin userinfo) y se sanea aquí como todo lo demás.
pub(crate) fn ask_secret_modal_text(
    conn: &str,
    endpoint: &str,
    input: &crate::app::TypedSecret,
    hint: &str,
) -> (String, String) {
    let (conn_txt, conn_hostile) = display_name(conn.as_bytes());
    let (ep_txt, ep_hostile) = display_name(endpoint.as_bytes());
    let lines = [
        ta(
            "modal-ask-secret-conn",
            &[
                ("badge", if conn_hostile { HOSTILE_BADGE } else { "" }),
                ("conn", &clamp_chars(&conn_txt, 48)),
            ],
        ),
        ta(
            "modal-ask-secret-endpoint",
            &[
                ("badge", if ep_hostile { HOSTILE_BADGE } else { "" }),
                ("endpoint", &clamp_chars(&ep_txt, 52)),
            ],
        ),
        ta(
            "modal-ask-secret-field",
            &[("dots", &"•".repeat(input.chars().min(SECRET_DOTS_MAX)))],
        ),
        t("modal-ask-secret-note"),
        hint.to_owned(),
    ];
    (t("modal-ask-secret-title"), lines.join("\n"))
}

#[cfg(test)]
mod ai_rename_plan_modal_tests {
    use super::{HOSTILE_BADGE, ai_rename_plan_modal_text, display_name, modal_height};
    use norte_proto::methods::{
        AiRenameEntry, FsRenameBatchPlanResult, PlanHash, RenameCollision, RenameCollisionKind,
        RenameStep,
    };
    use norte_proto::{Segment, VPath};

    fn dir() -> VPath {
        VPath::parse("mem:///proyecto").expect("wire válido")
    }

    fn entry(from: &str, to: &str) -> AiRenameEntry {
        AiRenameEntry {
            from: from.into(),
            to: to.into(),
        }
    }

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("segmento")
    }

    fn hash() -> PlanHash {
        PlanHash::parse(&"0".repeat(64)).expect("64 hex")
    }

    /// El caso NORMAL: el core contestó que el lote se puede ejecutar.
    fn plan_ok() -> norte_frontend::BatchPlan {
        listo(FsRenameBatchPlanResult {
            steps: vec![RenameStep {
                from: seg(b"a"),
                to: seg(b"b"),
                temp: false,
            }],
            collisions: vec![],
            executable: true,
            plan_hash: hash(),
        })
    }

    /// Envuelve un plan del core en el estado «ya contestó».
    fn listo(p: FsRenameBatchPlanResult) -> norte_frontend::BatchPlan {
        norte_frontend::BatchPlan::Ready(Box::new(p))
    }

    /// Un lote PARADO por un veredicto (`steps` vacío: el invariante del
    /// proto — un plan no ejecutable jamás viene ordenado a medias).
    fn plan_con_colision(kind: RenameCollisionKind, name: &[u8]) -> norte_frontend::BatchPlan {
        listo(FsRenameBatchPlanResult {
            steps: vec![],
            collisions: vec![RenameCollision {
                pair_index: 0,
                name: seg(name),
                kind,
            }],
            executable: false,
            plan_hash: hash(),
        })
    }

    /// H3c: con una ayuda abierta ENCIMA, las teclas del modal no responden,
    /// así que su pie no puede seguir ofreciéndolas.
    ///
    /// Este modal y el de hits semánticos son los dos únicos cuya pista es
    /// PROSA de Fluent en vez de un hint generado, y por eso necesitan esta
    /// rama: los generados ya los sustituye `DialogHints::with_modals_inert`.
    /// NO son los dos únicos que una ayuda puede tapar — eso lo decide
    /// `help_context::help_over_modal_allowed`, e incluye la aprobación de
    /// agente y el TOFU de host key. Sin esta rama, un lector con la ayuda
    /// delante veía «y/Enter: aplicar» y ninguna de las dos hacía nada: un pie
    /// que miente, que es exactamente lo que el diseño de `hints.rs` existe
    /// para no tener.
    #[test]
    fn el_pie_del_plan_no_ofrece_teclas_inertes_bajo_la_ayuda() {
        use norte_i18n::t;
        let alive = crate::hints::DialogHints::default();
        let (_, normal) =
            ai_rename_plan_modal_text(&dir(), &[entry("a", "b")], 0, &alive, &plan_ok());
        assert!(
            normal.contains(&t("modal-ai-rename-plan-hint")),
            "sin ayuda encima, el pie ofrece sus teclas: {normal}"
        );

        let inert = alive.with_modals_inert();
        let (_, tapado) =
            ai_rename_plan_modal_text(&dir(), &[entry("a", "b")], 0, &inert, &plan_ok());
        assert!(
            !tapado.contains(&t("modal-ai-rename-plan-hint")),
            "con la ayuda encima NO puede ofrecer y/n: {tapado}"
        );
        assert!(
            tapado.contains(&t("modal-hint-help-open")),
            "y tiene que decir por qué: {tapado}"
        );
    }

    /// Audit MINOR-6a (corpus canónico, molde del sweep de `app.rs`): cada
    /// nombre hostil, en la posición `from` Y en la `to` — ningún char de
    /// `is_terminal_hazard` sobrevive en el texto pintado, y cuando el
    /// enmascarado altera el nombre la línea va MARCADA con el badge.
    #[test]
    fn barrido_corpus_ningun_hazard_sobrevive_y_el_enmascarado_marca() {
        for n in norte_testkit::corpus::hostile_names() {
            let name = String::from_utf8_lossy(&n.bytes).into_owned();
            let cases = [
                (name.clone(), "limpio.txt".to_owned()),
                ("limpio.txt".to_owned(), name.clone()),
            ];
            for (from, to) in cases {
                let hostile = display_name(from.as_bytes()).1 || display_name(to.as_bytes()).1;
                let (_, body) = ai_rename_plan_modal_text(
                    &dir(),
                    &[entry(&from, &to)],
                    0,
                    &crate::hints::DialogHints::default(),
                    &plan_ok(),
                );
                // Por LÍNEA: el `\n` que separa las líneas del cuerpo es un
                // control legítimo del formato, no contenido pintado.
                assert!(
                    !body
                        .lines()
                        .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                    "corpus {}: un hazard sobrevivió al render: {body:?}",
                    n.id
                );
                if hostile {
                    assert!(
                        body.contains(HOSTILE_BADGE),
                        "corpus {}: enmascarado SIN badge: {body:?}",
                        n.id
                    );
                }
            }
        }
    }

    /// Audit MINOR-4 (corpus `arrow_join_spoof`): un `from` que IMITA la
    /// flecha no fabrica una pareja falsa — el `from` lleva su etiqueta
    /// numerada fuera de banda en SU línea y el destino REAL conserva la
    /// suya con la flecha al inicio.
    #[test]
    fn arrow_join_spoof_no_fabrica_pareja() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "arrow_join_spoof")
            .expect("fixture del corpus");
        let from = String::from_utf8_lossy(&spoof.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry(&from, "real.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado + from + to + hint = 5 líneas exactas: el spoof no
        // añade una.
        assert_eq!(lines.len(), 5, "{body:?}");
        assert!(lines[2].contains("1."), "etiqueta fuera de banda: {body:?}");
        assert!(
            lines[3].starts_with('→') && lines[3].contains("real.txt"),
            "el destino real conserva SU línea: {body:?}"
        );
    }

    /// Audit MINOR-6c: un destino hostil (RLO del corpus) se enmascara y su
    /// línea va marcada — el badge antecede incluso a la flecha.
    #[test]
    fn destino_hostil_enmascara_y_marca() {
        let rtl = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let to = String::from_utf8_lossy(&rtl.bytes).into_owned();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("limpio.txt", &to)],
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let to_line = body.lines().nth(3).expect("línea del destino");
        assert!(to_line.starts_with(HOSTILE_BADGE), "{body:?}");
        assert!(to_line.contains('\u{FFFD}'), "{body:?}");
        assert!(
            !to_line.chars().any(norte_encoding::is_terminal_hazard),
            "{body:?}"
        );
    }

    /// Audit MAJOR-3: con 7 parejas la ventana pinta 5 desde `offset` con
    /// numeración ABSOLUTA, el indicador dice posición/total y el alto del
    /// modal cuadra con las líneas pintadas.
    #[test]
    fn plan_largo_ventana_indicador_y_alto() {
        let entries: Vec<AiRenameEntry> = (1..=7)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado del lote + 5 parejas × 2 + indicador + hint = 14.
        assert_eq!(lines.len(), 14, "{body:?}");
        assert!(
            lines[2].contains("1.") && lines[2].contains("f1"),
            "{body:?}"
        );
        assert!(lines[12].contains("5/7"), "indicador: {body:?}");
        assert!(!body.contains("f6"), "la cola espera al scroll: {body:?}");
        // offset 2 = parejas 3..=7, numeración absoluta, indicador al tope.
        let (_, body2) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            2,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 14, "alto ESTABLE al scroll: {body2:?}");
        assert!(
            lines2[2].contains("3.") && lines2[2].contains("f3"),
            "{body2:?}"
        );
        assert!(body2.contains("f7"), "{body2:?}");
        assert!(lines2[12].contains("7/7"), "{body2:?}");
        // Un offset desbocado se clampa en el render (cinturón).
        let (_, body3) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            999,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        assert!(body3.contains("f7"), "{body3:?}");
        // Alto: 14 líneas de cuerpo + 3 de marco.
        let modal = crate::app::Modal::AiRenamePlan {
            dir: dir(),
            entries,
            offset: 0,
            seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
            plan: plan_ok(),
        };
        assert_eq!(modal_height(&modal), 17);
    }

    /// Audit MAJOR-3: el indicador de desbordamiento delata una pareja
    /// hostil OCULTA (lo no visible jamás se cuela "limpio"), y deja de
    /// marcar cuando el scroll la pone a la vista.
    #[test]
    fn indicador_marca_hostil_oculto() {
        let mut entries: Vec<AiRenameEntry> = (1..=6)
            .map(|i| entry(&format!("f{i}"), &format!("t{i}")))
            .collect();
        entries[5] = entry("x\u{202e}y", "limpio.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            0,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let ind = body.lines().nth(12).expect("indicador");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: la hostil entra en la ventana; la oculta (pareja 1) es
        // limpia — el indicador ya no marca.
        let (_, body2) = ai_rename_plan_modal_text(
            &dir(),
            &entries,
            1,
            &crate::hints::DialogHints::default(),
            &plan_ok(),
        );
        let ind2 = body2.lines().nth(12).expect("indicador");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }

    /// §17: una colisión es VISIBLE con su veredicto y su índice de pareja,
    /// y el modal dice que el plan NO se puede aplicar — un humano no puede
    /// confirmar un lote que va a rebotar sin saber por qué.
    #[test]
    fn una_colision_se_pinta_y_el_plan_se_marca_inaplicable() {
        use norte_i18n::t;
        let plan = plan_con_colision(RenameCollisionKind::External, b"z.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a.txt", "z.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado + pareja × 2 + causa + nombre + hint = 7. La causa y
        // el nombre van SEPARADOS desde #273.
        assert_eq!(lines.len(), 7, "{body:?}");
        assert_eq!(lines[1], t("modal-rename-batch-not-applicable"), "{body:?}");
        assert!(
            lines[4].contains(&t("modal-rename-batch-collision-external")),
            "el veredicto se enseña: {body:?}"
        );
        assert!(lines[5].contains("z.txt"), "y el nombre ofensor: {body:?}");
        assert!(
            !lines[5].contains(&t("modal-rename-batch-collision-external")),
            "pero el nombre NO lleva la causa dentro: {body:?}"
        );
        // `pair_index` 0 se pinta 1-based, como la etiqueta del `from`: la
        // fila culpable es señalable.
        assert!(lines[4].contains("1."), "{body:?}");
        // El pie NO ofrece una tecla muda.
        assert_eq!(
            lines[6],
            t("modal-rename-batch-plan-hint-blocked"),
            "{body:?}"
        );
        assert!(!body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
    }

    /// Un veredicto de un daemon MÁS NUEVO degrada UNA línea a una etiqueta
    /// genérica, jamás el modal entero: el resto del plan se sigue leyendo y
    /// el lote sigue marcado como no aplicable.
    #[test]
    fn un_veredicto_desconocido_degrada_una_linea_no_el_modal() {
        use norte_i18n::t;
        let future: RenameCollisionKind =
            serde_json::from_str(r#""clase_del_futuro""#).expect("fallback");
        assert_eq!(future, RenameCollisionKind::Unknown);
        let plan = plan_con_colision(future, b"z.txt");
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a.txt", "z.txt")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 7, "el modal sigue entero: {body:?}");
        assert!(lines[2].contains("a.txt"), "las parejas se siguen viendo");
        assert!(
            lines[4].contains(&t("modal-rename-batch-collision-unknown")),
            "{body:?}"
        );
        assert_eq!(lines[1], t("modal-rename-batch-not-applicable"), "{body:?}");
    }

    /// Un paso temporal es MAQUINARIA del planificador: se dice CUÁNTOS hay,
    /// jamás cómo se llaman. Un `.norte-rename-…` entre las parejas haría
    /// creer al humano que norte va a dejar ese nombre en su disco.
    #[test]
    fn un_paso_temporal_se_cuenta_jamas_se_nombra() {
        use norte_i18n::t;
        let plan = listo(FsRenameBatchPlanResult {
            steps: vec![
                RenameStep {
                    from: seg(b"a"),
                    to: seg(b".norte-rename-0a1b2c3d-0"),
                    temp: true,
                },
                RenameStep {
                    from: seg(b"b"),
                    to: seg(b"a"),
                    temp: false,
                },
                RenameStep {
                    from: seg(b".norte-rename-0a1b2c3d-0"),
                    to: seg(b"b"),
                    temp: true,
                },
            ],
            collisions: vec![],
            executable: true,
            plan_hash: hash(),
        });
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b"), entry("b", "a")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        assert!(
            !body.contains(".norte-rename-"),
            "un temporal jamás se pinta como propuesta: {body:?}"
        );
        // Las DOS mitades del ciclo llevan `temp`, y las dos son maquinaria.
        assert!(
            body.contains(&norte_i18n::ta("modal-rename-batch-temp", &[("n", "2")])),
            "{body:?}"
        );
        // Aplicable: el rodeo no es una colisión.
        assert!(
            body.contains(&t("modal-rename-batch-applicable")),
            "{body:?}"
        );
        assert!(body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
    }

    /// El plan en vuelo (`None`): el modal abre con las parejas y dice que
    /// está comprobando — sin ofrecer una tecla de confirmar que está muda.
    #[test]
    fn sin_plan_todavia_el_pie_no_ofrece_confirmar() {
        use norte_i18n::t;
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b")],
            0,
            &crate::hints::DialogHints::default(),
            &norte_frontend::BatchPlan::Pending,
        );
        assert!(body.contains(&t("modal-rename-batch-pending")), "{body:?}");
        assert!(!body.contains(&t("modal-ai-rename-plan-hint")), "{body:?}");
        // Y el alto cuadra con lo pintado (dir + pareja × 2 + estado + hint).
        let modal = crate::app::Modal::AiRenamePlan {
            dir: dir(),
            entries: vec![entry("a", "b")],
            offset: 0,
            seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
            plan: norte_frontend::BatchPlan::Pending,
        };
        assert_eq!(modal_height(&modal), 8);
        assert_eq!(body.lines().count(), 5, "{body:?}");
    }

    /// Barrido del corpus sobre el NOMBRE OFENSOR de una colisión: ningún
    /// hazard sobrevive, el enmascarado MARCA la línea, y el veredicto —que
    /// es lo accionable— jamás se lo come el nombre.
    #[test]
    fn barrido_corpus_en_el_nombre_de_la_colision() {
        use norte_i18n::t;
        let verdict = t("modal-rename-batch-collision-internal");
        for n in norte_testkit::corpus::hostile_names() {
            let plan = plan_con_colision(RenameCollisionKind::Internal, &n.bytes);
            let (_, body) = ai_rename_plan_modal_text(
                &dir(),
                &[entry("a", "b")],
                0,
                &crate::hints::DialogHints::default(),
                &plan,
            );
            assert!(
                !body
                    .lines()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: un hazard sobrevivió al render: {body:?}",
                n.id
            );
            // DOS líneas por colisión desde #273: la causa y el nombre van
            // separados, porque un nombre que contenga la causa entera
            // fabricaba una entrada de la lista que no existe. Siguen siendo
            // exactamente dos: un nombre no puede fabricar una tercera.
            assert_eq!(body.lines().count(), 7, "corpus {}: {body:?}", n.id);
            let causa = body.lines().nth(4).expect("línea de la causa");
            let nombre = body.lines().nth(5).expect("línea del nombre");
            assert!(
                causa.contains(&verdict),
                "corpus {}: el veredicto va en SU línea: {body:?}",
                n.id
            );
            assert!(
                !nombre.contains(&verdict),
                "corpus {}: el nombre no lleva la causa dentro: {body:?}",
                n.id
            );
            if display_name(&n.bytes).1 {
                assert!(
                    nombre.trim_start().starts_with(HOSTILE_BADGE),
                    "corpus {}: enmascarado SIN badge: {body:?}",
                    n.id
                );
            }
        }
    }

    /// Un lote con MUCHAS colisiones no desborda el modal: se pintan hasta
    /// [`norte_frontend::RENAME_COLLISION_LIMIT`] y el resumen dice cuántas
    /// quedan fuera — y marca si alguna OCULTA es hostil (lo escondido no se
    /// cuela limpio).
    #[test]
    fn muchas_colisiones_se_resumen_y_lo_oculto_hostil_se_marca() {
        let mut collisions: Vec<RenameCollision> = (0..8)
            .map(|i| RenameCollision {
                pair_index: i,
                name: seg(format!("f{i}").as_bytes()),
                kind: RenameCollisionKind::Internal,
            })
            .collect();
        collisions[7].name = seg("x\u{202e}y".as_bytes());
        let total = collisions.len();
        let plan = listo(FsRenameBatchPlanResult {
            steps: vec![],
            collisions,
            executable: false,
            plan_hash: hash(),
        });
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "b")],
            0,
            &crate::hints::DialogHints::default(),
            &plan,
        );
        let lines: Vec<&str> = body.lines().collect();
        // dir + estado + pareja × 2 + 5 colisiones × 2 (causa y nombre van
        // separados desde #273) + resumen + hint = 16.
        assert_eq!(lines.len(), 16, "{body:?}");
        let summary = lines[14];
        assert!(
            summary.contains(&norte_frontend::RENAME_COLLISION_LIMIT.to_string())
                && summary.contains(&total.to_string()),
            "el resumen no calla cuántas quedan fuera: {body:?}"
        );
        assert!(
            summary.starts_with(HOSTILE_BADGE),
            "una colisión OCULTA hostil marca el resumen: {body:?}"
        );
        assert!(!body.contains("f7"), "la cola queda resumida: {body:?}");
    }
}

#[cfg(test)]
mod semantic_hits_modal_tests {
    use super::{HOSTILE_BADGE, modal_height, semantic_hits_modal_text};
    use norte_proto::methods::SemanticHit;
    use norte_proto::{Segment, VPath};

    fn hit(path: VPath, score: f64) -> SemanticHit {
        SemanticHit { path, score }
    }

    fn hits(n: u16) -> Vec<SemanticHit> {
        (1..=n)
            .map(|i| {
                hit(
                    VPath::parse(&format!("mem:///d/f{i}")).expect("wire válido"),
                    1.0 - f64::from(i) / 100.0,
                )
            })
            .collect()
    }

    /// M4-IA-2 (corpus canónico, molde del sweep del plan IA): cada nombre
    /// hostil como último segmento del path de un hit — ningún char de
    /// `is_terminal_hazard` sobrevive en el texto pintado, y cuando el
    /// enmascarado altera el path la línea va MARCADA con el badge.
    #[test]
    fn barrido_corpus_ningun_hazard_sobrevive_y_el_enmascarado_marca() {
        for n in norte_testkit::corpus::hostile_names() {
            let path = VPath::parse("mem:///d")
                .expect("wire válido")
                .join(Segment::new(n.bytes.clone()).expect("segmento del corpus"));
            let hostile = norte_frontend::path_display(&path).1;
            let (_, body) = semantic_hits_modal_text(
                &[hit(path, 0.5)],
                0,
                0,
                &crate::hints::DialogHints::default(),
            );
            // Por LÍNEA: el `\n` que separa las líneas del cuerpo es un
            // control legítimo del formato, no contenido pintado.
            assert!(
                !body
                    .lines()
                    .any(|l| l.chars().any(norte_encoding::is_terminal_hazard)),
                "corpus {}: un hazard sobrevivió al render: {body:?}",
                n.id
            );
            if hostile {
                assert!(
                    body.contains(HOSTILE_BADGE),
                    "corpus {}: enmascarado SIN badge: {body:?}",
                    n.id
                );
            }
        }
    }

    /// Encoding audit M4-IA-2 S1 (fixture `score_spoof_inband`): un nombre
    /// que IMITA la columna de score (`informe · 0.99.txt`: middle dot +
    /// decimales, todo imprimible — NO hay badge que avise) jamás desplaza
    /// al score REAL. Se pinea en dos formas: el fixture tal cual (cabe
    /// entero, el score genuino queda el ÚLTIMO campo) y el fixture inflado
    /// a >120 chars (fuerza la elipsis media: el path se RECORTA, marcado,
    /// pero el score sigue ahí — jamás al revés).
    #[test]
    fn score_spoof_inband_jamas_desplaza_al_score_real() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "score_spoof_inband")
            .expect("fixture del corpus");
        let dir = VPath::parse("mem:///d").expect("wire válido");
        let señuelo = String::from_utf8(fixture.bytes.clone()).expect("el fixture es UTF-8");

        let path = dir
            .clone()
            .join(Segment::new(fixture.bytes.clone()).expect("segmento del corpus"));
        let (_, body) = semantic_hits_modal_text(
            &[hit(path, 0.91)],
            0,
            0,
            &crate::hints::DialogHints::default(),
        );
        let line = body.lines().next().expect("la línea del hit");
        assert!(
            line.contains(&señuelo),
            "el señuelo se pinta tal cual (es un nombre legítimo): {line:?}"
        );
        assert!(
            line.trim_end().ends_with("0.91"),
            "el score REAL es el campo FINAL: {line:?}"
        );

        // Inflado: el señuelo al final de un nombre kilométrico. El recorte
        // se come el PATH (elipsis media, marcada), nunca el score.
        let mut long = b"x".repeat(120);
        long.extend_from_slice(&fixture.bytes);
        let path = dir.join(Segment::new(long).expect("segmento válido"));
        let (_, body) = semantic_hits_modal_text(
            &[hit(path, 0.91)],
            0,
            0,
            &crate::hints::DialogHints::default(),
        );
        let line = body.lines().next().expect("la línea del hit");
        assert!(
            line.trim_end().ends_with("0.91"),
            "path kilométrico: el score REAL sigue siendo el campo FINAL: {line:?}"
        );
        assert!(
            line.contains('…'),
            "el recorte del path se MARCA (spec §6): {line:?}"
        );
    }

    /// M4-IA-2: con 12 hits la ventana pinta 10 desde `offset` con
    /// numeración ABSOLUTA y marcador `>` en la fila del cursor; el
    /// indicador dice posición/total, el score va al final de la línea y el
    /// alto del modal cuadra con las líneas pintadas.
    #[test]
    fn hits_largos_ventana_cursor_indicador_y_alto() {
        let hits = hits(12);
        let (_, body) =
            semantic_hits_modal_text(&hits, 0, 3, &crate::hints::DialogHints::default());
        let lines: Vec<&str> = body.lines().collect();
        // 10 hits + indicador + hint = 12.
        assert_eq!(lines.len(), 12, "{body:?}");
        assert!(
            lines[0].contains("1.") && lines[0].contains("f1"),
            "{body:?}"
        );
        assert!(
            lines[3].starts_with("> ") && lines[3].contains("4."),
            "marcador en la fila del cursor: {body:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("> ")).count(),
            1,
            "un solo cursor: {body:?}"
        );
        assert!(lines[0].contains("0.99"), "score al final: {body:?}");
        assert!(lines[10].contains("10/12"), "indicador: {body:?}");
        assert!(!body.contains("f11"), "la cola espera al scroll: {body:?}");
        // La ventana sigue al cursor: offset 2 = hits 3..=12, numeración
        // absoluta, cursor al fondo visible.
        let (_, body2) =
            semantic_hits_modal_text(&hits, 2, 11, &crate::hints::DialogHints::default());
        let lines2: Vec<&str> = body2.lines().collect();
        assert_eq!(lines2.len(), 12, "alto ESTABLE al scroll: {body2:?}");
        assert!(
            lines2[0].contains("3.") && lines2[0].contains("f3"),
            "{body2:?}"
        );
        assert!(
            lines2[9].starts_with("> ") && lines2[9].contains("12."),
            "{body2:?}"
        );
        assert!(lines2[10].contains("12/12"), "{body2:?}");
        // Un offset desbocado se clampa en el render (cinturón).
        let (_, body3) =
            semantic_hits_modal_text(&hits, 999, 0, &crate::hints::DialogHints::default());
        assert!(body3.contains("f12"), "{body3:?}");
        // Alto: 12 líneas de cuerpo + 3 de marco.
        let modal = crate::app::Modal::SemanticHits {
            hits,
            offset: 0,
            cursor: 0,
        };
        assert_eq!(modal_height(&modal), 15);
    }

    /// M4-IA-2: el indicador de desbordamiento delata un hit hostil OCULTO
    /// (lo no visible jamás se cuela "limpio"), y deja de marcar cuando el
    /// scroll lo pone a la vista.
    #[test]
    fn indicador_marca_hostil_oculto() {
        let mut hits = hits(11);
        hits[10] = hit(
            VPath::parse("mem:///d")
                .expect("wire válido")
                .join(Segment::new(b"x\xe2\x80\xaey".to_vec()).expect("segmento")),
            0.1,
        );
        let (_, body) =
            semantic_hits_modal_text(&hits, 0, 0, &crate::hints::DialogHints::default());
        let ind = body.lines().nth(10).expect("indicador");
        assert!(ind.starts_with(HOSTILE_BADGE), "{body:?}");
        // offset 1: el hostil entra en la ventana; el oculto (hit 1) es
        // limpio — el indicador ya no marca.
        let (_, body2) =
            semantic_hits_modal_text(&hits, 1, 10, &crate::hints::DialogHints::default());
        let ind2 = body2.lines().nth(10).expect("indicador");
        assert!(!ind2.starts_with(HOSTILE_BADGE), "{body2:?}");
    }
}

/// El modal de aprobación de agente: la lista de rutas y su ALTO (review H3c
/// MINOR-5).
#[cfg(test)]
mod approval_modal_tests {
    use super::{HOSTILE_BADGE, approval_modal_text, modal_height};

    fn req(paths: Vec<String>) -> norte_proto::methods::PolicyApprovalRequired {
        norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            op: "copy".into(),
            paths_total: paths.len() as u64,
            paths,
            ttl_ms: 60_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        }
    }

    fn rutas(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("mem:///proj/f{i}.txt")).collect()
    }

    /// El recorte del SERVER también se cuenta (0.36.0). Un lote de renames
    /// gatea miles de rutas y el daemon difunde solo las primeras: si el modal
    /// pintara `paths.len()` como si fuera todo, el humano aprobaría 32 rutas
    /// inocentes sin saber que la decisión cubría ocho mil. Eso no es una
    /// aprobación informada, es una aprobación engañada.
    #[test]
    fn el_recorte_del_server_se_le_dice_al_humano() {
        let mut r = req(rutas(3));
        r.paths_total = 8192;
        let (_, body) = approval_modal_text(&r, "PIE");
        let lines: Vec<&str> = body.lines().collect();
        // cabecera + plazo + 3 rutas + resumen + pie.
        assert_eq!(lines.len(), 7, "{body:?}");
        assert!(
            lines[5].contains(&(8192 - 3).to_string()),
            "el resumen cuenta las que la DECISIÓN cubre y no se ven: {body:?}"
        );
        assert_eq!(lines[6], "PIE", "y el pie sigue siendo la última: {body:?}");
        assert_eq!(
            modal_height(&crate::app::Modal::ApproveAgentOp { req: r }),
            9,
            "el alto cuenta la línea de resumen que acaba de aparecer",
        );
    }

    /// `paths_total: 0` es un server N-1 que no lo mandaba: lo recibido ES
    /// todo lo que hubo, y no se inventa un resumen que mentiría al revés.
    #[test]
    fn sin_paths_total_no_se_inventa_recorte() {
        let mut r = req(rutas(2));
        r.paths_total = 0;
        let (_, body) = approval_modal_text(&r, "PIE");
        assert_eq!(
            body.lines().count(),
            5,
            "cabecera + plazo + 2 rutas + pie: {body:?}"
        );
    }

    /// Review MINOR-5: el número de rutas lo elige el AGENTE, y el alto no
    /// podía crecer con él sin tope.
    ///
    /// `centered` recorta contra el frame, así que las líneas de sobra
    /// simplemente no se pintaban — incluida la ÚLTIMA, que bajo H3c es la
    /// única explicación de por qué `y`/`n` no hacen nada. Un `paths` de 400
    /// entradas borraba el aviso de la pantalla. Ahora se enventana como
    /// `ConfirmDelete`: `MODAL_ITEM_LIMIT` rutas más una línea de resumen.
    #[test]
    fn la_lista_de_rutas_se_enventana_y_el_pie_siempre_cabe() {
        let limit = norte_frontend::MODAL_ITEM_LIMIT;
        let total = limit + 7;
        let (_, body) = approval_modal_text(&req(rutas(total)), "PIE-DEL-MODAL");
        let lines: Vec<&str> = body.lines().collect();

        // cabecera + plazo + LIMITE rutas + resumen + pie.
        assert_eq!(lines.len(), limit + 4, "{body:?}");
        assert!(lines[2].contains("f1.txt"), "{body:?}");
        assert!(
            lines[limit + 1].contains(&format!("f{limit}.txt")),
            "la última ruta de la ventana: {body:?}"
        );
        assert!(
            !body.contains(&format!("f{}.txt", limit + 1)),
            "la cola NO se pinta: {body:?}"
        );
        assert!(
            lines[limit + 2].contains(&(total - limit).to_string()),
            "el resumen dice cuántas quedan fuera: {body:?}"
        );
        assert_eq!(
            lines[limit + 3],
            "PIE-DEL-MODAL",
            "y el pie es la ÚLTIMA línea, siempre presente: {body:?}"
        );

        // El alto lo dice el mismo cómputo acotado: cuerpo + marco, jamás
        // `paths.len()` crudo.
        let modal = crate::app::Modal::ApproveAgentOp {
            req: req(rutas(total)),
        };
        let height = modal_height(&modal);
        assert_eq!(
            height,
            u16::try_from(limit + 4).expect("cabe") + 2,
            "{height}"
        );
        assert_eq!(
            modal_height(&crate::app::Modal::ApproveAgentOp { req: req(rutas(1)) }),
            6,
            "un lote que cabe conserva su alto de siempre: acotar la lista no \
             le mueve la caja"
        );
        assert_eq!(
            height,
            modal_height(&crate::app::Modal::ApproveAgentOp {
                req: req(rutas(400)),
            }),
            "el agente no elige el alto: 17 rutas y 400 miden lo mismo"
        );
    }

    /// Un lote que CABE se pinta entero y sin línea de resumen: enventanar no
    /// puede inventarse un «y N más» que no existe.
    #[test]
    fn un_lote_que_cabe_no_lleva_resumen() {
        let (_, body) = approval_modal_text(&req(rutas(2)), "PIE");
        let lines: Vec<&str> = body.lines().collect();
        // Cabecera + PLAZO + 2 rutas + pie. El plazo va siempre desde que este
        // modal dice cuánto le queda, como el de la ventana.
        assert_eq!(lines.len(), 5, "{body:?}");
        assert!(body.contains("f2.txt"), "{body:?}");
        assert_eq!(lines[4], "PIE", "{body:?}");
    }

    /// Y lo ESCONDIDO no se cuela limpio (misma doctrina que el plan IA y los
    /// hits semánticos): si alguna ruta fuera de la ventana es hostil, la línea
    /// de resumen va MARCADA — el humano decide sabiendo que hay algo raro que
    /// no está viendo.
    #[test]
    fn el_resumen_marca_una_ruta_hostil_escondida() {
        let limit = norte_frontend::MODAL_ITEM_LIMIT;
        let mut paths = rutas(limit + 2);
        paths[limit + 1] = "mem:///proj/x\u{202e}y.txt".to_owned();
        let (_, body) = approval_modal_text(&req(paths), "PIE");
        let summary = body.lines().nth(limit + 2).expect("resumen");
        assert!(
            summary.starts_with(HOSTILE_BADGE),
            "el resumen delata la hostil oculta: {body:?}"
        );

        // Con TODAS las ocultas limpias, no marca (o el badge no diría nada).
        let (_, clean) = approval_modal_text(&req(rutas(limit + 2)), "PIE");
        let clean_summary = clean.lines().nth(limit + 2).expect("resumen");
        assert!(!clean_summary.starts_with(HOSTILE_BADGE), "{clean:?}");
    }
}
