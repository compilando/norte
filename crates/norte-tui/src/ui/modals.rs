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

/// CELDAS de terminal (`UnicodeWidthStr::width`, mismo idioma que
/// [`draw_nav_popup`]/[`middle_ellipsis`]), no en `chars` — un cuerpo con
/// CJK (dos celdas por char, p. ej. un path con `日本語`) desbordaba la caja
/// con el conteo de chars antiguo.
pub(crate) fn modal_width(titulo: &str, cuerpo: &str, frame_width: u16) -> u16 {
    let content_max = cuerpo
        .lines()
        .map(UnicodeWidthStr::width)
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
#[allow(clippy::too_many_lines)] // tabla modal→texto, no lógica
pub(crate) fn modal_title_body(
    modal: &crate::app::Modal,
    reinterpret: Option<norte_encoding::NameEncoding>,
    hints: &crate::hints::DialogHints,
) -> (String, String) {
    use crate::app::{Modal, TransferKind};
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
        Modal::ConfirmTransfer {
            kind,
            items,
            to,
            space,
            confine,
        } => (
            match kind {
                TransferKind::Copy => t("modal-copy-title"),
                TransferKind::Move => t("modal-move-title"),
            },
            [
                norte_frontend::item_lines_with(items, HOSTILE_BADGE, reinterpret),
                vec![
                    // El destino es un DIRECTORIO y va en SU línea, con la
                    // flecha FUERA de banda: ningún nombre de la lista de
                    // arriba puede imitar esta línea.
                    format!("→ {}", norte_frontend::path_display_with(to, reinterpret).0),
                ],
                // #149: el aviso de espacio va DEBAJO del destino y encima de
                // las teclas — es lo último que se lee antes de decidir. Solo
                // cuando lo hay: cabe, o el destino no sabe decirlo, o no se
                // sabe cuánto se mueve, y ninguna de las tres se anuncia.
                space.clone().into_iter().collect::<Vec<String>>(),
                // #164: y justo debajo, si el destino no sabe confinar lo que
                // se escriba en él. Las dos líneas son de la misma clase —un
                // hecho del destino que conviene saber antes de decir que sí—
                // y ninguna bloquea nada.
                confine.clone().into_iter().collect::<Vec<String>>(),
                vec![hints.confirm.clone()],
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
        // #105: nombre de destino editable — dir destino + campo + error,
        // todo de usuario y todo enmascarado.
        Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            error,
            enc,
            ..
        } => transfer_name_modal_text(*kind, from, to_dir, name, error.as_deref(), *enc),
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
            let lines = 1 + shown + usize::from(total > shown) + 1;
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
        // + bordes. TransferName con error (#105): origen + dir destino +
        // campo + hint + teclas + error (6 líneas), +3.
        Modal::TrustHostKey { .. } | Modal::TransferName { error: Some(_), .. } => 9,
        // TrustLuaInit: un mensaje largo con wrap (~4 líneas a 58 cols) +
        // bordes. TransferName sin error: 5 líneas de cuerpo (origen y dir
        // destino incluidos), +3.
        Modal::TrustLuaInit { .. } | Modal::TransferName { .. } => 8,
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
    let mut body = Paragraph::new(body).block(
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
        let hidden_hostile = req
            .paths
            .iter()
            .skip(shown)
            .any(|p| display_name(p.as_bytes()).1);
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
pub(crate) fn transfer_name_modal_text(
    kind: crate::app::TransferKind,
    from: &norte_proto::VPath,
    to_dir: &norte_proto::VPath,
    name: &str,
    error: Option<&str>,
    enc: Option<norte_encoding::NameEncoding>,
) -> (String, String) {
    let (masked, hostile) = display_name(name.as_bytes());
    let field = if hostile {
        format!("{HOSTILE_BADGE} {masked}_")
    } else {
        format!("{masked}_")
    };
    // #105 review MAJOR-2/MINOR-1: origen y dir destino, cada uno en SU
    // línea con la flecha fuera de banda, bajo la reinterpretación
    // CAPTURADA al abrir (#98/M1 — jamás la del pane al pintar). La ruta
    // va con ELIPSIS MEDIA, como en el modal de aprobación y el de
    // colisión: `modal_width` topa contra el ancho del frame y el
    // `Paragraph` de `draw_modal` no envuelve, así que una ruta honda se
    // cortaba a pelo contra el borde y expulsaba de la caja la COLA del
    // destino — justo lo que el usuario necesita ver para saber dónde
    // aterriza la copia — sin ni un `…` que lo delatara.
    let badge_line = |p: &norte_proto::VPath| {
        let (line, hostile) = norte_frontend::path_display_with(p, enc);
        let line = middle_ellipsis(&line, MODAL_PATH_CHARS);
        if hostile {
            format!("{HOSTILE_BADGE} {line}")
        } else {
            line
        }
    };
    let mut lines = vec![
        badge_line(from),
        format!("→ {}", badge_line(to_dir)),
        field,
        t("modal-transfer-name-hint"),
        t("modal-mark-pattern-keys"),
    ];
    if let Some(err) = error {
        let (masked_err, _) = display_name(err.as_bytes());
        lines.push(masked_err);
    }
    let title = match kind {
        crate::app::TransferKind::Copy => t("modal-transfer-name-copy"),
        crate::app::TransferKind::Move => t("modal-transfer-name-move"),
    };
    (title, lines.join("\n"))
}

#[cfg(test)]
mod transfer_name_modal_text_tests {
    use super::transfer_name_modal_text;
    use crate::app::TransferKind;
    use norte_proto::VPath;

    /// #105 review MINOR-2 (misma clase que el M4 del patrón): fn PURA — un
    /// RLO crudo en nombre y error sale enmascarado, y un byte hostil en el
    /// ORIGEN y el dir destino jamás llega crudo (`path_display` los enmascara
    /// y llevan badge).
    #[test]
    fn masks_every_user_surface() {
        let hostile = "abc\u{202E}rid";
        let from = VPath::parse("mem:///src/a%FF.txt").unwrap();
        let to_dir = VPath::parse("mem:///dst%FE").unwrap();
        let (_, body) = transfer_name_modal_text(
            TransferKind::Move,
            &from,
            &to_dir,
            hostile,
            Some(hostile),
            None,
        );
        assert!(!body.contains('\u{202E}'), "{body:?}");
        assert!(
            body.matches('\u{FFFD}').count() >= 4,
            "nombre + error (RLO) y origen + destino (bytes): {body:?}"
        );
        assert!(body.matches(super::HOSTILE_BADGE).count() >= 2, "{body:?}");
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

    /// `tail_window` cuenta CHARS, no bytes: cortar por bytes partiría un
    /// carácter multibyte por la mitad.
    #[test]
    fn la_ventana_de_cola_cuenta_chars() {
        assert_eq!(tail_window("abc", 10), "abc");
        assert_eq!(tail_window("abcdef", 3), "…ef");
        let cjk = "日本語のファイル";
        let w = tail_window(cjk, 4);
        assert_eq!(w.chars().count(), 4);
        assert!(w.starts_with('…'));
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
        // cabecera + 3 rutas + resumen + pie.
        assert_eq!(lines.len(), 6, "{body:?}");
        assert!(
            lines[4].contains(&(8192 - 3).to_string()),
            "el resumen cuenta las que la DECISIÓN cubre y no se ven: {body:?}"
        );
        assert_eq!(lines[5], "PIE", "y el pie sigue siendo la última: {body:?}");
        assert_eq!(
            modal_height(&crate::app::Modal::ApproveAgentOp { req: r }),
            8,
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
            4,
            "cabecera + 2 rutas + pie: {body:?}"
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

        // cabecera + LIMITE rutas + resumen + pie.
        assert_eq!(lines.len(), limit + 3, "{body:?}");
        assert!(lines[1].contains("f1.txt"), "{body:?}");
        assert!(
            lines[limit].contains(&format!("f{limit}.txt")),
            "la última ruta de la ventana: {body:?}"
        );
        assert!(
            !body.contains(&format!("f{}.txt", limit + 1)),
            "la cola NO se pinta: {body:?}"
        );
        assert!(
            lines[limit + 1].contains(&(total - limit).to_string()),
            "el resumen dice cuántas quedan fuera: {body:?}"
        );
        assert_eq!(
            lines[limit + 2],
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
            u16::try_from(limit + 3).expect("cabe") + 2,
            "{height}"
        );
        assert_eq!(
            modal_height(&crate::app::Modal::ApproveAgentOp { req: req(rutas(1)) }),
            5,
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
        assert_eq!(lines.len(), 4, "{body:?}"); // cabecera + 2 rutas + pie
        assert!(body.contains("f2.txt"), "{body:?}");
        assert_eq!(lines[3], "PIE", "{body:?}");
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
        let summary = body.lines().nth(limit + 1).expect("resumen");
        assert!(
            summary.starts_with(HOSTILE_BADGE),
            "el resumen delata la hostil oculta: {body:?}"
        );

        // Con TODAS las ocultas limpias, no marca (o el badge no diría nada).
        let (_, clean) = approval_modal_text(&req(rutas(limit + 2)), "PIE");
        let clean_summary = clean.lines().nth(limit + 1).expect("resumen");
        assert!(!clean_summary.starts_with(HOSTILE_BADGE), "{clean:?}");
    }
}
