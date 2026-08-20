//! Política de LISTA de un modal de confirmación: cuántos ítems se pintan
//! antes de resumir el resto, y cómo se sanea cada uno.
//!
//! Vivía duplicada en la GUI (#103 T10 la sube aquí): un lote de copia,
//! movimiento o borrado se confirma sobre una LISTA de nombres, y esa lista
//! es superficie de ataque — un nombre hostil que cuele un `\n`, un override
//! bidi o un separador podría FABRICAR una entrada falsa y hacer que el
//! humano apruebe algo que no leyó. La regla es una sola para ambos
//! frontends: una ruta POR LÍNEA, siempre por [`display_name_with`]
//! (`crate::display_name_with`), y el badge del flag hostil.

use norte_proto::{Segment, VPath};

/// Cuántos ítems lista un modal antes de resumir el resto en «… y N más».
///
/// Es tope de LEGIBILIDAD, no de seguridad: el resumen final jamás calla
/// cuántos quedan fuera (un lote de 500 no puede parecer uno de 10).
pub const MODAL_ITEM_LIMIT: usize = 10;

/// Parejas del plan de rename IA (M4-IA) visibles a la vez en el modal del
/// plan (ventana de scroll, audit MAJOR-3: el plan ENTERO es revisable por
/// scroll — sin ventana, la cola de un plan largo se aplicaría sin poder
/// verse). Única fuente para el render, el alto del modal (TUI) y el clamp
/// del scroll en ambos frontends.
pub const AI_RENAME_PAIR_LIMIT: usize = 5;

/// Hits de la búsqueda semántica (M4-IA-2) visibles a la vez en el modal de
/// hits (ventana de scroll con cursor, molde [`AI_RENAME_PAIR_LIMIT`]).
/// Única fuente para el render, el alto del modal (TUI) y el clamp del
/// cursor en ambos frontends. El `k` que se PIDE al server es
/// [`SEMANTIC_K`]: mayor que esta ventana (el resto queda a un scroll).
pub const SEMANTIC_HIT_LIMIT: usize = 10;

/// `k` que ambos frontends piden a `index.search_semantic` (M4-IA-2): mayor
/// que la ventana del modal ([`SEMANTIC_HIT_LIMIT`] — el resto queda a un
/// scroll) y muy por debajo del techo contractual del server
/// (`INDEX_SEMANTIC_MAX_K` = 100, que además recorta por su cuenta). Única
/// fuente para TUI y GUI: pedir `k` distintos haría que la MISMA consulta
/// devolviera resultados distintos por frontend.
///
/// ```
/// assert!(norte_frontend::SEMANTIC_K as usize > norte_frontend::SEMANTIC_HIT_LIMIT);
/// assert!(norte_frontend::SEMANTIC_K <= norte_proto::methods::INDEX_SEMANTIC_MAX_K);
/// ```
pub const SEMANTIC_K: u32 = 20;

/// Tope de parejas que un frontend ACEPTA de `ai.rename_plan` (M4-IA,
/// cinturón de ingestión): el engine acota los planes legítimos MUY por
/// debajo (los basenames de UN directorio), así que un plan que lo supere
/// delata un daemon hostil/N+1 inflando la respuesta — se rechaza EN BLOQUE
/// (mismo mensaje que un plan adulterado), jamás se trocea ni se revisa "lo
/// que quepa".
pub const MAX_AI_PLAN_ENTRIES: usize = 256;

/// Valida TODAS las parejas del plan como [`Segment`] (cinturón fail-loud,
/// audit MAJOR-2, compartido por TUI y GUI): un plan bien formado del engine
/// JAMÁS trae un segmento inválido (el daemon los validó al armarlo), así
/// que UN rechazo aquí delata un daemon hostil/roto — `None` aborta el lote
/// ENTERO, jamás un skip silencioso que aplique «lo demás» de un plan
/// adulterado.
///
/// PURA a propósito: testeable sin backend (audit MINOR-6e).
///
/// ```
/// use norte_proto::methods::AiRenameEntry;
/// let ok = AiRenameEntry { from: "a.txt".into(), to: "b.txt".into() };
/// assert!(norte_frontend::validate_ai_plan(std::slice::from_ref(&ok)).is_some());
/// let evil = AiRenameEntry { from: "c.txt".into(), to: "../evil".into() };
/// // UNA pareja inválida tumba el plan ENTERO, aunque el resto sea legítimo.
/// assert!(norte_frontend::validate_ai_plan(&[ok, evil]).is_none());
/// ```
#[must_use]
pub fn validate_ai_plan(
    entries: &[norte_proto::methods::AiRenameEntry],
) -> Option<Vec<(Segment, Segment)>> {
    entries
        .iter()
        .map(|e| {
            Some((
                Segment::new(e.from.as_bytes().to_vec()).ok()?,
                Segment::new(e.to.as_bytes().to_vec()).ok()?,
            ))
        })
        .collect()
}

/// Las MISMAS parejas de [`validate_ai_plan`], ya en la forma que piden
/// `fs.rename_batch_plan` y `fs.rename_batch` (spec §17, ADR 0042).
///
/// Único convertidor plan-IA → parejas de lote para TUI y GUI: los dos
/// frontends mandan exactamente la misma INTENCIÓN, y por tanto el core les
/// contesta el mismo `plan_hash`. `None` con el mismo criterio fail-loud que
/// [`validate_ai_plan`] — un segmento inválido tumba el lote ENTERO.
///
/// ```
/// use norte_proto::methods::AiRenameEntry;
/// let e = AiRenameEntry { from: "ep1.mkv".into(), to: "ep01.mkv".into() };
/// let pares = norte_frontend::rename_pairs(std::slice::from_ref(&e)).expect("válido");
/// assert_eq!(pares[0].from.as_bytes(), b"ep1.mkv");
/// assert_eq!(pares[0].to.as_bytes(), b"ep01.mkv");
/// ```
#[must_use]
pub fn rename_pairs(
    entries: &[norte_proto::methods::AiRenameEntry],
) -> Option<Vec<norte_proto::methods::RenamePair>> {
    Some(
        validate_ai_plan(entries)?
            .into_iter()
            .map(|(from, to)| norte_proto::methods::RenamePair { from, to })
            .collect(),
    )
}

/// Colisiones del plan de lote que se pintan antes de resumir el resto
/// (molde [`AI_RENAME_PAIR_LIMIT`]). Tope de LEGIBILIDAD, no de seguridad: el
/// resumen final jamás calla cuántas quedan fuera, y ninguna colisión hace
/// aplicable un plan que el core marcó como no aplicable.
pub const RENAME_COLLISION_LIMIT: usize = 5;

/// Celdas a las que se acota la línea ENTERA de una colisión.
///
/// El presupuesto es de la LÍNEA, no del nombre, porque lo que hay que
/// impedir es el recorte mudo por la derecha que hace cada frontend cuando la
/// línea no cabe (el ancho de la caja en la TUI —suelo de 60 columnas, 56 de
/// interior con bordes y padding—, el `.truncate()` del div en la GUI). Del
/// presupuesto se descuenta lo que ocupa el prefijo YA TRADUCIDO —marca,
/// índice de pareja y veredicto— y lo que sobra es lo que se le da al nombre:
/// un veredicto largo (o un locale con etiquetas largas) acorta el nombre en
/// vez de empujarlo fuera de la caja sin marca.
const COLLISION_LINE_COLS: usize = 56;

/// Suelo de celdas para el nombre ofensor: por muy largo que sea el
/// veredicto, el nombre no se queda en nada. Si el prefijo se come el
/// presupuesto, quien se recorta es la línea —marcada por `middle_ellipsis`—
/// y no el nombre hasta desaparecer.
const COLLISION_NAME_MIN_COLS: usize = 12;

/// Clave Fluent del VEREDICTO de una colisión de lote (spec §17): única
/// fuente para TUI y GUI — el frontend pinta la etiqueta, jamás deduce el
/// veredicto.
///
/// [`RenameCollisionKind::Unknown`](norte_proto::methods::RenameCollisionKind)
/// (clase de un daemon N+1) tiene su propia
/// clave genérica: degrada UNA línea, jamás el modal entero.
///
/// ```
/// use norte_proto::methods::RenameCollisionKind as K;
/// assert_eq!(
///     norte_frontend::collision_kind_key(K::External),
///     "modal-rename-batch-collision-external",
/// );
/// // Un veredicto que este binario no conoce sigue teniendo etiqueta.
/// assert_eq!(
///     norte_frontend::collision_kind_key(K::Unknown),
///     "modal-rename-batch-collision-unknown",
/// );
/// ```
#[must_use]
pub fn collision_kind_key(kind: norte_proto::methods::RenameCollisionKind) -> &'static str {
    use norte_proto::methods::RenameCollisionKind as K;
    match kind {
        K::Internal => "modal-rename-batch-collision-internal",
        K::External => "modal-rename-batch-collision-external",
        K::AbsentSource => "modal-rename-batch-collision-absent-source",
        K::AmbiguousSource => "modal-rename-batch-collision-ambiguous-source",
        // Cualquier clase futura cae aquí (el enum es `non_exhaustive` y
        // `Unknown` es su fallback de deserialización): «rechazado, motivo que
        // no entiendo» es honesto; adivinar no lo sería.
        _ => "modal-rename-batch-collision-unknown",
    }
}

/// En qué punto está el plan de lote (`fs.rename_batch_plan`, spec §17) que
/// el modal del rename IA necesita para poder confirmar.
///
/// Tres estados y no un `Option`, porque «todavía no ha contestado» y «no va
/// a contestar» no se le pueden enseñar igual al humano: el primero se
/// resuelve solo, el segundo no, y una etiqueta de «comprobando…» que no
/// avanza nunca es una mentira con forma de spinner.
///
/// El tipo es COMPARTIDO por todas las superficies, y con él toda la política
/// de presentación del veredicto ([`Self::status_key`],
/// [`Self::detail_lines`]): cada superficie pinta nombres que un atacante
/// controla, y una que derive por su cuenta es exactamente cómo se pierde el
/// saneado en ella sin que nadie lo note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchPlan {
    /// Pedido al core y en vuelo: el modal abre y se rellena.
    Pending,
    /// El core contestó.
    Ready(Box<norte_proto::methods::FsRenameBatchPlanResult>),
    /// El core no pudo contestar (o ni se le pudo preguntar). El motivo
    /// concreto fue a la barra; aquí solo se sabe que NO hay plan, y sin plan
    /// no hay `plan_hash` aprobado que mandar.
    Failed,
}

impl BatchPlan {
    /// El plan si lo hay. `None` en [`Self::Pending`] y [`Self::Failed`].
    #[must_use]
    pub fn ready(&self) -> Option<&norte_proto::methods::FsRenameBatchPlanResult> {
        match self {
            Self::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// Si confirmar puede hacer algo: hace falta un plan y que el CORE lo
    /// haya marcado aplicable. Única fuente del gate en ambos frontends — la
    /// TUI enmudece sus comandos de confirmar y la GUI su tecla, y las dos
    /// preguntan aquí.
    ///
    /// Lee `executable`, JAMÁS `collisions.is_empty()`: el campo es normativo
    /// (ver su rustdoc en `norte_proto`) y un veredicto futuro puede parar un
    /// plan sin nombre ofensor que listar.
    ///
    /// ```
    /// use norte_frontend::BatchPlan;
    /// assert!(!BatchPlan::Pending.confirmable());
    /// assert!(!BatchPlan::Failed.confirmable());
    /// ```
    #[must_use]
    pub fn confirmable(&self) -> bool {
        self.ready().is_some_and(|p| p.executable)
    }

    /// Clave Fluent del ESTADO, la línea que va ARRIBA del modal (junto al
    /// dir, antes de las parejas): un modal más alto que el terminal se
    /// recorta por abajo, y de todo el cuerpo esta es la línea que no puede
    /// perderse.
    ///
    /// ```
    /// use norte_frontend::BatchPlan;
    /// assert_eq!(BatchPlan::Pending.status_key(), "modal-rename-batch-pending");
    /// assert_eq!(BatchPlan::Failed.status_key(), "modal-rename-batch-unchecked");
    /// ```
    #[must_use]
    pub fn status_key(&self) -> &'static str {
        match self {
            Self::Pending => "modal-rename-batch-pending",
            Self::Failed => "modal-rename-batch-unchecked",
            Self::Ready(p) if p.executable => "modal-rename-batch-applicable",
            Self::Ready(_) => "modal-rename-batch-not-applicable",
        }
    }

    /// Cuántos pasos del plan son MAQUINARIA del planificador (temporales que
    /// rompen un ciclo). Se cuentan, jamás se enseña el nombre: un
    /// `.norte-rename-…` no es nada que el humano haya pedido, y pintarlo
    /// entre sus parejas le haría creer que norte va a dejar ese nombre en su
    /// disco.
    #[must_use]
    pub fn temp_steps(&self) -> usize {
        self.ready()
            .map_or(0, |p| p.steps.iter().filter(|s| s.temp).count())
    }

    /// Renames que este lote va a hacer DE VERDAD: los pasos que no son
    /// maquinaria. No es `pairs.len()`: el planificador tira las parejas
    /// nulas (`from == to`), así que contar lo PEDIDO le prometería al humano
    /// más renombrados de los que el core se comprometió a hacer.
    #[must_use]
    pub fn real_steps(&self) -> usize {
        self.ready()
            .map_or(0, |p| p.steps.iter().filter(|s| !s.temp).count())
    }

    /// El DETALLE del veredicto, que va BAJO las parejas: la maquinaria del
    /// planificador (su número) y las colisiones, UNA POR LÍNEA, hasta
    /// [`RENAME_COLLISION_LIMIT`] más un resumen de las que no caben.
    ///
    /// Cada línea vuelve con su flag HOSTIL: el badge lo pone cada frontend
    /// (la TUI usa `!` ASCII, la GUI `⚠`), pero el saneado —quién se
    /// enmascara, quién se acorta y por dónde— se decide aquí una sola vez.
    ///
    /// `pair_count` es cuántas parejas tiene la petición, y sirve para UNA
    /// cosa: un `pair_index` que se salga de ella no se pinta. Un daemon
    /// hostil que conteste «pareja 41» sobre un plan de 3 no puede hacer que
    /// el modal señale una fila que no existe; la línea pierde el índice y
    /// conserva el veredicto.
    #[must_use]
    pub fn detail_lines(&self, pair_count: usize) -> Vec<(String, bool)> {
        let mut lines = Vec::new();
        let Some(plan) = self.ready() else {
            return lines;
        };
        let temps = self.temp_steps();
        if temps > 0 {
            lines.push((
                norte_i18n::ta("modal-rename-batch-temp", &[("n", &temps.to_string())]),
                false,
            ));
        }
        let shown = plan.collisions.len().min(RENAME_COLLISION_LIMIT);
        for c in plan.collisions.iter().take(shown) {
            let (name, hostil) = crate::display_name(c.name.as_bytes());
            let kind = norte_i18n::t(collision_kind_key(c.kind));
            // El índice de pareja es 1-based, como la etiqueta del `from`, y
            // solo se pinta si de verdad señala una fila de la petición.
            let indexado = (c.pair_index as usize) < pair_count;
            let render = |name: &str| {
                if indexado {
                    norte_i18n::ta(
                        "modal-rename-batch-collision",
                        &[
                            ("n", &c.pair_index.saturating_add(1).to_string()),
                            ("kind", &kind),
                            ("name", name),
                        ],
                    )
                } else {
                    norte_i18n::ta(
                        "modal-rename-batch-collision-unindexed",
                        &[("kind", &kind), ("name", name)],
                    )
                }
            };
            // El presupuesto del nombre sale de lo que MIDE el prefijo ya
            // traducido, no de una constante adivinada contra la etiqueta más
            // corta: se pinta la línea con el nombre vacío, se mide, y lo que
            // queda del presupuesto de línea es lo que se le da al nombre.
            let prefijo = crate::cells(&render(""));
            let presupuesto = COLLISION_LINE_COLS
                .saturating_sub(prefijo)
                .max(COLLISION_NAME_MIN_COLS);
            lines.push((render(&crate::middle_ellipsis(&name, presupuesto)), hostil));
        }
        if plan.collisions.len() > shown {
            // Lo escondido no se cuela limpio (molde del indicador de
            // parejas): una colisión OCULTA con nombre hostil marca el
            // resumen.
            let hidden_hostil = plan
                .collisions
                .iter()
                .skip(shown)
                .any(|c| crate::display_name(c.name.as_bytes()).1);
            lines.push((
                norte_i18n::ta(
                    "modal-rename-batch-collision-more",
                    &[
                        ("shown", &shown.to_string()),
                        ("total", &plan.collisions.len().to_string()),
                    ],
                ),
                hidden_hostil,
            ));
        }
        lines
    }

    /// Cuántas líneas pinta [`Self::detail_lines`], sin construirlas. El alto
    /// del modal de la TUI se recalcula en CADA frame; interpolar Fluent y
    /// alocar un `Vec<String>` solo para contar sería trabajo por frame.
    #[must_use]
    pub fn detail_line_count(&self) -> usize {
        let Some(plan) = self.ready() else {
            return 0;
        };
        let shown = plan.collisions.len().min(RENAME_COLLISION_LIMIT);
        usize::from(self.temp_steps() > 0) + shown + usize::from(plan.collisions.len() > shown)
    }
}

/// Cinturón de INGESTIÓN de los hits semánticos (M4-IA-2, paridad con el
/// belt del plan IA, compartido por TUI y GUI): un daemon CONFORME jamás
/// supera [`norte_proto::methods::INDEX_SEMANTIC_MAX_K`] (el server recorta
/// `k` a ese techo contractual) ni emite scores no finitos (el engine los
/// filtra) — superar el techo o colar un NaN/∞ delata un daemon hostil/N+1
/// inflando o envenenando la respuesta. `None` = rechazo EN BLOQUE (cero
/// hits pintados, jamás un recorte silencioso); `Some` devuelve los hits
/// intactos.
///
/// PURA a propósito: testeable sin backend, como [`validate_ai_plan`].
///
/// ```
/// use norte_proto::VPath;
/// use norte_proto::methods::SemanticHit;
/// let ok = SemanticHit { path: VPath::parse("mem:///a").unwrap(), score: 0.9 };
/// assert!(norte_frontend::validate_semantic_hits(vec![ok.clone()]).is_some());
/// // UN score no finito tumba la respuesta ENTERA, aunque el resto sea legítimo.
/// let evil = SemanticHit { path: VPath::parse("mem:///b").unwrap(), score: f64::NAN };
/// assert!(norte_frontend::validate_semantic_hits(vec![ok, evil]).is_none());
/// ```
#[must_use]
pub fn validate_semantic_hits(
    hits: Vec<norte_proto::methods::SemanticHit>,
) -> Option<Vec<norte_proto::methods::SemanticHit>> {
    (hits.len() <= norte_proto::methods::INDEX_SEMANTIC_MAX_K as usize
        && hits.iter().all(|h| h.score.is_finite()))
    .then_some(hits)
}

/// Badge por defecto de [`item_lines`]: el aviso que ya usaba el modal de la
/// GUI. Los frontends con un badge propio (el TUI usa `!`, ASCII, por los
/// terminales que no pintan `⚠`) pasan el suyo a [`item_lines_with`] — el
/// crate no elige badge, solo garantiza que el flag hostil se MARCA.
const DEFAULT_BADGE: &str = "⚠";

/// Hasta [`MODAL_ITEM_LIMIT`] nombres saneados (una línea por ítem, jamás
/// dos rutas en la misma); si sobran, una línea final localizada con cuántos
/// quedan fuera.
///
/// Pinta el NOMBRE de cada ítem, no la ruta entera: en un lote todos
/// comparten directorio (el del pane) y el destino va en su propia línea.
///
/// ```
/// use norte_proto::VPath;
/// let items: Vec<VPath> = (0..12)
///     .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
///     .collect();
/// let lines = norte_frontend::item_lines(&items);
/// assert_eq!(lines.len(), norte_frontend::MODAL_ITEM_LIMIT + 1);
/// assert_eq!(lines[0], "f0");
/// // La última RESUME los que no caben: 12 - 10 = 2.
/// assert!(lines.last().unwrap().contains('2'));
/// ```
#[must_use]
pub fn item_lines(items: &[VPath]) -> Vec<String> {
    item_lines_with(items, DEFAULT_BADGE, None)
}

/// [`item_lines`] con el badge del frontend y la REINTERPRETACIÓN de nombres
/// del pane origen (#57): el diálogo debe pintar el MISMO texto por el que el
/// usuario navegó — con un pane en cp866, confirmar un borrado mostrando el
/// lossy `�����` en vez de `Папка` sería preguntar por otra cosa.
///
/// ```
/// use norte_encoding::NameEncoding;
/// use norte_proto::VPath;
/// let items = vec![VPath::parse("mem:///CAF%90.TXT").unwrap()];
/// let lines = norte_frontend::item_lines_with(&items, "!", Some(NameEncoding::Cp437));
/// // Reinterpretado Y marcado: el texto pintado no son los bytes.
/// assert_eq!(lines, vec!["! CAFÉ.TXT".to_string()]);
/// ```
#[must_use]
pub fn item_lines_with(
    items: &[VPath],
    badge: &str,
    reinterpret: Option<norte_encoding::NameEncoding>,
) -> Vec<String> {
    let mut lines: Vec<String> = items
        .iter()
        .take(MODAL_ITEM_LIMIT)
        .map(|p| {
            let bytes = p.file_name().map_or(&b""[..], Segment::as_bytes);
            let (name, hostile) = crate::display_name_with(bytes, reinterpret);
            if hostile {
                format!("{badge} {name}")
            } else {
                name
            }
        })
        .collect();
    if items.len() > MODAL_ITEM_LIMIT {
        let n = (items.len() - MODAL_ITEM_LIMIT).to_string();
        // Clave heredada de la GUI (GUI-e T1): la comparte ahora el TUI —
        // renombrarla no cambiaría el texto y rompería las traducciones.
        lines.push(norte_i18n::ta("gui-modal-more", &[("n", n.as_str())]));
    }
    lines
}

#[cfg(test)]
mod ai_plan_tests {
    use super::validate_ai_plan;
    use norte_proto::methods::AiRenameEntry;

    fn e(from: &str, to: &str) -> AiRenameEntry {
        AiRenameEntry {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Audit MAJOR-2 (fail-loud): UNA pareja inválida — traversal `..`,
    /// separador embebido o nombre vacío — tumba el plan ENTERO (`None`),
    /// jamás un skip silencioso que aplique "lo demás" de un plan
    /// adulterado por un daemon hostil/roto.
    #[test]
    fn una_pareja_invalida_tumba_el_plan_entero() {
        assert!(validate_ai_plan(&[e("a", "b"), e("c", "..")]).is_none());
        assert!(validate_ai_plan(&[e("a/b", "c"), e("d", "e")]).is_none());
        assert!(validate_ai_plan(&[e("", "x")]).is_none());
        assert!(validate_ai_plan(&[e("ok", "tambien-ok"), e("x", "a/b")]).is_none());
    }

    /// Un plan bien formado conserva orden y longitud, bytes exactos.
    #[test]
    fn un_plan_valido_conserva_orden_y_longitud() {
        let pairs = validate_ai_plan(&[e("a", "b"), e("c", "d")]).expect("plan válido");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0.as_bytes(), b"a");
        assert_eq!(pairs[0].1.as_bytes(), b"b");
        assert_eq!(pairs[1].0.as_bytes(), b"c");
        assert_eq!(pairs[1].1.as_bytes(), b"d");
    }

    /// El plan vacío es válido (los frontends no encolan nada con él).
    #[test]
    fn un_plan_vacio_es_valido() {
        assert_eq!(validate_ai_plan(&[]).expect("vacío válido").len(), 0);
    }

    /// Regla 1, en la costura: `AiRenameEntry` viaja como `String` porque el
    /// core rechaza fail-loud un dir con nombres no-UTF8 ANTES de llamar al
    /// proveedor, así que su `from_utf8_lossy` es la identidad. Eso valía
    /// mientras el valor solo se PINTABA; ahora es el `from` de un rename de
    /// verdad, y lo que hay que pinear es qué pasa si esa premisa se rompiera:
    /// un `U+FFFD` es un `Segment` perfectamente legal, así que la pareja
    /// viaja tal cual y el core contesta `AbsentSource` — el lote no ejecuta
    /// NADA. Falla cerrado, nunca renombra el fichero equivocado.
    #[test]
    fn un_nombre_con_residuo_lossy_viaja_tal_cual_y_muere_en_el_core() {
        let pares = super::rename_pairs(&[e("caf\u{FFFD}.txt", "cafe.txt")]).expect("segmento");
        assert_eq!(
            pares[0].from.as_bytes(),
            "caf\u{FFFD}.txt".as_bytes(),
            "el frontend no inventa bytes: manda lo que le dieron",
        );
    }
}

#[cfg(test)]
mod batch_plan_tests {
    use super::{BatchPlan, COLLISION_LINE_COLS, RENAME_COLLISION_LIMIT};
    use norte_i18n::Lang;
    use norte_proto::Segment;
    use norte_proto::methods::{
        FsRenameBatchPlanResult, PlanHash, RenameCollision, RenameCollisionKind, RenameStep,
    };

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("segmento")
    }

    fn listo(
        collisions: Vec<RenameCollision>,
        steps: Vec<RenameStep>,
        executable: bool,
    ) -> BatchPlan {
        BatchPlan::Ready(Box::new(FsRenameBatchPlanResult {
            steps,
            collisions,
            executable,
            plan_hash: PlanHash::parse(&"0".repeat(64)).expect("64 hex"),
        }))
    }

    fn colision(pair_index: u32, name: &[u8], kind: RenameCollisionKind) -> RenameCollision {
        RenameCollision {
            pair_index,
            name: seg(name),
            kind,
        }
    }

    /// Los tres estados dicen cosas DISTINTAS y solo uno deja confirmar. Sin
    /// esto, «en vuelo» y «no se pudo comprobar» se pintarían igual y el
    /// segundo sería un spinner que no avanza nunca.
    #[test]
    fn los_tres_estados_no_se_confunden() {
        assert!(!BatchPlan::Pending.confirmable());
        assert!(!BatchPlan::Failed.confirmable());
        assert!(!listo(vec![], vec![], false).confirmable());
        assert!(listo(vec![], vec![], true).confirmable());
        let claves = [
            BatchPlan::Pending.status_key(),
            BatchPlan::Failed.status_key(),
            listo(vec![], vec![], true).status_key(),
            listo(vec![], vec![], false).status_key(),
        ];
        for (i, a) in claves.iter().enumerate() {
            for b in &claves[i + 1..] {
                assert_ne!(a, b, "dos estados con la misma etiqueta: {a}");
            }
        }
        // Sin plan no hay detalle que pintar (ni una línea fantasma).
        assert!(BatchPlan::Pending.detail_lines(1).is_empty());
        assert!(BatchPlan::Failed.detail_lines(1).is_empty());
    }

    /// El VEREDICTO es lo accionable y va antes del nombre, en los DOS
    /// locales: un traductor que reordenase `{ $name }` delante de `{ $kind }`
    /// dejaría el veredicto a merced del recorte mudo por la derecha.
    #[test]
    fn el_veredicto_precede_al_nombre_en_los_dos_locales() {
        let plan = listo(
            vec![colision(0, b"zzzzz.txt", RenameCollisionKind::External)],
            vec![],
            false,
        );
        for lang in [Lang::En, Lang::Es] {
            let _ = norte_i18n::force(lang);
            let verdicto = norte_i18n::t(super::collision_kind_key(RenameCollisionKind::External));
            let (linea, _) = plan.detail_lines(1).remove(0);
            let iv = linea.find(&verdicto).expect("el veredicto está");
            let inom = linea.find("zzzzz.txt").expect("el nombre está");
            assert!(iv < inom, "{lang:?}: veredicto DESPUÉS del nombre: {linea}");
        }
        let _ = norte_i18n::force(Lang::En);
    }

    /// El presupuesto es de la LÍNEA: con la etiqueta más larga de cada
    /// locale y un nombre kilométrico, la línea entera sigue cabiendo — quien
    /// se acorta es el nombre, y el recorte va MARCADO.
    #[test]
    fn la_linea_entera_cabe_en_su_presupuesto_en_los_dos_locales() {
        let largo = vec![b'x'; 300];
        for lang in [Lang::En, Lang::Es] {
            let _ = norte_i18n::force(lang);
            for kind in [
                RenameCollisionKind::Internal,
                RenameCollisionKind::External,
                RenameCollisionKind::AbsentSource,
                RenameCollisionKind::AmbiguousSource,
                RenameCollisionKind::Unknown,
            ] {
                let plan = listo(vec![colision(0, &largo, kind)], vec![], false);
                let (linea, _) = plan.detail_lines(1).remove(0);
                assert!(
                    crate::cells(&linea) <= COLLISION_LINE_COLS,
                    "{lang:?} {kind:?}: {} celdas > {COLLISION_LINE_COLS}: {linea}",
                    crate::cells(&linea),
                );
                assert!(linea.contains('…'), "el recorte se MARCA: {linea}");
            }
        }
        let _ = norte_i18n::force(Lang::En);
    }

    /// Dos nombres que solo se distinguen por la COLA no pueden renderizarse
    /// idénticos: si el presupuesto se los come, la elipsis media conserva la
    /// cola. (Mutación de control: cambiar `middle_ellipsis` por un truncado
    /// por la derecha rompe este test.)
    #[test]
    fn dos_nombres_gemelos_por_la_cola_no_se_pintan_iguales() {
        let _ = norte_i18n::force(Lang::Es);
        let v2 = b"factura-2024-enero-final-revisada-v2.pdf";
        let v3 = b"factura-2024-enero-final-revisada-v3.pdf";
        let render = |n: &[u8]| {
            listo(
                vec![colision(0, n, RenameCollisionKind::Unknown)],
                vec![],
                false,
            )
            .detail_lines(1)
            .remove(0)
            .0
        };
        assert_ne!(render(v2), render(v3), "la cola distingue, y sobrevive");
        let _ = norte_i18n::force(Lang::En);
    }

    /// Un `pair_index` que no señala ninguna fila de la petición se CAE: un
    /// daemon hostil no puede hacer que el modal apunte a una pareja que no
    /// existe. El veredicto y el nombre siguen ahí.
    #[test]
    fn un_indice_fuera_de_rango_no_senala_una_fila_inexistente() {
        let _ = norte_i18n::force(Lang::En);
        let plan = listo(
            vec![colision(u32::MAX, b"z.txt", RenameCollisionKind::Internal)],
            vec![],
            false,
        );
        let (linea, _) = plan.detail_lines(3).remove(0);
        assert!(
            !linea.contains("4294967295") && !linea.contains("4294967296"),
            "{linea}"
        );
        assert!(linea.contains("z.txt"), "{linea}");
        // Con la petición de verdad detrás, el índice SÍ se pinta 1-based.
        let plan = listo(
            vec![colision(1, b"z.txt", RenameCollisionKind::Internal)],
            vec![],
            false,
        );
        assert!(plan.detail_lines(3).remove(0).0.contains("2."));
    }

    /// El tope de colisiones se respeta, el resumen no calla cuántas quedan
    /// fuera, y `detail_line_count` cuenta EXACTAMENTE lo que se pinta (el
    /// alto del modal de la TUI se calcula con él).
    #[test]
    fn el_tope_de_colisiones_y_el_contador_van_a_una() {
        let _ = norte_i18n::force(Lang::En);
        for n in [
            0usize,
            1,
            RENAME_COLLISION_LIMIT,
            RENAME_COLLISION_LIMIT + 3,
        ] {
            let cs: Vec<_> = (0..n)
                .map(|i| {
                    colision(
                        u32::try_from(i).expect("cabe"),
                        format!("f{i}.txt").as_bytes(),
                        RenameCollisionKind::Internal,
                    )
                })
                .collect();
            let plan = listo(
                cs,
                vec![RenameStep {
                    from: seg(b"a"),
                    to: seg(b".norte-rename-0"),
                    temp: true,
                }],
                false,
            );
            let lines = plan.detail_lines(n.max(1));
            assert_eq!(lines.len(), plan.detail_line_count(), "n={n}");
            assert!(
                lines.len() <= 1 + RENAME_COLLISION_LIMIT + 1,
                "n={n}: {lines:?}"
            );
            if n > RENAME_COLLISION_LIMIT {
                let (resumen, _) = lines.last().expect("resumen");
                assert!(resumen.contains(&n.to_string()), "n={n}: {resumen}");
            }
            // Un temporal se CUENTA, jamás se nombra.
            assert!(!lines.iter().any(|(l, _)| l.contains(".norte-rename-")));
        }
    }

    /// Cada nombre del corpus canónico: ninguna línea trae un hazard, ninguna
    /// se parte en dos, y el enmascarado viene MARCADO para que el frontend
    /// pueda ponerle su badge.
    #[test]
    fn barrido_del_corpus_en_el_nombre_ofensor() {
        let _ = norte_i18n::force(Lang::En);
        for fixture in norte_testkit::corpus::hostile_names() {
            let plan = listo(
                vec![colision(0, &fixture.bytes, RenameCollisionKind::External)],
                vec![],
                false,
            );
            let lines = plan.detail_lines(1);
            assert_eq!(lines.len(), 1, "corpus {}: {lines:?}", fixture.id);
            let (fila, marcado) = &lines[0];
            assert!(
                !fila.chars().any(norte_encoding::is_terminal_hazard),
                "corpus {}: hazard vivo: {fila:?}",
                fixture.id
            );
            assert!(!fila.contains('\n'), "corpus {}: {fila:?}", fixture.id);
            assert_eq!(
                *marcado,
                crate::display_name(&fixture.bytes).1,
                "corpus {}: el flag hostil tiene que llegar al frontend",
                fixture.id
            );
        }
    }
}

#[cfg(test)]
mod semantic_hits_tests {
    use super::validate_semantic_hits;
    use norte_proto::VPath;
    use norte_proto::methods::{INDEX_SEMANTIC_MAX_K, SemanticHit};

    fn hits(n: usize) -> Vec<SemanticHit> {
        (1..=n)
            .map(|i| SemanticHit {
                path: VPath::parse(&format!("mem:///d/f{i}")).expect("wire válido"),
                score: 0.5,
            })
            .collect()
    }

    /// M4-IA-2 (paridad IA-1 con el belt del plan): el cinturón acepta hasta
    /// el techo contractual del server (`INDEX_SEMANTIC_MAX_K` — un daemon
    /// conforme jamás lo supera) con los hits INTACTOS, y rechaza EN BLOQUE
    /// una respuesta inflada (daemon hostil/N+1) — jamás un recorte
    /// silencioso.
    #[test]
    fn el_techo_exacto_pasa_y_uno_mas_se_rechaza_en_bloque() {
        let max = usize::try_from(INDEX_SEMANTIC_MAX_K).expect("techo pequeño");
        let ok = validate_semantic_hits(hits(max));
        assert_eq!(
            ok.as_ref().map(Vec::len),
            Some(max),
            "el techo exacto pasa intacto"
        );
        assert!(
            validate_semantic_hits(hits(max + 1)).is_none(),
            "uno más = rechazo en bloque"
        );
    }

    /// UN score no finito (NaN/∞ — el engine los filtra, así que solo un
    /// daemon hostil/roto los emite) tumba la respuesta ENTERA, aunque el
    /// resto sea legítimo.
    #[test]
    fn un_score_no_finito_tumba_la_respuesta_entera() {
        for evil in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut lote = hits(3);
            lote[1].score = evil;
            assert!(
                validate_semantic_hits(lote).is_none(),
                "score {evil} debe rechazar en bloque"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::VPath;

    /// La lista de un modal se corta en [`super::MODAL_ITEM_LIMIT`] y RESUME
    /// el resto en una última línea localizada — jamás pinta 500 rutas ni,
    /// peor, calla las que no caben.
    #[test]
    fn the_modal_lists_the_first_items_and_summarises_the_rest() {
        let many: Vec<VPath> = (0..20)
            .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
            .collect();
        let lines = crate::item_lines(&many);
        assert_eq!(lines.len(), crate::MODAL_ITEM_LIMIT + 1);
        assert!(
            lines
                .last()
                .unwrap()
                .contains(&(20 - crate::MODAL_ITEM_LIMIT).to_string())
        );
    }

    /// Un lote que cabe entero NO lleva línea de resumen (ni un «y 0 más»).
    #[test]
    fn a_batch_that_fits_has_no_summary_line() {
        let few: Vec<VPath> = (0..crate::MODAL_ITEM_LIMIT)
            .map(|i| VPath::parse(&format!("mem:///f{i}")).unwrap())
            .collect();
        assert_eq!(crate::item_lines(&few).len(), crate::MODAL_ITEM_LIMIT);
    }

    /// El corpus hostil entero: ni una línea deja un hazard crudo, ni una
    /// línea contiene un salto (una ruta por línea, siempre) — la lista no
    /// se puede FABRICAR desde un nombre.
    #[test]
    fn item_lines_never_leak_raw_hazards_nor_forge_a_line() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let Ok(seg) = norte_proto::Segment::new(fixture.bytes.clone()) else {
                continue; // un nombre no representable como segmento no llega aquí
            };
            let p = VPath::root(norte_proto::Scheme::new("mem").unwrap(), None).join(seg);
            let lines = crate::item_lines(std::slice::from_ref(&p));
            assert_eq!(lines.len(), 1, "{}: una línea por ítem", fixture.id);
            let line = &lines[0];
            assert!(
                !line.chars().any(norte_encoding::is_terminal_hazard),
                "{}: hazard crudo en {line:?}",
                fixture.id,
            );
            assert!(
                !line.contains('\n'),
                "{}: un nombre no puede fabricar una línea: {line:?}",
                fixture.id,
            );
        }
    }
}
