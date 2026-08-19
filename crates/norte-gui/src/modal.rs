//! Modales de confirmación y resolución de conflicto (máquina de estado PURA,
//! testeable sin GPUI). El handler de `main.rs` enruta la tecla aquí con
//! [`on_key`] y actúa sobre el [`ModalOutcome`]; el render pinta el [`Modal`].
//!
//! Aquí también viven los tipos de OPERACIÓN mutante ([`TransferKind`],
//! [`PendingOp`]) que `session.rs` ejecuta: un modal confirmado produce
//! `PendingOp`s que la GUI manda por el canal de comandos.

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, ConflictKind, DeleteMode, VPath};

use crate::keys::typed_char;

/// Ventana de parejas del plan IA — la constante y el cinturón
/// [`norte_frontend::validate_ai_plan`] viven en `norte-frontend`
/// (compartidos con la TUI, quality review 78eb243 MAJOR-1); re-export para
/// el render (`modal_lines` en `main.rs`) y el clamp del scroll de
/// [`on_key`].
pub use norte_frontend::AI_RENAME_PAIR_LIMIT;

/// Ventana de hits del modal semántico (M4-IA-2) — la constante y el
/// cinturón [`norte_frontend::validate_semantic_hits`] viven en
/// `norte-frontend` (compartidos con la TUI); re-export para el render
/// (`modal_lines` en `main.rs`) y el clamp del cursor de [`on_key`].
pub use norte_frontend::SEMANTIC_HIT_LIMIT;

/// Tope de caracteres de la instrucción de [`Modal::AiRenamePrompt`] (molde
/// TUI `MARK_PATTERN_MAX_CHARS`): un paste accidental no desborda el modal;
/// el límite REAL (4 KiB) lo pone el daemon.
pub const AI_INSTRUCTION_MAX_CHARS: usize = 256;

/// Tope de BYTES del nombre de [`Modal::RenamePrompt`]. Anti-paste, no una
/// regla de nombres: el límite real por componente lo pone el backend (255
/// bytes en la mayoría de filesystems y en MinIO, otra cosa en otros) y
/// quien lo hace cumplir es él, no este modal. Por eso el tope es holgado y
/// sólo frena lo que se AÑADE: un nombre ya existente más largo que esto se
/// siembra entero y se puede seguir editando (borrar siempre vale).
pub const RENAME_NAME_MAX_BYTES: usize = 512;

/// Copia o movimiento (la clase de una transferencia).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Copia (origen intacto).
    Copy,
    /// Movimiento (origen desaparece al terminar).
    Move,
}

/// Una operación mutante concreta lista para mandar al daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingOp {
    /// Copia/movimiento de `from` a `to` (ambos ABSOLUTOS; `to` ya incluye el
    /// nombre destino) con la política de colisión de `opts`.
    Transfer {
        /// Copia o movimiento.
        kind: TransferKind,
        /// Origen (path absoluto de la entrada).
        from: VPath,
        /// Destino absoluto (dir destino + nombre del origen).
        to: VPath,
        /// Opciones (colisión, symlinks, resume).
        opts: TransferOptions,
    },
    /// Borrado de `path` (papelera o permanente).
    Delete {
        /// Entrada a borrar.
        path: VPath,
        /// Papelera (default) o permanente.
        mode: DeleteMode,
    },
    /// Lote de renames dentro de UN directorio ejecutado como UNA task y UNA
    /// unidad deshacible del journal (`fs.rename_batch`, spec §17, ADR 0042).
    ///
    /// No es «N transfers en una op»: el core decide el ORDEN y mete los
    /// temporales que hagan falta, así que una permutación (`a→b, b→a`) —el
    /// caso normal del rename IA— se puede hacer, y un fallo a mitad se
    /// deshace entero.
    RenameBatch {
        /// El directorio en el que viven TODAS las parejas.
        dir: VPath,
        /// Las parejas from→to, en nombres BASE (no rutas).
        pairs: Vec<norte_proto::methods::RenamePair>,
        /// El hash del plan que el humano aprobó: ata esta ejecución a lo que
        /// se le enseñó. Si el directorio derivó, el core contesta
        /// `PlanStale` y no toca nada.
        plan_hash: norte_proto::methods::PlanHash,
    },
}

/// Modal activo. Uno a la vez; el render lo pinta como overlay.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Confirmar copia/movimiento de `items` al dir `to`.
    ConfirmTransfer {
        /// Copia o movimiento.
        kind: TransferKind,
        /// Orígenes (paths absolutos; marcas o el target del cursor).
        items: Vec<VPath>,
        /// DIRECTORIO destino (el `dir` del pane inactivo).
        to: VPath,
    },
    /// Confirmar borrado de `items`; `permanent` toggleable.
    ConfirmDelete {
        /// Entradas a borrar.
        items: Vec<VPath>,
        /// `false` = papelera (default), `true` = permanente.
        permanent: bool,
    },
    /// Un transfer chocó con el destino: reintentar/saltar/cancelar.
    ConflictResolve {
        /// La transferencia que falló (para reemitir).
        pending: PendingTransfer,
        /// Subtipo de conflicto (para pintar el motivo).
        conflict: ConflictKind,
    },
    /// Confirmar `app.quit` habiendo trabajo pendiente (revisión C2/G0
    /// IMPORTANT 3): tasks visibles en la franja y/o marcas activas se
    /// perderían de la vista (las tasks siguen en el daemon; las marcas son
    /// solo de sesión) si se cierra sin avisar. Contadores puramente
    /// informativos (para el título del modal, ver `modal_lines` en
    /// `main.rs`) — `on_key` no los necesita.
    ConfirmQuit {
        /// Tasks visibles en la franja (`task_progress.len()`).
        tasks: usize,
        /// Suma de marcas activas en ambos panes.
        marks: usize,
    },
    /// Prompt de instrucción del rename IA (M4-IA). Query en BYTES (molde
    /// `PaletteView`): push/backspace UTF-8-boundary-aware; se pinta
    /// enmascarada (`modal_lines`), jamás cruda.
    AiRenamePrompt {
        /// Dir del pane activo al abrir (viaja al daemon y luego al plan).
        dir: VPath,
        /// La instrucción tecleada hasta ahora, en bytes UTF-8.
        query: Vec<u8>,
    },
    /// Plan de rename IA revisable (M4-IA): superficie de DECISIÓN —
    /// `y`/`enter` aplica (tras el cinturón
    /// [`norte_frontend::rename_pairs`]), `n`/Esc
    /// descarta, `up`/`down` desplazan la ventana de
    /// [`AI_RENAME_PAIR_LIMIT`] parejas.
    AiRenamePlan {
        /// Dir sobre el que se aplican los renames.
        dir: VPath,
        /// Parejas from→to del modelo (proto; UTF-8 garantizado por el
        /// engine, pero se re-valida y se pinta a la defensiva igual).
        entries: Vec<norte_proto::methods::AiRenameEntry>,
        /// Primera pareja visible de la ventana de scroll (paridad TUI
        /// audit MAJOR-3: sin esto, la cola de un plan largo se aplicaría
        /// sin poder verse).
        offset: usize,
        /// El plan del LOTE que contestó `fs.rename_batch_plan` (spec §17,
        /// ADR 0042): veredictos, si es aplicable y el `plan_hash` que hay
        /// que devolver para ejecutar EXACTAMENTE lo que se enseñó.
        ///
        /// Sin un plan APLICABLE `y`/`enter` queda MUDO ([`on_key`] contesta
        /// `Ignored`) y el pie deja de ofrecerlo: no hay hash aprobado que
        /// mandar.
        plan: norte_frontend::BatchPlan,
    },
    /// Renombrar in situ (`pane.rename`, shift+F6 — paridad de SEMÁNTICA con
    /// el `TransferName` de la TUI en su modo rename, no de widget).
    ///
    /// Tres cosas que lo definen, y las tres importan:
    ///
    /// - El destino es el PADRE de `from`, no el dir del pane: renombrar
    ///   nunca mueve de sitio.
    /// - Actúa sobre UNA entrada (el cursor). Las marcas no renombran en
    ///   bloque — eso sería un batch-rename, otra feature.
    /// - El nombre editable son BYTES, sembrados con los del nombre real. La
    ///   TUI siembra TEXTO (decodificado o lossy) y por eso necesita un flag
    ///   `touched` y un guard anti-`U+FFFD`: sin ellos, renombrar un nombre
    ///   no-UTF8 escribiría el residuo lossy en disco. Aquí no hay residuo
    ///   que heredar porque no hay decodificación: los bytes que no toca el
    ///   usuario salen byte-exactos, incluso si edita el resto del nombre
    ///   (un `.bak` al final de un nombre CP437 conserva el prefijo crudo).
    ///   El precio es que un `U+FFFD` TECLEADO a propósito sí se acepta —
    ///   asimetría deliberada con la TUI, que no puede distinguirlo del
    ///   residuo y por eso lo rechaza.
    RenamePrompt {
        /// Entrada a renombrar (absoluta): también es la fuente del nombre
        /// ORIGINAL que se pinta y contra el que se compara el destino.
        from: VPath,
        /// Padre de `from` — dónde aterriza el nombre nuevo.
        to_dir: VPath,
        /// El nombre en edición, en BYTES (sembrado con los de `from`).
        name: Vec<u8>,
        /// Diagnóstico del último intento inválido, bajo el campo. El texto
        /// tecleado sobrevive para corregirlo (paridad TUI).
        error: Option<String>,
    },
    /// Prompt de consulta de la búsqueda semántica (M4-IA-2). Query en
    /// BYTES (molde [`Modal::AiRenamePrompt`]): push/backspace
    /// UTF-8-boundary-aware; se pinta enmascarada (`modal_lines`), jamás
    /// cruda. Sin `dir`: la búsqueda es global (root = None, paridad TUI).
    SemanticQuery {
        /// La consulta tecleada hasta ahora, en bytes UTF-8.
        query: Vec<u8>,
    },
    /// Hits de la búsqueda semántica (M4-IA-2): superficie de NAVEGACIÓN —
    /// `y`/`enter` abre la ubicación del hit bajo el cursor
    /// ([`ModalOutcome::NavigateTo`]), `n`/Esc cierra, `up`/`down` mueven
    /// el cursor con ventana de [`SEMANTIC_HIT_LIMIT`] que lo sigue
    /// (molde TUI `semantic_cursor`).
    SemanticHits {
        /// Hits path+score (ya pasaron el cinturón
        /// [`norte_frontend::validate_semantic_hits`] en la ingestión; se
        /// pintan a la defensiva igual).
        hits: Vec<norte_proto::methods::SemanticHit>,
        /// Primer hit visible de la ventana de scroll.
        offset: usize,
        /// Hit bajo el cursor (índice ABSOLUTO en `hits`).
        cursor: usize,
    },
    /// El picker de volúmenes (2026-08-10-volumes.md task V4, design §D):
    /// `pane.select-drive`/`-left`/`-right`. Molde `SemanticHits` — una
    /// snapshot CONGELADA de `Backend::volumes` que `main.rs` pide async y
    /// entrega ya construida (este módulo no conoce `Backend`).
    Volumes {
        /// El pane que `Enter` navega — el foco para `pane.select-drive`, un
        /// LADO fijo para `-left`/`-right` independientemente de dónde esté
        /// el foco AHORA (design §D, paridad TUI `NavPopup::target_pane`):
        /// congelado al pedir la lista, no releído al confirmar.
        pane: usize,
        /// El modo con el que se pidió ESTA lista (el toggle "mostrar todo"
        /// del design §E la vuelve a pedir invertido — literalmente una
        /// reapertura, no un caso especial, paridad TUI
        /// `open_volumes_popup`).
        include_pseudo: bool,
        /// Los volúmenes, en el orden que el daemon los mandó.
        volumes: Vec<norte_proto::methods::Volume>,
        /// Primer volumen visible de la ventana de scroll.
        offset: usize,
        /// Volumen bajo el cursor (índice ABSOLUTO en `volumes`).
        cursor: usize,
    },
}

/// Ventana de volúmenes visibles a la vez en [`Modal::Volumes`] — el mismo
/// número que [`SEMANTIC_HIT_LIMIT`] (un modal de tamaño fijo, no una lista
/// de terminal que crece con la ventana), como constante PROPIA en vez de
/// reutilizar esa: el recuento de volúmenes de un host no tiene ninguna
/// relación con el de hits semánticos, y acoplarlas haría que cambiar una
/// cambiara la otra sin que nadie lo pidiera.
pub const VOLUMES_LIMIT: usize = 10;

/// La transferencia que originó un conflicto (para reemitir con otra política).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTransfer {
    /// Copia o movimiento.
    pub kind: TransferKind,
    /// Origen.
    pub from: VPath,
    /// Destino absoluto.
    pub to: VPath,
}

/// Qué debe hacer el handler tras una tecla en el modal.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalOutcome {
    /// Tecla irrelevante: no pasa nada (el modal sigue abierto).
    Ignored,
    /// El estado del modal cambió (p. ej. toggle permanent): re-render, sigue abierto.
    StayOpen,
    /// Cierra el modal sin hacer nada.
    Dismiss,
    /// Cierra el modal y manda estas operaciones al daemon.
    Submit(Vec<PendingOp>),
    /// Confirma `app.quit` (revisión C2/G0 IMPORTANT 3): el caller (`main.rs`)
    /// debe cerrar la ventana (`cx.quit()`) — este módulo es puro y no puede
    /// hacerlo por sí mismo.
    Quit,
    /// Enter en [`Modal::AiRenamePrompt`] con instrucción no vacía: el caller
    /// cierra el modal, manda `SessionCmd::AiRenamePlan` y avisa en el banner
    /// (`msg-ai-rename-running`).
    RequestAiPlan {
        /// Dir del prompt (viaja tal cual a la sesión).
        dir: VPath,
        /// Instrucción ya recortada (trim), no vacía.
        instruction: String,
    },
    /// El plan falló el cinturón [`norte_frontend::validate_ai_plan`]
    /// (paridad TUI audit
    /// MAJOR-2): un daemon hostil/roto mandó un segmento inválido — el caller
    /// cierra el modal SIN someter NADA y avisa (`msg-ai-rename-invalid-plan`).
    InvalidPlan,
    /// Enter en [`Modal::SemanticQuery`] con consulta no vacía (M4-IA-2): el
    /// caller cierra el modal, manda `SessionCmd::SemanticSearch` y avisa en
    /// el banner (`gui-msg-semantic-running`).
    RequestSemantic {
        /// Consulta ya recortada (trim), no vacía.
        query: String,
    },
    /// `y`/Enter sobre un hit de [`Modal::SemanticHits`] (M4-IA-2): el
    /// caller cierra el modal y navega el pane activo a la ubicación del
    /// hit (cd al padre + cursor sobre la entrada).
    NavigateTo(VPath),
    /// `y`/Enter sobre un volumen de [`Modal::Volumes`] (2026-08-10-
    /// volumes.md task V4): el caller cierra el modal y hace `cd` de `pane`
    /// —el LADO congelado al abrir, NO necesariamente el foco actual, design
    /// §D— a `target` (la raíz del volumen). Un `NavigateTo` normal no
    /// alcanza aquí porque `-left`/`-right` deben navegar un lado fijo
    /// incluso si el foco se movió mientras el picker estaba abierto.
    NavigateToPane {
        /// El pane objetivo — ver el campo `pane` de [`Modal::Volumes`].
        pane: usize,
        /// El mount point del volumen elegido.
        target: VPath,
    },
    /// El toggle "mostrar todo" de [`Modal::Volumes`] (`tab`, design §E): el
    /// caller cierra el modal y manda `SessionCmd::Volumes` con
    /// `include_pseudo` invertido — la MISMA petición que abrir el picker la
    /// primera vez, así que el caller la trata igual (banner "cargando…",
    /// reapertura vía la cola de modales pendientes cuando la respuesta
    /// llega).
    RequestVolumes {
        /// El pane a re-pedir para — ver el campo `pane` de
        /// [`Modal::Volumes`].
        pane: usize,
        /// El modo INVERTIDO a pedir.
        include_pseudo: bool,
    },
}

/// Destino absoluto de un item copiado/movido a `to_dir`: `to_dir` + nombre del
/// origen. `None` si el origen no tiene nombre (raíz — no debería marcarse).
#[must_use]
pub fn dest_for(to_dir: &VPath, item: &VPath) -> Option<VPath> {
    item.file_name().map(|n| to_dir.join(n.clone()))
}

/// Enruta una tecla (nombre GPUI) al modal, mutándolo si hace falta. `main.rs`
/// llama a esto cuando hay un modal abierto, ANTES del mapeo de navegación.
/// `key_char` solo lo consume [`Modal::AiRenamePrompt`] (tecleo libre, molde
/// `PaletteView`); el caller ya lo anula bajo ctrl/alt/platform.
/// Lo que un PEGADO aporta a un campo de una línea (#200).
///
/// (El doc de contrato de [`on_key`] está en `on_key`: estas dos funciones se
/// metieron delante de él y se lo quedaron por el camino, que es lo que pasa
/// cuando se inserta código encima de un comentario.)
///
/// Filtra lo mismo que el tecleo y por lo mismo: un portapapeles trae lo que
/// otro programa puso ahí, y en un gestor de ficheros eso incluye rutas
/// copiadas de una web con un `U+202E` dentro o un escape de terminal. El
/// predicado es [`norte_encoding::is_terminal_hazard`], el MISMO que usa cada
/// otra superficie de este repositorio — dos listas de peligros son dos
/// opiniones que divergen a la primera.
///
/// **Un salto de línea CORTA, no confirma.** Es la pregunta que #147 dejó
/// abierta: pegar un texto de dos líneas en un campo de nombre no puede
/// significar «acepta el nombre y sigue con el resto», porque el resto no es
/// un nombre y nadie lo ha leído. Se queda la primera línea, que es lo que el
/// usuario ve pegado.
///
/// Sus casos están en `pegado_de_un_campo` (abajo) y no en un doctest: este
/// crate es un BINARIO, no tiene `lib.rs`, y rustdoc no recoge doctests de un
/// target `[[bin]]` — un ejemplo aquí sería decorativo.
#[must_use]
pub fn pegado_para_campo(texto: &str, tope_chars: usize) -> String {
    texto
        .split(['\n', '\r'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !norte_encoding::is_terminal_hazard(*c))
        .take(tope_chars)
        .collect()
}

/// Recorta a `tope` BYTES sin partir un carácter.
///
/// El corte cae en la frontera anterior: medio carácter en un nombre no es
/// medio nombre, es un nombre distinto.
fn recorta_a_bytes(s: &str, tope: usize) -> String {
    if s.len() <= tope {
        return s.to_owned();
    }
    let mut corte = tope;
    while corte > 0 && !s.is_char_boundary(corte) {
        corte -= 1;
    }
    s[..corte].to_owned()
}

/// Pega en el modal ABIERTO, si es uno de los que se teclean (#200).
///
/// `true` si el modal se quedó con algo. Los modales de DECISIÓN —confirmar,
/// resolver una colisión, aprobar— devuelven `false`: pegar texto en un
/// diálogo de sí/no no significa nada, y hacer que signifique algo es la
/// clase de atajo que aprueba cosas sin querer.
pub fn paste(modal: &mut Modal, texto: &str) -> bool {
    match modal {
        Modal::RenamePrompt { name, error, .. } => {
            // El tope de este campo es de BYTES y el nombre es un `Vec<u8>`,
            // así que el hueco se gasta en bytes: pasarle el número a un
            // `take` de CARACTERES dejaba entrar 512 emojis, o sea 2 KiB, en
            // un campo cuyo tope existe justamente para que un pegado
            // accidental no lo desborde. El test no lo veía porque pegaba
            // «x», el único carácter para el que las dos cuentas coinciden.
            let hueco = RENAME_NAME_MAX_BYTES.saturating_sub(name.len());
            let trozo = recorta_a_bytes(&pegado_para_campo(texto, hueco), hueco);
            if trozo.is_empty() {
                return false;
            }
            name.extend_from_slice(trozo.as_bytes());
            *error = None;
            true
        }
        Modal::AiRenamePrompt { query, .. } | Modal::SemanticQuery { query } => {
            let hueco = AI_INSTRUCTION_MAX_CHARS
                .saturating_sub(String::from_utf8_lossy(query).chars().count());
            let trozo = pegado_para_campo(texto, hueco);
            if trozo.is_empty() {
                return false;
            }
            query.extend_from_slice(trozo.as_bytes());
            true
        }
        _ => false,
    }
}

pub fn on_key(modal: &mut Modal, key: &str, key_char: Option<&str>) -> ModalOutcome {
    match modal {
        Modal::ConfirmTransfer { kind, items, to } => match key {
            "y" => {
                let ops = items
                    .iter()
                    .filter_map(|from| {
                        dest_for(to, from).map(|dest| PendingOp::Transfer {
                            kind: *kind,
                            from: from.clone(),
                            to: dest,
                            opts: TransferOptions::default(),
                        })
                    })
                    .collect();
                ModalOutcome::Submit(ops)
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::ConfirmDelete { items, permanent } => match key {
            "y" => {
                let mode = if *permanent {
                    DeleteMode::Permanent
                } else {
                    DeleteMode::Trash
                };
                let ops = items
                    .iter()
                    .map(|path| PendingOp::Delete {
                        path: path.clone(),
                        mode,
                    })
                    .collect();
                ModalOutcome::Submit(ops)
            }
            "p" | "tab" => {
                *permanent = !*permanent;
                ModalOutcome::StayOpen
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::ConflictResolve { pending, .. } => {
            let policy = match key {
                "o" => CollisionPolicy::Overwrite,
                "s" => CollisionPolicy::Skip,
                "c" | "escape" => return ModalOutcome::Dismiss,
                _ => return ModalOutcome::Ignored,
            };
            ModalOutcome::Submit(vec![PendingOp::Transfer {
                kind: pending.kind,
                from: pending.from.clone(),
                to: pending.to.clone(),
                opts: TransferOptions {
                    on_collision: policy,
                    ..Default::default()
                },
            }])
        }
        // Los contadores son solo para el título (main.rs); "y" no los
        // necesita — confirma sin importar cuántos sean.
        Modal::ConfirmQuit { .. } => match key {
            "y" => ModalOutcome::Quit,
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::RenamePrompt {
            from,
            to_dir,
            name,
            error,
        } => match key {
            "enter" => {
                // El nombre sale tal cual (BYTES) — ni decodificado ni
                // recodificado. `Segment` es quien dice qué es un nombre
                // válido (ni vacío, ni `/`, ni NUL, ni `.`/`..`), la misma
                // puerta que usa el resto del programa.
                match norte_proto::Segment::new(name.clone()) {
                    Ok(seg) => {
                        let dest = to_dir.join(seg);
                        if dest == *from {
                            // Renombrar a lo mismo no es una op: someterlo
                            // gastaría una task y un apunte de journal para
                            // nada.
                            *error = Some(norte_i18n::t("msg-transfer-name-same"));
                            return ModalOutcome::StayOpen;
                        }
                        ModalOutcome::Submit(vec![PendingOp::Transfer {
                            kind: TransferKind::Move,
                            from: from.clone(),
                            to: dest,
                            opts: TransferOptions::default(),
                        }])
                    }
                    Err(e) => {
                        // Taxonomía cerrada de `VPathError`: sin bytes del
                        // usuario dentro (por eso se interpola cruda, mismo
                        // criterio que los banners del protocolo).
                        *error = Some(e.to_string());
                        ModalOutcome::StayOpen
                    }
                }
            }
            "escape" => ModalOutcome::Dismiss,
            "backspace" => {
                // Retira el último carácter UTF-8 COMPLETO cuando lo hay; un
                // byte suelto no-UTF8 (el `0xFF` de un nombre CP437) se
                // retira solo, que es lo único honesto: no forma parte de
                // ningún carácter.
                if name.is_empty() {
                    return ModalOutcome::Ignored;
                }
                let mut cut = name.len() - 1;
                while cut > 0 && (name[cut] & 0b1100_0000) == 0b1000_0000 {
                    cut -= 1;
                }
                name.truncate(cut);
                *error = None;
                ModalOutcome::StayOpen
            }
            _ => {
                if let Some(c) = typed_char(key, key_char) {
                    if c.is_control() || name.len() + c.len_utf8() > RENAME_NAME_MAX_BYTES {
                        return ModalOutcome::Ignored;
                    }
                    let mut buf = [0u8; 4];
                    name.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    *error = None;
                    ModalOutcome::StayOpen
                } else {
                    ModalOutcome::Ignored
                }
            }
        },
        Modal::AiRenamePrompt { dir, query } => match key {
            "enter" => {
                // La query es UTF-8 por construcción (`push_char`); lossy es
                // cinturón por si alguien construyera el modal con bytes
                // arbitrarios (el test hostil lo hace a propósito).
                let instruction = String::from_utf8_lossy(query).trim().to_owned();
                if instruction.is_empty() {
                    return ModalOutcome::StayOpen;
                }
                ModalOutcome::RequestAiPlan {
                    dir: dir.clone(),
                    instruction,
                }
            }
            "escape" => ModalOutcome::Dismiss,
            "backspace" => {
                // Retira el último carácter UTF-8 COMPLETO (molde
                // `PaletteView::backspace`), jamás un byte suelto.
                if query.is_empty() {
                    return ModalOutcome::Ignored;
                }
                let mut cut = query.len() - 1;
                while cut > 0 && (query[cut] & 0b1100_0000) == 0b1000_0000 {
                    cut -= 1;
                }
                query.truncate(cut);
                ModalOutcome::StayOpen
            }
            _ => {
                if let Some(c) = typed_char(key, key_char) {
                    if c.is_control()
                        || String::from_utf8_lossy(query).chars().count()
                            >= AI_INSTRUCTION_MAX_CHARS
                    {
                        return ModalOutcome::Ignored;
                    }
                    let mut buf = [0u8; 4];
                    query.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    ModalOutcome::StayOpen
                } else {
                    ModalOutcome::Ignored
                }
            }
        },
        Modal::AiRenamePlan {
            dir,
            entries,
            offset,
            plan,
        } => match key {
            // La convención "Enter jamás confirma" aplica a modales de
            // CONSENTIMIENTO/destructivos (aprobación de agente, quit); este
            // es un plan REVISADO por el humano — `y/Enter` aplica, igual
            // que la TUI (`modal-ai-rename-plan-hint`).
            "y" | "enter" => {
                // Cinturón fail-loud (paridad TUI audit MAJOR-2): TODAS las
                // parejas se validan ANTES de someter nada.
                let Some(pairs) = norte_frontend::rename_pairs(entries) else {
                    return ModalOutcome::InvalidPlan;
                };
                // §17: confirmar necesita un plan de lote APLICABLE. Sin plan
                // no hay `plan_hash` aprobado que mandar; con veredictos el
                // core no ejecutaría nada. La tecla queda MUDA y el pie deja
                // de ofrecerla (paridad con el gate de `dialog_action` en la
                // TUI) — la decisión de si un lote se puede ejecutar es del
                // core, aquí solo se lee `executable`.
                if !plan.confirmable() {
                    return ModalOutcome::Ignored;
                }
                let Some(resuelto) = plan.ready() else {
                    return ModalOutcome::Ignored;
                };
                // UNA op para el lote entero: una task, un deshacer, y el
                // orden lo pone el core (regla dura 7).
                ModalOutcome::Submit(vec![PendingOp::RenameBatch {
                    dir: dir.clone(),
                    pairs,
                    plan_hash: resuelto.plan_hash.clone(),
                }])
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            "down" | "up" => {
                // El scroll JAMÁS confirma ni cancela (paridad TUI
                // `ai_plan_scroll`): ventana clampada a [0, len - ventana].
                let max = entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT);
                *offset = if key == "down" {
                    (*offset + 1).min(max)
                } else {
                    offset.saturating_sub(1)
                };
                ModalOutcome::StayOpen
            }
            _ => ModalOutcome::Ignored,
        },
        // M4-IA-2: espejo byte a byte de `AiRenamePrompt` (tecleo libre,
        // backspace UTF-8-boundary-aware, tope de chars) — solo cambia el
        // outcome del Enter (RequestSemantic, sin dir).
        Modal::SemanticQuery { query } => match key {
            "enter" => {
                // UTF-8 por construcción (`push_char`); lossy es cinturón por
                // si alguien construyera el modal con bytes arbitrarios (el
                // test hostil lo hace a propósito).
                let q = String::from_utf8_lossy(query).trim().to_owned();
                if q.is_empty() {
                    return ModalOutcome::StayOpen;
                }
                ModalOutcome::RequestSemantic { query: q }
            }
            "escape" => ModalOutcome::Dismiss,
            "backspace" => {
                // Retira el último carácter UTF-8 COMPLETO (molde
                // `PaletteView::backspace`), jamás un byte suelto.
                if query.is_empty() {
                    return ModalOutcome::Ignored;
                }
                let mut cut = query.len() - 1;
                while cut > 0 && (query[cut] & 0b1100_0000) == 0b1000_0000 {
                    cut -= 1;
                }
                query.truncate(cut);
                ModalOutcome::StayOpen
            }
            _ => {
                if let Some(c) = typed_char(key, key_char) {
                    if c.is_control()
                        || String::from_utf8_lossy(query).chars().count()
                            >= AI_INSTRUCTION_MAX_CHARS
                    {
                        return ModalOutcome::Ignored;
                    }
                    let mut buf = [0u8; 4];
                    query.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    ModalOutcome::StayOpen
                } else {
                    ModalOutcome::Ignored
                }
            }
        },
        Modal::SemanticHits {
            hits,
            offset,
            cursor,
        } => match key {
            // Igual que el plan IA: superficie ya REVISADA por el humano y
            // no destructiva (navegar no muta) — `y/Enter` abre, como la TUI
            // (`modal-semantic-hits-hint`).
            "y" | "enter" => match hits.get(*cursor) {
                Some(h) => ModalOutcome::NavigateTo(h.path.clone()),
                // Defensivo: sin hit bajo el cursor (hits vacíos — la
                // ingestión no abre este modal vacío) no hay nada que abrir.
                None => ModalOutcome::Dismiss,
            },
            "n" | "escape" => ModalOutcome::Dismiss,
            "down" | "up" => {
                // Cursor con ventana que lo sigue (molde TUI
                // `semantic_cursor`); JAMÁS confirma ni cancela.
                if hits.is_empty() {
                    return ModalOutcome::Ignored;
                }
                *cursor = if key == "down" {
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
                ModalOutcome::StayOpen
            }
            _ => ModalOutcome::Ignored,
        },
        Modal::Volumes {
            pane,
            include_pseudo,
            volumes,
            offset,
            cursor,
        } => match key {
            // Molde `SemanticHits`: superficie de navegación, no destructiva
            // — `y`/`enter` abre (aquí: cambia el dir del pane congelado).
            "y" | "enter" => match volumes.get(*cursor) {
                Some(v) => ModalOutcome::NavigateToPane {
                    pane: *pane,
                    target: v.mount.clone(),
                },
                // Defensivo: sin volumen bajo el cursor (lista vacía — la
                // ingestión no abre este modal vacío) no hay nada que abrir.
                None => ModalOutcome::Dismiss,
            },
            "n" | "escape" => ModalOutcome::Dismiss,
            "down" | "up" => {
                // Cursor con ventana que lo sigue (molde TUI/`SemanticHits`);
                // JAMÁS confirma ni cancela.
                if volumes.is_empty() {
                    return ModalOutcome::Ignored;
                }
                *cursor = if key == "down" {
                    (*cursor + 1).min(volumes.len() - 1)
                } else {
                    cursor.saturating_sub(1)
                };
                if *cursor < *offset {
                    *offset = *cursor;
                }
                if *cursor >= *offset + VOLUMES_LIMIT {
                    *offset = *cursor + 1 - VOLUMES_LIMIT;
                }
                ModalOutcome::StayOpen
            }
            // El toggle "mostrar todo" (design §E): literalmente una
            // reapertura con el modo invertido, paridad TUI
            // `open_drive_popup`/`dialog.toggle-enabled` — el caller cierra
            // este modal y vuelve a pedir la lista, no hay estado que
            // voltear aquí (la lista congelada NO cambia sin una respuesta
            // fresca del daemon).
            "tab" => ModalOutcome::RequestVolumes {
                pane: *pane,
                include_pseudo: !*include_pseudo,
            },
            _ => ModalOutcome::Ignored,
        },
    }
}

#[cfg(test)]
mod tests {

    /// #200: pegar en un campo de nombre añade lo pegado, saneado.
    ///
    /// La GUI no tenía pegado de NINGUNA clase: no hay `InputHandler`, no se
    /// leía el portapapeles para texto, y `cmd+v` se filtraba antes de llegar
    /// a un campo. Lo que faltaba no era solo la lectura: era el filtro, que
    /// es lo que hace que un `U+202E` copiado de una web no entre en el nombre
    /// de un fichero.
    #[test]
    fn pegar_en_el_nombre_anade_lo_pegado_saneado() {
        let mut m = Modal::RenamePrompt {
            from: vp("mem:///d/a.txt"),
            to_dir: vp("mem:///d"),
            name: b"a".to_vec(),
            error: Some("lo que fuera".to_owned()),
        };
        assert!(paste(&mut m, "\u{202e}.txt"));
        let Modal::RenamePrompt { name, error, .. } = &m else {
            panic!("sigue siendo el prompt");
        };
        assert_eq!(name, b"a.txt", "sin el peligro, y pegado al final");
        assert!(error.is_none(), "un pegado limpia el diagnóstico viejo");
    }

    /// Un salto de línea CORTA. Pegar dos líneas en un campo de una no puede
    /// significar «acepta la primera y sigue con la segunda»: la segunda no la
    /// ha leído nadie.
    #[test]
    fn un_pegado_multilinea_se_corta_y_no_somete() {
        let mut m = Modal::SemanticQuery { query: Vec::new() };
        assert!(paste(&mut m, "informes de 2024\nrm -rf /"));
        let Modal::SemanticQuery { query } = &m else {
            panic!("sigue abierto");
        };
        assert_eq!(query, b"informes de 2024");
    }

    /// Y en un modal de DECISIÓN no se pega: un sí/no no tiene campo, y
    /// hacer que lo tenga es la clase de atajo que aprueba cosas sin querer.
    #[test]
    fn en_un_modal_de_decision_no_se_pega() {
        let mut m = Modal::ConfirmDelete {
            items: vec![vp("mem:///d/a")],
            permanent: false,
        };
        assert!(!paste(&mut m, "y"));
    }

    /// El tope se respeta: un portapapeles con un mega de texto no desborda un
    /// campo de nombre.
    #[test]
    fn el_pegado_respeta_el_tope_del_campo() {
        let mut m = Modal::RenamePrompt {
            from: vp("mem:///d/a"),
            to_dir: vp("mem:///d"),
            name: Vec::new(),
            error: None,
        };
        // **Con un carácter de CUATRO bytes**, que es lo que destapa la
        // trampa: el tope es de bytes y se estaba gastando en caracteres, así
        // que 512 emojis entraban como 2 KiB en un campo de 512. Con «x» —lo
        // que este test pegaba— las dos cuentas coinciden y el fallo era
        // invisible.
        assert!(paste(&mut m, &"😀".repeat(RENAME_NAME_MAX_BYTES)));
        let Modal::RenamePrompt { name, .. } = &m else {
            panic!("sigue abierto");
        };
        assert!(
            name.len() <= RENAME_NAME_MAX_BYTES,
            "el tope es de BYTES: {}",
            name.len()
        );
        assert!(
            std::str::from_utf8(name).is_ok(),
            "y el corte no parte un carácter"
        );

        let mut m = Modal::RenamePrompt {
            from: vp("mem:///d/a"),
            to_dir: vp("mem:///d"),
            name: Vec::new(),
            error: None,
        };
        assert!(paste(&mut m, &"x".repeat(RENAME_NAME_MAX_BYTES + 100)));
        let Modal::RenamePrompt { name, .. } = &m else {
            panic!("sigue abierto");
        };
        assert_eq!(name.len(), RENAME_NAME_MAX_BYTES);
        // Y lleno, un pegado más no hace nada.
        assert!(!paste(&mut m, "mas"));
    }
    use super::*;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn dest_for_une_dir_destino_con_nombre_origen() {
        assert_eq!(
            dest_for(&vp("mem:///dst"), &vp("mem:///src/foo.txt")),
            Some(vp("mem:///dst/foo.txt"))
        );
    }

    #[test]
    fn dest_for_preserva_bytes_no_utf8_del_nombre_origen() {
        // Origen con bytes 0xFF 0xFE en el nombre (inválido como UTF-8).
        let src = vp("mem:///src/%FF%FE.bin");
        let dest = dest_for(&vp("mem:///dst"), &src).expect("tiene nombre");
        assert_eq!(
            dest.file_name().unwrap().as_bytes(),
            &[0xFF, 0xFE, b'.', b'b', b'i', b'n'],
        );
        assert_eq!(dest.parent().as_ref(), Some(&vp("mem:///dst")));
    }

    #[test]
    fn dest_for_raiz_sin_nombre_es_none_sin_panic() {
        assert_eq!(dest_for(&vp("mem:///dst"), &vp("mem:///")), None);
    }

    #[test]
    fn dest_for_no_pliega_nfd_a_nfc() {
        let src = VPath::parse("mem:///")
            .unwrap()
            .join(norte_proto::Segment::new(vec![0x65, 0xCC, 0x81]).unwrap());
        let dest = dest_for(&vp("mem:///dst"), &src).expect("tiene nombre");
        assert_eq!(dest.file_name().unwrap().as_bytes(), &[0x65, 0xCC, 0x81]);
    }

    #[test]
    fn confirm_transfer_y_produce_una_op_por_item() {
        let mut m = Modal::ConfirmTransfer {
            kind: TransferKind::Copy,
            items: vec![vp("mem:///src/a"), vp("mem:///src/b")],
            to: vp("mem:///dst"),
        };
        let out = on_key(&mut m, "y", None);
        assert_eq!(
            out,
            ModalOutcome::Submit(vec![
                PendingOp::Transfer {
                    kind: TransferKind::Copy,
                    from: vp("mem:///src/a"),
                    to: vp("mem:///dst/a"),
                    opts: TransferOptions::default(),
                },
                PendingOp::Transfer {
                    kind: TransferKind::Copy,
                    from: vp("mem:///src/b"),
                    to: vp("mem:///dst/b"),
                    opts: TransferOptions::default(),
                },
            ])
        );
    }

    #[test]
    fn confirm_transfer_n_o_escape_descartan() {
        let mut m = Modal::ConfirmTransfer {
            kind: TransferKind::Move,
            items: vec![vp("mem:///src/a")],
            to: vp("mem:///dst"),
        };
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
    }

    #[test]
    fn confirm_delete_togglea_permanent_y_confirma_con_el_modo() {
        let mut m = Modal::ConfirmDelete {
            items: vec![vp("mem:///a")],
            permanent: false,
        };
        assert_eq!(on_key(&mut m, "p", None), ModalOutcome::StayOpen);
        assert_eq!(
            on_key(&mut m, "y", None),
            ModalOutcome::Submit(vec![PendingOp::Delete {
                path: vp("mem:///a"),
                mode: DeleteMode::Permanent,
            }])
        );
        // "tab" es alias de "p" para el toggle.
        assert_eq!(on_key(&mut m, "tab", None), ModalOutcome::StayOpen);
        // "n"/"escape" descartan sin importar el estado de `permanent`.
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }

    #[test]
    fn confirm_delete_default_es_papelera() {
        let mut m = Modal::ConfirmDelete {
            items: vec![vp("mem:///a")],
            permanent: false,
        };
        assert_eq!(
            on_key(&mut m, "y", None),
            ModalOutcome::Submit(vec![PendingOp::Delete {
                path: vp("mem:///a"),
                mode: DeleteMode::Trash,
            }])
        );
    }

    #[test]
    fn conflict_overwrite_reemite_con_overwrite() {
        let mut m = Modal::ConflictResolve {
            pending: PendingTransfer {
                kind: TransferKind::Copy,
                from: vp("mem:///src/a"),
                to: vp("mem:///dst/a"),
            },
            conflict: ConflictKind::Exists,
        };
        let out = on_key(&mut m, "o", None);
        match out {
            ModalOutcome::Submit(ops) => {
                assert_eq!(ops.len(), 1);
                match &ops[0] {
                    PendingOp::Transfer { opts, .. } => {
                        assert_eq!(opts.on_collision, CollisionPolicy::Overwrite);
                    }
                    other => panic!("esperaba Transfer, vino {other:?}"),
                }
            }
            other => panic!("esperaba Submit, vino {other:?}"),
        }

        // "s" reemite con Skip (cubre el otro brazo de política).
        let out = on_key(&mut m, "s", None);
        match out {
            ModalOutcome::Submit(ops) => {
                assert_eq!(ops.len(), 1);
                match &ops[0] {
                    PendingOp::Transfer { opts, .. } => {
                        assert_eq!(opts.on_collision, CollisionPolicy::Skip);
                    }
                    other => panic!("esperaba Transfer, vino {other:?}"),
                }
            }
            other => panic!("esperaba Submit, vino {other:?}"),
        }
    }

    #[test]
    fn conflict_cancelar_descarta() {
        let mut m = Modal::ConflictResolve {
            pending: PendingTransfer {
                kind: TransferKind::Copy,
                from: vp("mem:///src/a"),
                to: vp("mem:///dst/a"),
            },
            conflict: ConflictKind::Exists,
        };
        assert_eq!(on_key(&mut m, "c", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }

    /// Revisión C2/G0 IMPORTANT 3: "y" confirma la salida (el caller en
    /// `main.rs` hace `cx.quit()` al ver `ModalOutcome::Quit` — este módulo
    /// no puede tocar GPUI); "n"/Esc cancelan; cualquier otra tecla se
    /// ignora (el modal sigue abierto).
    #[test]
    fn confirm_quit_y_confirma_n_o_escape_cancelan() {
        let mut m = Modal::ConfirmQuit { tasks: 2, marks: 1 };
        assert_eq!(on_key(&mut m, "y", None), ModalOutcome::Quit);
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
        // Pin explícito de la convención del modal de aprobación: Enter
        // JAMÁS confirma una acción destructiva/consentimiento.
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::Ignored);
    }

    /// M4-IA: Enter en el prompt con texto pide el plan (instrucción
    /// recortada), vacío/espacios se queda abierto sin pedir nada, y Esc
    /// descarta.
    #[test]
    fn ai_prompt_enter_pide_plan_y_esc_descarta() {
        let mut m = Modal::AiRenamePrompt {
            dir: vp("mem:///docs"),
            query: Vec::new(),
        };
        // Vacío: StayOpen (nada viaja al daemon).
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::StayOpen);
        // Solo espacios: sigue vacío tras el trim.
        assert_eq!(on_key(&mut m, "space", None), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::StayOpen);
        for c in "kebab".chars() {
            let buf = c.to_string();
            assert_eq!(
                on_key(&mut m, &buf, Some(&buf)),
                ModalOutcome::StayOpen,
                "teclear {c:?} debe quedarse abierto"
            );
        }
        assert_eq!(
            on_key(&mut m, "enter", None),
            ModalOutcome::RequestAiPlan {
                dir: vp("mem:///docs"),
                instruction: "kebab".into(),
            }
        );
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }

    /// M4-IA: backspace retira el último carácter UTF-8 COMPLETO (jamás un
    /// byte suelto — molde `PaletteView::backspace`).
    #[test]
    fn ai_prompt_backspace_respeta_fronteras_utf8() {
        let mut m = Modal::AiRenamePrompt {
            dir: vp("mem:///d"),
            query: Vec::new(),
        };
        assert_eq!(on_key(&mut m, "a", Some("a")), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "ñ", Some("ñ")), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "backspace", None), ModalOutcome::StayOpen);
        let Modal::AiRenamePrompt { query, .. } = &m else {
            unreachable!()
        };
        assert_eq!(query, b"a", "la ñ (2 bytes) se retiró entera");
    }

    /// Plan de lote (`fs.rename_batch_plan`) con el veredicto que se pida, ya
    /// en el estado «el core contestó».
    fn batch_plan(executable: bool) -> norte_frontend::BatchPlan {
        norte_frontend::BatchPlan::Ready(Box::new(norte_proto::methods::FsRenameBatchPlanResult {
            steps: Vec::new(),
            collisions: Vec::new(),
            executable,
            plan_hash: norte_proto::methods::PlanHash::parse(&"0".repeat(64)).expect("64 hex"),
        }))
    }

    /// §17: `y` aplica el plan como UN lote transaccional (`fs.rename_batch`,
    /// con el `plan_hash` aprobado), no como N moves sueltos; `n` descarta.
    #[test]
    fn ai_plan_y_aplica_como_un_lote_y_n_descarta() {
        let entry = norte_proto::methods::AiRenameEntry {
            from: "a.txt".into(),
            to: "b.txt".into(),
        };
        let plan = batch_plan(true);
        let mut m = Modal::AiRenamePlan {
            dir: vp("mem:///docs"),
            entries: vec![entry],
            offset: 0,
            plan: plan.clone(),
        };
        assert_eq!(
            on_key(&mut m, "y", None),
            ModalOutcome::Submit(vec![PendingOp::RenameBatch {
                dir: vp("mem:///docs"),
                pairs: vec![norte_proto::methods::RenamePair {
                    from: norte_proto::Segment::new(b"a.txt".to_vec()).expect("segmento"),
                    to: norte_proto::Segment::new(b"b.txt".to_vec()).expect("segmento"),
                }],
                plan_hash: plan.ready().expect("listo").plan_hash.clone(),
            }])
        );
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
    }

    /// §17 (paridad con el gate de `dialog_action` en la TUI): confirmar está
    /// MUDO sin un plan de lote aplicable — sin plan no hay `plan_hash`
    /// aprobado que mandar, y con veredictos el core no ejecutaría nada.
    /// Descartar sigue vivo en los dos casos.
    ///
    /// (Mutación de control: quitar el gate de `on_key` devuelve un `Submit`
    /// en las dos vueltas y rompe este test.)
    #[test]
    fn ai_plan_no_somete_sin_un_lote_aplicable() {
        for plan in [
            norte_frontend::BatchPlan::Pending,
            norte_frontend::BatchPlan::Failed,
            batch_plan(false),
        ] {
            let mut m = Modal::AiRenamePlan {
                dir: vp("mem:///docs"),
                entries: vec![norte_proto::methods::AiRenameEntry {
                    from: "a.txt".into(),
                    to: "b.txt".into(),
                }],
                offset: 0,
                plan,
            };
            assert_eq!(on_key(&mut m, "y", None), ModalOutcome::Ignored);
            assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::Ignored);
            assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        }
    }

    /// M4-IA cinturón fail-loud (paridad TUI audit MAJOR-2): UNA pareja
    /// inválida (aquí un `to` con `/`) aborta el plan ENTERO — `InvalidPlan`,
    /// cero ops — aunque el resto de parejas fuera legítimo.
    #[test]
    fn ai_plan_invalido_no_somete_nada() {
        let ok = norte_proto::methods::AiRenameEntry {
            from: "a.txt".into(),
            to: "b.txt".into(),
        };
        let evil = norte_proto::methods::AiRenameEntry {
            from: "c.txt".into(),
            to: "../evil".into(),
        };
        for entries in [vec![evil.clone()], vec![ok.clone(), evil.clone()]] {
            let mut m = Modal::AiRenamePlan {
                dir: vp("mem:///docs"),
                entries,
                offset: 0,
                // Con un lote aplicable: lo que aborta es el CINTURÓN de las
                // parejas, que corre ANTES de mirar el plan.
                plan: batch_plan(true),
            };
            assert_eq!(on_key(&mut m, "y", None), ModalOutcome::InvalidPlan);
            assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::InvalidPlan);
        }
    }

    /// M4-IA scroll (paridad TUI audit MAJOR-3): `down`/`up` desplazan la
    /// ventana clampada a `[0, len - AI_RENAME_PAIR_LIMIT]` y JAMÁS
    /// confirman ni cancelan.
    #[test]
    fn ai_plan_scroll_clampa_y_no_decide() {
        let entries: Vec<_> = (0..7)
            .map(|i| norte_proto::methods::AiRenameEntry {
                from: format!("f{i}.txt"),
                to: format!("t{i}.txt"),
            })
            .collect();
        let mut m = Modal::AiRenamePlan {
            dir: vp("mem:///d"),
            entries,
            offset: 0,
            plan: batch_plan(true),
        };
        for expected in [1, 2, 2, 2] {
            assert_eq!(on_key(&mut m, "down", None), ModalOutcome::StayOpen);
            let Modal::AiRenamePlan { offset, .. } = &m else {
                unreachable!()
            };
            assert_eq!(*offset, expected, "clamp en len - ventana = 2");
        }
        for expected in [1, 0, 0] {
            assert_eq!(on_key(&mut m, "up", None), ModalOutcome::StayOpen);
            let Modal::AiRenamePlan { offset, .. } = &m else {
                unreachable!()
            };
            assert_eq!(*offset, expected);
        }
    }

    /// M4-IA-2: Enter en el prompt semántico con texto pide la búsqueda
    /// (consulta recortada), vacío/espacios se queda abierto sin pedir
    /// nada, y Esc descarta (espejo de `ai_prompt_enter_pide_plan_y_esc_
    /// descarta`).
    #[test]
    fn semantic_query_enter_pide_busqueda_y_esc_descarta() {
        let mut m = Modal::SemanticQuery { query: Vec::new() };
        // Vacío: StayOpen (nada viaja al daemon).
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::StayOpen);
        // Solo espacios: sigue vacío tras el trim.
        assert_eq!(on_key(&mut m, "space", None), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::StayOpen);
        for c in "facturas".chars() {
            let buf = c.to_string();
            assert_eq!(
                on_key(&mut m, &buf, Some(&buf)),
                ModalOutcome::StayOpen,
                "teclear {c:?} debe quedarse abierto"
            );
        }
        assert_eq!(
            on_key(&mut m, "enter", None),
            ModalOutcome::RequestSemantic {
                query: "facturas".into(),
            }
        );
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }

    /// M4-IA-2: backspace retira el último carácter UTF-8 COMPLETO (jamás
    /// un byte suelto — mismo molde que el prompt IA).
    #[test]
    fn semantic_query_backspace_respeta_fronteras_utf8() {
        let mut m = Modal::SemanticQuery { query: Vec::new() };
        assert_eq!(on_key(&mut m, "a", Some("a")), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "ñ", Some("ñ")), ModalOutcome::StayOpen);
        assert_eq!(on_key(&mut m, "backspace", None), ModalOutcome::StayOpen);
        let Modal::SemanticQuery { query } = &m else {
            unreachable!()
        };
        assert_eq!(query, b"a", "la ñ (2 bytes) se retiró entera");
    }

    fn semantic_hits(n: usize) -> Vec<norte_proto::methods::SemanticHit> {
        (0..n)
            .map(|i| norte_proto::methods::SemanticHit {
                path: vp(&format!("mem:///docs/f{i}.txt")),
                score: 0.9 - (i as f64) * 0.01,
            })
            .collect()
    }

    /// M4-IA-2: `y`/Enter navegan al hit bajo el cursor (`NavigateTo` con
    /// SU path), `n`/Esc cierran y cualquier otra tecla se ignora.
    #[test]
    fn semantic_hits_enter_navega_al_hit_del_cursor_y_esc_cierra() {
        let mut m = Modal::SemanticHits {
            hits: semantic_hits(3),
            offset: 0,
            cursor: 1,
        };
        assert_eq!(
            on_key(&mut m, "enter", None),
            ModalOutcome::NavigateTo(vp("mem:///docs/f1.txt"))
        );
        assert_eq!(
            on_key(&mut m, "y", None),
            ModalOutcome::NavigateTo(vp("mem:///docs/f1.txt"))
        );
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
    }

    /// M4-IA-2 defensivo: un cursor fuera de rango (o hits vacíos, que la
    /// ingestión no abre) no navega a NADA — Dismiss, jamás un panic.
    #[test]
    fn semantic_hits_cursor_fuera_de_rango_no_navega() {
        let mut m = Modal::SemanticHits {
            hits: Vec::new(),
            offset: 0,
            cursor: 0,
        };
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "down", None), ModalOutcome::Ignored);
        let mut m = Modal::SemanticHits {
            hits: semantic_hits(2),
            offset: 0,
            cursor: 9,
        };
        assert_eq!(on_key(&mut m, "y", None), ModalOutcome::Dismiss);
    }

    /// M4-IA-2 (molde TUI `semantic_cursor`): `down`/`up` mueven el CURSOR
    /// clampado a `[0, len-1]`, la ventana lo sigue por ambos extremos y el
    /// scroll JAMÁS confirma ni cancela.
    #[test]
    fn semantic_hits_cursor_clampa_y_la_ventana_lo_sigue() {
        let n = SEMANTIC_HIT_LIMIT + 2;
        let mut m = Modal::SemanticHits {
            hits: semantic_hits(n),
            offset: 0,
            cursor: 0,
        };
        // Baja hasta el fondo (y una de más: clamp en len-1).
        for _ in 0..n {
            assert_eq!(on_key(&mut m, "down", None), ModalOutcome::StayOpen);
        }
        let Modal::SemanticHits { offset, cursor, .. } = &m else {
            unreachable!()
        };
        assert_eq!(*cursor, n - 1, "clamp en len - 1");
        assert_eq!(
            *offset,
            n - SEMANTIC_HIT_LIMIT,
            "la ventana siguió al cursor por abajo"
        );
        // Sube hasta arriba (y una de más: clamp en 0).
        for _ in 0..n {
            assert_eq!(on_key(&mut m, "up", None), ModalOutcome::StayOpen);
        }
        let Modal::SemanticHits { offset, cursor, .. } = &m else {
            unreachable!()
        };
        assert_eq!(*cursor, 0);
        assert_eq!(*offset, 0, "la ventana siguió al cursor por arriba");
    }

    fn volumes(n: usize) -> Vec<norte_proto::methods::Volume> {
        (0..n)
            .map(|i| norte_proto::methods::Volume {
                mount: vp(&format!("file:///mnt/v{i}")),
                label: None,
                fs_type: "ext4".into(),
                kind: norte_proto::methods::VolumeKind::Fixed,
                total_bytes: Some(1_000),
                free_bytes: Some(500),
                read_only: false,
            })
            .collect()
    }

    /// 2026-08-10-volumes.md task V4, molde `semantic_hits_enter_navega...`:
    /// `y`/Enter navegan al volumen bajo el cursor CON EL PANE congelado en
    /// el modal (no necesariamente el foco actual — design §D), `n`/Esc
    /// cierran, cualquier otra tecla se ignora.
    #[test]
    fn volumes_enter_navega_al_volumen_del_cursor_con_su_pane_y_esc_cierra() {
        let mut m = Modal::Volumes {
            pane: 1,
            include_pseudo: false,
            volumes: volumes(3),
            offset: 0,
            cursor: 1,
        };
        assert_eq!(
            on_key(&mut m, "enter", None),
            ModalOutcome::NavigateToPane {
                pane: 1,
                target: vp("file:///mnt/v1")
            }
        );
        assert_eq!(
            on_key(&mut m, "y", None),
            ModalOutcome::NavigateToPane {
                pane: 1,
                target: vp("file:///mnt/v1")
            }
        );
        assert_eq!(on_key(&mut m, "n", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x", None), ModalOutcome::Ignored);
    }

    /// Defensivo (molde `semantic_hits_cursor_fuera_de_rango_no_navega`): sin
    /// volúmenes, o con un cursor que quedó fuera de rango, `y`/Enter cierra
    /// en vez de indexar fuera de rango.
    #[test]
    fn volumes_cursor_fuera_de_rango_no_navega() {
        let mut m = Modal::Volumes {
            pane: 0,
            include_pseudo: false,
            volumes: Vec::new(),
            offset: 0,
            cursor: 0,
        };
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "down", None), ModalOutcome::Ignored);
        let mut m = Modal::Volumes {
            pane: 0,
            include_pseudo: false,
            volumes: volumes(2),
            offset: 0,
            cursor: 9,
        };
        assert_eq!(on_key(&mut m, "y", None), ModalOutcome::Dismiss);
    }

    /// Molde `semantic_hits_cursor_clampa_y_la_ventana_lo_sigue`: `down`/`up`
    /// mueven el cursor clampado a `[0, len-1]`, la ventana lo sigue por
    /// ambos extremos, el scroll jamás confirma ni cancela.
    #[test]
    fn volumes_cursor_clampa_y_la_ventana_lo_sigue() {
        let n = VOLUMES_LIMIT + 2;
        let mut m = Modal::Volumes {
            pane: 0,
            include_pseudo: false,
            volumes: volumes(n),
            offset: 0,
            cursor: 0,
        };
        for _ in 0..n {
            assert_eq!(on_key(&mut m, "down", None), ModalOutcome::StayOpen);
        }
        let Modal::Volumes { offset, cursor, .. } = &m else {
            unreachable!()
        };
        assert_eq!(*cursor, n - 1, "clamp en len - 1");
        assert_eq!(
            *offset,
            n - VOLUMES_LIMIT,
            "la ventana siguió al cursor por abajo"
        );
        for _ in 0..n {
            assert_eq!(on_key(&mut m, "up", None), ModalOutcome::StayOpen);
        }
        let Modal::Volumes { offset, cursor, .. } = &m else {
            unreachable!()
        };
        assert_eq!(*cursor, 0);
        assert_eq!(*offset, 0, "la ventana siguió al cursor por arriba");
    }

    /// El toggle "mostrar todo" (`tab`, design §E): pide de nuevo con el
    /// modo INVERTIDO y el MISMO pane — no voltea ningún campo local (la
    /// lista congelada no cambia sin una respuesta fresca del daemon, ver
    /// `ModalOutcome::RequestVolumes`'s rustdoc).
    #[test]
    fn volumes_tab_pide_el_modo_invertido_del_mismo_pane() {
        let mut m = Modal::Volumes {
            pane: 1,
            include_pseudo: false,
            volumes: volumes(1),
            offset: 0,
            cursor: 0,
        };
        assert_eq!(
            on_key(&mut m, "tab", None),
            ModalOutcome::RequestVolumes {
                pane: 1,
                include_pseudo: true,
            }
        );
    }

    // --- Renombrado in situ (`pane.rename`) -------------------------------
    //
    // Lo que se clava aquí es lo que distingue este modal de un campo de
    // texto: que el nombre son BYTES de punta a punta. Un nombre que no es
    // UTF-8 (un zip de MS-DOS, un tar de otra máquina) tiene que poder
    // renombrarse sin que la GUI lo reescriba de camino.

    /// Un nombre inválido como UTF-8 se siembra y VUELVE byte-exacto: el
    /// usuario añade una extensión al final y los bytes crudos del principio
    /// llegan intactos al destino. Es la propiedad entera del diseño de
    /// bytes: con un buffer de texto, esos `0xFF 0xFE` habrían salido como
    /// `U+FFFD` y el rename habría creado un fichero con otro nombre.
    #[test]
    fn un_nombre_no_utf8_sobrevive_al_rename_byte_a_byte() {
        let from = vp("mem:///d/%FF%FE.bin");
        let original = from.file_name().unwrap().as_bytes().to_vec();
        assert_eq!(original, b"\xFF\xFE.bin", "sembrado con los bytes reales");
        let mut m = Modal::RenamePrompt {
            from: from.clone(),
            to_dir: vp("mem:///d"),
            name: original.clone(),
            error: None,
        };
        for c in ".bak".chars() {
            assert_eq!(
                on_key(&mut m, "", Some(&c.to_string())),
                ModalOutcome::StayOpen
            );
        }
        let ModalOutcome::Submit(ops) = on_key(&mut m, "enter", None) else {
            panic!("enter con un nombre nuevo válido somete");
        };
        let [
            PendingOp::Transfer {
                kind,
                from: f,
                to,
                opts,
            },
        ] = ops.as_slice()
        else {
            panic!("un rename es UN move: {ops:?}");
        };
        assert_eq!(*kind, TransferKind::Move, "renombrar es mover");
        assert_eq!(*f, from);
        assert_eq!(*opts, TransferOptions::default());
        assert_eq!(
            to.file_name().unwrap().as_bytes(),
            b"\xFF\xFE.bin.bak",
            "el prefijo crudo llega intacto, sin un solo U+FFFD"
        );
        assert_eq!(to.parent().as_ref(), Some(&vp("mem:///d")), "mismo dir");
    }

    /// Confirmar sin tocar nada no somete: renombrar a lo mismo no es una op
    /// (gastaría task y apunte de journal para nada). El modal SIGUE abierto,
    /// con el diagnóstico y el nombre tecleado intactos.
    #[test]
    fn confirmar_el_mismo_nombre_no_somete_y_lo_dice() {
        let from = vp("mem:///d/%FF%FE.bin");
        let mut m = Modal::RenamePrompt {
            from: from.clone(),
            to_dir: vp("mem:///d"),
            name: from.file_name().unwrap().as_bytes().to_vec(),
            error: None,
        };
        assert_eq!(on_key(&mut m, "enter", None), ModalOutcome::StayOpen);
        let Modal::RenamePrompt { name, error, .. } = &m else {
            unreachable!()
        };
        assert!(error.is_some(), "el motivo se dice");
        assert_eq!(
            name.as_slice(),
            b"\xFF\xFE.bin",
            "y lo tecleado sobrevive para corregirlo"
        );
    }

    /// Un nombre que `Segment` rechaza (`..`, vacío, con `/`) no somete nada
    /// y deja el diagnóstico bajo el campo.
    #[test]
    fn un_nombre_invalido_no_somete_y_deja_el_motivo() {
        for malo in [&b".."[..], &b""[..], &b"a/b"[..]] {
            let mut m = Modal::RenamePrompt {
                from: vp("mem:///d/x"),
                to_dir: vp("mem:///d"),
                name: malo.to_vec(),
                error: None,
            };
            assert_eq!(
                on_key(&mut m, "enter", None),
                ModalOutcome::StayOpen,
                "{malo:?} no debe someterse"
            );
            let Modal::RenamePrompt { error, .. } = &m else {
                unreachable!()
            };
            assert!(error.is_some(), "sin motivo para {malo:?}");
        }
    }

    /// Backspace retira un carácter UTF-8 COMPLETO cuando lo hay, y un byte
    /// suelto no-UTF8 cuando no (no forma parte de ningún carácter). El tope
    /// anti-paste sólo frena lo que se AÑADE.
    #[test]
    fn backspace_respeta_los_caracteres_y_los_bytes_sueltos() {
        let mut m = Modal::RenamePrompt {
            from: vp("mem:///d/x"),
            to_dir: vp("mem:///d"),
            name: "añ".as_bytes().to_vec(),
            error: None,
        };
        assert_eq!(on_key(&mut m, "backspace", None), ModalOutcome::StayOpen);
        let Modal::RenamePrompt { name, .. } = &m else {
            unreachable!()
        };
        assert_eq!(
            name.as_slice(),
            "a".as_bytes(),
            "la ñ entera, no medio byte"
        );

        let mut m = Modal::RenamePrompt {
            from: vp("mem:///d/x"),
            to_dir: vp("mem:///d"),
            name: vec![b'a', 0xFF],
            error: None,
        };
        on_key(&mut m, "backspace", None);
        let Modal::RenamePrompt { name, .. } = &m else {
            unreachable!()
        };
        assert_eq!(name.as_slice(), b"a", "el byte suelto se va solo");

        // Vacío: nada que retirar.
        on_key(&mut m, "backspace", None);
        let mut vacio = m;
        assert_eq!(on_key(&mut vacio, "backspace", None), ModalOutcome::Ignored);
    }

    /// El tope de bytes frena el paste sin bloquear la edición de un nombre
    /// que ya era más largo (borrar siempre vale).
    #[test]
    fn el_tope_del_nombre_solo_frena_lo_que_se_anade() {
        let largo = vec![b'x'; RENAME_NAME_MAX_BYTES + 10];
        let mut m = Modal::RenamePrompt {
            from: vp("mem:///d/x"),
            to_dir: vp("mem:///d"),
            name: largo.clone(),
            error: None,
        };
        assert_eq!(
            on_key(&mut m, "", Some("z")),
            ModalOutcome::Ignored,
            "no crece más"
        );
        assert_eq!(on_key(&mut m, "backspace", None), ModalOutcome::StayOpen);
        let Modal::RenamePrompt { name, .. } = &m else {
            unreachable!()
        };
        assert_eq!(name.len(), largo.len() - 1, "pero sí se puede acortar");
    }

    /// Esc cierra sin renombrar.
    #[test]
    fn esc_cierra_el_rename_sin_someter() {
        let mut m = Modal::RenamePrompt {
            from: vp("mem:///d/x"),
            to_dir: vp("mem:///d"),
            name: b"x".to_vec(),
            error: None,
        };
        assert_eq!(on_key(&mut m, "escape", None), ModalOutcome::Dismiss);
    }
}
