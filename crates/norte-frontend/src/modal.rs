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
    validate_ai_plan_in(entries, None)
}

/// Como [`validate_ai_plan`], y además exige que cada `from` EXISTA entre
/// `names` cuando se pasan.
///
/// El cinturón existe para sobrevivir a un daemon hostil o roto, y era más
/// flojo que el validador del que defiende (#275): `norte_core::ai` comprueba
/// tres cosas más que aquí no se miraban.
///
/// - **`!` no es un nombre**: es el marcador de archivo-como-directorio
///   (ADR 0018), y dejarlo pasar convierte un renombrado en una travesía
///   hacia dentro de un archivo.
/// - **`\` tampoco**: es separador en Windows, así que `..\evil` es un
///   traversal que `Segment` no ve porque solo mira `/`. Que
///   `norte-vfs-local::native_path` lo rechace después no lo arregla: es un
///   error lejos de su causa, y el cinturón está aquí precisamente para que
///   el error salga donde se puede explicar.
/// - **`from` tiene que existir** donde se va a aplicar. Sin esa comprobación
///   un plan adulterado puede renombrar algo que el lector no está mirando.
///   `None` en `names` significa «este llamante no tiene el listado
///   delante», no «da igual»: los dos frontends sí lo tienen y lo pasan.
///
/// UNA pareja inválida tumba el plan ENTERO, como antes: nunca un skip
/// silencioso que aplique «lo demás» de un plan adulterado.
///
/// ```
/// use norte_proto::methods::AiRenameEntry;
/// use norte_frontend::validate_ai_plan_in;
///
/// let e = AiRenameEntry { from: "a.txt".into(), to: "b.txt".into() };
/// assert!(validate_ai_plan_in(std::slice::from_ref(&e), Some(&[b"a.txt".to_vec()])).is_some());
/// // El mismo plan sobre un directorio donde `a.txt` no está: se rechaza.
/// assert!(validate_ai_plan_in(std::slice::from_ref(&e), Some(&[b"otro.txt".to_vec()])).is_none());
/// ```
#[must_use]
pub fn validate_ai_plan_in(
    entries: &[norte_proto::methods::AiRenameEntry],
    names: Option<&[Vec<u8>]>,
) -> Option<Vec<(Segment, Segment)>> {
    entries
        .iter()
        .map(|e| {
            let from = Segment::new(e.from.as_bytes().to_vec()).ok()?;
            let to = Segment::new(e.to.as_bytes().to_vec()).ok()?;
            if from.as_bytes() == b"!" || to.as_bytes() == b"!" {
                return None;
            }
            if to.as_bytes().contains(&b'\\') {
                return None;
            }
            if let Some(names) = names
                && !names.iter().any(|n| n.as_slice() == from.as_bytes())
            {
                return None;
            }
            Some((from, to))
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
    rename_pairs_in(entries, None)
}

/// Como [`rename_pairs`], con la comprobación de existencia de
/// [`validate_ai_plan_in`].
#[must_use]
pub fn rename_pairs_in(
    entries: &[norte_proto::methods::AiRenameEntry],
    names: Option<&[Vec<u8>]>,
) -> Option<Vec<norte_proto::methods::RenamePair>> {
    Some(
        validate_ai_plan_in(entries, names)?
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
/// Una línea del DETALLE de un veredicto, EN PARTES.
///
/// En partes y no en una cadena (#273): la forma anterior componía
/// `✗ { $n }. { $kind }: { $name }` aquí, y ni el `✗`, ni los dígitos, ni el
/// `.`, ni el `:` los enmascara `display_name` —son todos legítimos en un
/// nombre—, así que un fichero llamado `✗ 4. ya existe: otro.txt` producía
/// `✗ 3. ya existe: ✗ 4. ya existe: otro.txt`. Es la forma exacta que la
/// fixture `cause_join_spoof` del corpus existe para prohibir: la causa y la
/// ortografía del destino se separan FUERA de banda. Agrava que `norte-i18n`
/// llama a `set_use_isolating(false)`, así que Fluent no mete FSI/PDI
/// alrededor del placeable y un nombre con letras RTL fuertes reordena el
/// `✗`, el índice y el `:` dentro de la línea.
///
/// Cada frontend las coloca como pueda: la ventana con un elemento por
/// parte, el terminal poniendo el nombre en su propia línea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailPart {
    /// El planificador necesitó pasos temporales. No lleva nada de nadie.
    Temp {
        /// Cuántos.
        count: usize,
    },
    /// Una colisión concreta.
    Collision {
        /// Índice 1-based de la pareja, si de verdad señala una fila.
        index: Option<usize>,
        /// Clave Fluent del veredicto.
        kind_key: &'static str,
        /// El nombre, YA saneado y recortado. Es lo único que controla un
        /// tercero, y por eso viaja solo.
        name: String,
        /// El nombre difiere del real.
        hostile: bool,
    },
    /// Las colisiones que no caben.
    More {
        /// Cuántas se enseñan.
        shown: usize,
        /// Cuántas hay.
        total: usize,
        /// Alguna de las OCULTAS tiene nombre hostil.
        hostile: bool,
    },
}

/// el modal del rename IA necesita para poder confirmar.
///
/// Tres estados y no un `Option`, porque «todavía no ha contestado» y «no va
/// a contestar» no se le pueden enseñar igual al humano: el primero se
/// resuelve solo, el segundo no, y una etiqueta de «comprobando…» que no
/// avanza nunca es una mentira con forma de spinner.
///
/// El tipo es COMPARTIDO por todas las superficies, y con él toda la política
/// de presentación del veredicto ([`Self::status_key`],
/// [`Self::detail_parts`]): cada superficie pinta nombres que un atacante
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
    pub fn detail_parts(&self, pair_count: usize, lang: norte_i18n::Lang) -> Vec<DetailPart> {
        let mut out = Vec::new();
        let Some(plan) = self.ready() else {
            return out;
        };
        let temps = self.temp_steps();
        if temps > 0 {
            out.push(DetailPart::Temp { count: temps });
        }
        let shown = plan.collisions.len().min(RENAME_COLLISION_LIMIT);
        for c in plan.collisions.iter().take(shown) {
            let (name, hostile) = crate::display_name(c.name.as_bytes());
            // El presupuesto del nombre sale de lo que MIDE el prefijo ya
            // traducido, no de una constante adivinada contra la etiqueta más
            // corta.
            let kind_key = collision_kind_key(c.kind);
            let prefijo = crate::cells(&norte_i18n::ta_in(
                lang,
                "modal-rename-batch-collision-prefix",
                &[
                    ("n", &c.pair_index.saturating_add(1).to_string()),
                    ("kind", &norte_i18n::t_in(lang, kind_key)),
                ],
            ));
            let presupuesto = COLLISION_LINE_COLS
                .saturating_sub(prefijo)
                .max(COLLISION_NAME_MIN_COLS);
            out.push(DetailPart::Collision {
                // El índice de pareja es 1-based, como la etiqueta del
                // `from`, y solo viaja si de verdad señala una fila de la
                // petición: un daemon hostil que conteste «pareja 41» sobre
                // un plan de 3 no puede hacer que el modal señale una fila
                // que no existe.
                index: ((c.pair_index as usize) < pair_count)
                    .then(|| c.pair_index.saturating_add(1) as usize),
                kind_key,
                name: crate::middle_ellipsis(&name, presupuesto),
                hostile,
            });
        }
        if plan.collisions.len() > shown {
            // Lo escondido no se cuela limpio: una colisión OCULTA con nombre
            // hostil marca el resumen.
            let hostile = plan
                .collisions
                .iter()
                .skip(shown)
                .any(|c| crate::display_name(c.name.as_bytes()).1);
            out.push(DetailPart::More {
                shown,
                total: plan.collisions.len(),
                hostile,
            });
        }
        out
    }

    /// Cuántas líneas pinta [`Self::detail_parts`], sin construirlas. El alto
    /// del modal de la TUI se recalcula en CADA frame; interpolar Fluent y
    /// alocar un `Vec<String>` solo para contar sería trabajo por frame.
    #[must_use]
    pub fn detail_line_count(&self) -> usize {
        let Some(plan) = self.ready() else {
            return 0;
        };
        let shown = plan.collisions.len().min(RENAME_COLLISION_LIMIT);
        // DOS por colisión desde #273: la causa y el nombre no comparten
        // línea, para que un nombre no pueda fabricar la causa de otra.
        usize::from(self.temp_steps() > 0) + shown * 2 + usize::from(plan.collisions.len() > shown)
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

/// Si un plan se puede APROBAR: el core lo acepta **y** el lector ha llegado
/// al final.
///
/// Dos preguntas, y ninguna de las dos puede contestar la otra. Que el plan
/// sea ejecutable lo sabe el core y no el lector; que el lector lo haya visto
/// no lo sabe el core. Una aprobación es una firma, y la firma de algo que no
/// se ha leído no es una aprobación — con un plan de doscientos renombrados,
/// los que importan pueden estar en la fila ciento ochenta.
///
/// Vivía solo en la ventana: el terminal dejaba aprobar sin bajar, así que la
/// misma pregunta tenía dos respuestas en la superficie donde más caro sale
/// (ADR 0077). Aquí la respuesta es una.
///
/// `visto` es la marca de agua ALTA —hasta dónde se ha llegado alguna vez—,
/// no la posición actual: volver arriba no des-lee lo que ya se leyó.
///
/// ```
/// use norte_frontend::approval_ready;
///
/// // Visto entero y el core lo acepta.
/// assert!(approval_ready(true, 20, 20));
/// // Visto entero pero el core lo rechaza: no hay hash que mandar.
/// assert!(!approval_ready(false, 20, 20));
/// // El core lo acepta y el lector se ha quedado a mitad.
/// assert!(!approval_ready(true, 10, 20));
/// // Un plan vacío está visto por definición.
/// assert!(approval_ready(true, 0, 0));
/// ```
#[must_use]
pub fn approval_ready(plan_confirmable: bool, visto: usize, total: usize) -> bool {
    plan_confirmable && visto >= total
}

/// ¿Alguna de las rutas que NO se enseñan se pintaría alterada?
///
/// El badge de una ruta visible dice «lo que lees no son los bytes que hay».
/// Sobre lo recortado no se puede decir eso —no está delante para mirarlo—,
/// pero sí que ahí fuera hay algo así, que es lo que decide si merece la pena
/// ampliar antes de aprobar. `saltar` es cuántas se enseñan.
///
/// De los dos frontends porque es la misma pregunta sobre las mismas rutas y
/// sobre la superficie donde equivocarse sale más caro: el terminal lo decía
/// desde siempre y la ventana no (ADR 0077).
///
/// ```
/// use norte_proto::VPath;
/// use norte_frontend::overflow_hostile;
///
/// let limpia = VPath::parse("file:///casa/a.txt").unwrap();
/// let rara = VPath::parse("file:///casa/a%E2%80%AE.txt").unwrap();
///
/// // La hostil se ENSEÑA: el badge es suyo, no del resumen.
/// assert!(!overflow_hostile(&[rara.clone(), limpia.clone()], 2));
/// // La hostil se queda fuera: el resumen lo dice.
/// assert!(overflow_hostile(&[limpia.clone(), rara], 1));
/// // Nada recortado, nada que decir.
/// assert!(!overflow_hostile(&[limpia], 9));
/// ```
#[must_use]
pub fn overflow_hostile(rutas: &[VPath], saltar: usize) -> bool {
    rutas.iter().skip(saltar).any(|p| crate::path_display(p).1)
}

/// ¿Este texto YA REDACTADO se pintaría alterado?
///
/// Para las rutas que llegan como TEXTO y no como [`VPath`] — las de una
/// petición de aprobación, que el daemon manda redactadas porque los bytes
/// originales no salen de ahí.
///
/// Dos motivos para marcar, y el segundo es el que se olvida: comparar contra
/// el original no detecta nada, porque el daemon ya pasó los bytes por su
/// `display_lossy` y los controles, los overrides bidi y los bytes inválidos
/// YA son `U+FFFD`. Ese carácter ES la señal de que lo que se lee no es lo que
/// hay; no se puede recuperar qué había, pero sí decir que no es fiel.
///
/// ```
/// use norte_frontend::redacted_hostile;
/// assert!(!redacted_hostile("casa/a.txt"));
/// // Lo que el daemon ya sustituyó.
/// assert!(redacted_hostile("casa/a\u{FFFD}.txt"));
/// // Y lo que llega entero y hay que enmascarar aquí.
/// assert!(redacted_hostile("casa/a\u{200B}.txt"));
/// ```
#[must_use]
pub fn redacted_hostile(texto: &str) -> bool {
    norte_encoding::mask_terminal_hazards(texto) != texto || texto.contains('\u{FFFD}')
}

/// [`overflow_hostile`] para rutas que llegan como texto redactado.
///
/// ```
/// use norte_frontend::overflow_hostile_redacted;
/// let rutas = ["a.txt".to_owned(), "b\u{FFFD}.txt".to_owned()];
/// assert!(overflow_hostile_redacted(&rutas, 1), "la rara se queda fuera");
/// assert!(!overflow_hostile_redacted(&rutas, 2), "se enseñan las dos");
/// ```
#[must_use]
pub fn overflow_hostile_redacted(rutas: &[String], saltar: usize) -> bool {
    rutas.iter().skip(saltar).any(|p| redacted_hostile(p))
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

/// Una línea del informe de un lote de renombrado (`fs.rename_batch_report`).
///
/// Las frases van traducidas; las rutas NO se convierten en texto aquí,
/// porque cada frontend las pinta con su saneado y su badge. Lo que sí se
/// decide aquí es que una ruta va SOLA en su línea: metida en una frase, otra
/// ruta la puede suplantar (#273).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchReportLine {
    /// Una frase del informe, ya traducida. No lleva nada de nadie.
    Phrase(String),
    /// Una ruta que buscar: el nombre que lleva AHORA lo que se quedó a
    /// medias.
    Path(norte_proto::VPath),
}

/// `true` si el lote no dejó nada que buscar ni que rematar.
///
/// ```
/// use norte_proto::methods::FsRenameBatchReportResult;
/// let r = FsRenameBatchReportResult {
///     applied: 2, rolled_back: 0, failed_pair: None, stuck: None,
///     uncertain: None, compensations_lost: 0,
/// };
/// assert!(norte_frontend::batch_report_is_clean(&r));
/// ```
#[must_use]
pub fn batch_report_is_clean(r: &norte_proto::methods::FsRenameBatchReportResult) -> bool {
    r.stuck.is_none()
        && r.uncertain.is_none()
        && r.failed_pair.is_none()
        && r.compensations_lost == 0
        && r.rolled_back == 0
}

/// El cuerpo del informe de un lote: qué se aplicó, qué no se pudo devolver,
/// y CÓMO SE LLAMA AHORA lo que se quedó a medias.
///
/// `stuck` y `uncertain` pueden venir LOS DOS, y dicen cosas distintas: uno
/// es «no se pudo devolver», el otro «no se sabe si surtió efecto». Las
/// compensaciones perdidas son el único aviso de que un undo de sesión se
/// parará ahí.
#[must_use]
pub fn batch_report_lines(
    r: &norte_proto::methods::FsRenameBatchReportResult,
    lang: norte_i18n::Lang,
) -> Vec<BatchReportLine> {
    let frase = |clave: &str| BatchReportLine::Phrase(norte_i18n::t_in(lang, clave));
    let mut cuerpo = vec![BatchReportLine::Phrase(norte_i18n::ta_in(
        lang,
        "modal-batch-summary",
        &[
            ("applied", &r.applied.to_string()),
            ("back", &r.rolled_back.to_string()),
        ],
    ))];
    if let Some(paso) = &r.stuck {
        cuerpo.push(frase("modal-batch-stuck"));
        cuerpo.push(BatchReportLine::Path(paso.to.clone()));
        cuerpo.push(frase(if paso.journalled {
            "modal-batch-stuck-journalled"
        } else {
            "modal-batch-stuck-unjournalled"
        }));
    }
    if let Some(paso) = &r.uncertain {
        cuerpo.push(frase("modal-batch-uncertain"));
        cuerpo.push(BatchReportLine::Path(paso.to.clone()));
    }
    if r.compensations_lost > 0 {
        cuerpo.push(BatchReportLine::Phrase(norte_i18n::ta_in(
            lang,
            "modal-batch-compensations-lost",
            &[("n", &r.compensations_lost.to_string())],
        )));
    }
    cuerpo
}

#[cfg(test)]
mod batch_report_tests {
    use super::{BatchReportLine, batch_report_is_clean, batch_report_lines};
    use norte_proto::VPath;
    use norte_proto::methods::{FsRenameBatchReportResult, RenameStuckStep};

    fn paso(to: &str) -> RenameStuckStep {
        RenameStuckStep {
            from: VPath::parse("mem:///d/.norte-rename-1").expect("vpath"),
            to: VPath::parse(to).expect("vpath"),
            pair_index: 0,
            error: norte_proto::Error::Io { retryable: false },
            journalled: true,
            still_applied: 1,
        }
    }

    /// Con `stuck` Y `uncertain`, salen los dos, cada uno con su frase y su
    /// ruta en una línea propia.
    #[test]
    fn stuck_and_uncertain_both_show_each_path_on_its_own_line() {
        let r = FsRenameBatchReportResult {
            applied: 1,
            rolled_back: 1,
            failed_pair: Some(1),
            stuck: Some(paso("mem:///d/a")),
            uncertain: Some(paso("mem:///d/b")),
            compensations_lost: 2,
        };
        assert!(!batch_report_is_clean(&r));
        let lineas = batch_report_lines(&r, norte_i18n::Lang::En);
        let rutas: Vec<_> = lineas
            .iter()
            .filter_map(|l| match l {
                BatchReportLine::Path(p) => Some(p.clone()),
                BatchReportLine::Phrase(_) => None,
            })
            .collect();
        assert_eq!(
            rutas,
            vec![
                VPath::parse("mem:///d/a").expect("vpath"),
                VPath::parse("mem:///d/b").expect("vpath")
            ]
        );
        assert!(
            lineas
                .iter()
                .any(|l| matches!(l, BatchReportLine::Phrase(t) if t.contains("compensations"))),
            "las compensaciones perdidas se dicen: {lineas:?}"
        );
    }
}

#[cfg(test)]
mod ai_plan_tests {
    use super::validate_ai_plan;
    use norte_proto::methods::AiRenameEntry;

    pub(super) fn e(from: &str, to: &str) -> AiRenameEntry {
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
        // #275: lo que el validador del engine rechaza y el cinturón dejaba
        // pasar. `!` es el marcador de archivo-como-directorio (ADR 0018) y
        // `\\` es separador en Windows, o sea travesía que `Segment` no ve
        // porque solo mira `/`.
        assert!(validate_ai_plan(&[e("a.txt", "!")]).is_none());
        assert!(validate_ai_plan(&[e("!", "a.txt")]).is_none());
        assert!(validate_ai_plan(&[e("a.txt", "..\\evil")]).is_none());
        assert!(validate_ai_plan(&[e("a.txt", "sub\\x")]).is_none());
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
        assert!(
            BatchPlan::Pending
                .detail_parts(1, norte_i18n::active())
                .is_empty()
        );
        assert!(
            BatchPlan::Failed
                .detail_parts(1, norte_i18n::active())
                .is_empty()
        );
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
            let partes = plan.detail_parts(1, lang);
            let super::DetailPart::Collision { kind_key, name, .. } = &partes[0] else {
                panic!("{lang:?}: la parte es una colisión: {partes:?}");
            };
            assert_eq!(
                *kind_key,
                super::collision_kind_key(RenameCollisionKind::External)
            );
            assert_eq!(name, "zzzzz.txt");
            // El veredicto va en SU parte y el nombre en la suya: el orden lo
            // decide el frontend, y ninguno puede recortar al otro.
            assert!(
                !name.contains(&norte_i18n::t_in(lang, kind_key)),
                "{lang:?}: el nombre no lleva el veredicto dentro"
            );
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
                let partes = plan.detail_parts(1, lang);
                let super::DetailPart::Collision { kind_key, name, .. } = &partes[0] else {
                    panic!("{lang:?} {kind:?}: {partes:?}");
                };
                // El presupuesto sigue siendo de la LÍNEA entera: causa más
                // nombre. Quien se acorta es el nombre, y va MARCADO.
                let causa = norte_i18n::ta_in(
                    lang,
                    "modal-rename-batch-collision-prefix",
                    &[("n", "1"), ("kind", &norte_i18n::t_in(lang, kind_key))],
                );
                let celdas = crate::cells(&causa) + crate::cells(name);
                assert!(
                    celdas <= COLLISION_LINE_COLS,
                    "{lang:?} {kind:?}: {celdas} celdas > {COLLISION_LINE_COLS}"
                );
                assert!(name.contains('…'), "el recorte se MARCA: {name}");
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
            .detail_parts(1, norte_i18n::active())
            .remove(0)
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
        let partes = plan.detail_parts(3, norte_i18n::active());
        let super::DetailPart::Collision { index, name, .. } = &partes[0] else {
            panic!("{partes:?}");
        };
        assert_eq!(*index, None, "un índice imposible no viaja");
        assert_eq!(name, "z.txt", "el nombre sigue ahí");
        // Con la petición de verdad detrás, el índice SÍ viaja 1-based.
        let plan = listo(
            vec![colision(1, b"z.txt", RenameCollisionKind::Internal)],
            vec![],
            false,
        );
        let super::DetailPart::Collision { index, .. } =
            &plan.detail_parts(3, norte_i18n::active())[0]
        else {
            panic!("colisión");
        };
        assert_eq!(*index, Some(2));
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
            let partes = plan.detail_parts(n.max(1), norte_i18n::active());
            // Cada colisión pinta DOS líneas (causa y nombre); el aviso de
            // temporales una, y el resumen otra.
            let pintadas: usize = partes
                .iter()
                .map(|p| usize::from(matches!(p, super::DetailPart::Collision { .. })) + 1)
                .sum();
            assert_eq!(pintadas, plan.detail_line_count(), "n={n}");
            assert!(
                partes.len() <= 1 + RENAME_COLLISION_LIMIT + 1,
                "n={n}: {partes:?}"
            );
            if n > RENAME_COLLISION_LIMIT {
                let super::DetailPart::More { total, .. } = partes.last().expect("resumen") else {
                    panic!("n={n}: el último es el resumen: {partes:?}");
                };
                assert_eq!(*total, n, "n={n}");
            }
            // Un temporal se CUENTA, jamás se nombra.
            assert!(!partes.iter().any(|p| matches!(
                p,
                super::DetailPart::Collision { name, .. } if name.contains(".norte-rename-")
            )));
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
            let partes = plan.detail_parts(1, norte_i18n::active());
            assert_eq!(partes.len(), 1, "corpus {}: {partes:?}", fixture.id);
            let super::DetailPart::Collision { name, hostile, .. } = &partes[0] else {
                panic!("corpus {}: {partes:?}", fixture.id);
            };
            assert!(
                !name.chars().any(norte_encoding::is_terminal_hazard),
                "corpus {}: hazard vivo: {name:?}",
                fixture.id
            );
            // Ni un salto: un nombre no puede fabricar una línea de la lista.
            assert!(!name.contains('\n'), "corpus {}: {name:?}", fixture.id);
            assert_eq!(
                *hostile,
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

#[cfg(test)]
mod belt_tests {
    use super::ai_plan_tests::e;
    use super::validate_ai_plan_in;

    /// El `from` tiene que existir DONDE se va a aplicar (#275).
    ///
    /// Sin esto, un plan adulterado renombra algo que el lector no está
    /// mirando: la pantalla que aprueba enseña un directorio y la operación
    /// toca otro fichero del mismo.
    #[test]
    fn un_from_que_no_esta_en_el_listado_tumba_el_plan() {
        let nombres = vec![b"a.txt".to_vec(), b"b.txt".to_vec()];
        assert!(validate_ai_plan_in(&[e("a.txt", "c.txt")], Some(&nombres)).is_some());
        assert!(validate_ai_plan_in(&[e("z.txt", "c.txt")], Some(&nombres)).is_none());
        // Y una sola mala tumba el lote entero, como el resto del cinturón.
        assert!(
            validate_ai_plan_in(&[e("a.txt", "c.txt"), e("z.txt", "d.txt")], Some(&nombres))
                .is_none()
        );
    }

    /// Sin listado delante se comprueba la FORMA y nada más: `None` significa
    /// «este llamante no lo tiene», no «da igual».
    #[test]
    fn sin_listado_solo_se_comprueba_la_forma() {
        assert!(validate_ai_plan_in(&[e("z.txt", "c.txt")], None).is_some());
        assert!(validate_ai_plan_in(&[e("z.txt", "..")], None).is_none());
    }
}
