//! El `plan_hash`: la huella de lo que un humano aprueba cuando aprueba un
//! plan.
//!
//! Un [`PlanHasher`] se siembra con la INTENCIÓN del plan —las dos raíces, el
//! modo, `on_unknown` y las opciones de comparación— y luego traga los
//! elementos del flujo UNO a UNO, en orden. No guarda ninguno: su estado es un
//! `Sha256` y un contador, así que planificar medio millón de pasos cuesta lo
//! mismo en memoria que planificar tres (regla dura 3: una Task no se puede
//! permitir juntar el plan entero solo para hashearlo).
//!
//! # Qué entra y qué no
//! Entran las CONCLUSIONES: qué se va a hacer, sobre qué ruta del origen, sobre
//! qué ruta del destino, con qué tamaño, con qué criterio y confianza, con qué
//! reversa y con qué motivo — y cada bloqueo. NO entra el `id` del paso, que es
//! presentación: un filtro que renumere el panel no puede invalidar una
//! aprobación.
//!
//! # Cómo se alimenta (y por qué no basta con concatenar)
//! Cada campo va con su LONGITUD delante y cada opcional con un byte de
//! presencia, igual que la cadena del journal (ADR 0023) y que el `plan_hash`
//! del lote de renames. Sin el prefijo, `"ab" + "c"` y `"a" + "bc"` producen el
//! mismo digest y dos planes que escriben en rutas DISTINTAS comparan iguales;
//! sin el byte de presencia, «no hay `dest_rel`» y «hay un `dest_rel` que es la
//! raíz» tampoco se distinguen — y un `Skip` puede llevar legítimamente un `rel`
//! raíz, que es exactamente la secuencia de cero bytes.
//!
//! Cada elemento va además con una ETIQUETA de clase, para que un `Skip` y un
//! bloqueo en la misma `rel` no colisionen.
//!
//! De los enums se alimenta el NOMBRE serde, jamás el discriminante:
//! `TypeMismatchDir` se insertó ANTES de `Unknown` cuando ya había código
//! escrito, y la siguiente variante también se insertará por en medio. Un
//! digest sobre el discriminante habría convertido esa inserción en una
//! aprobación que autoriza otro plan.
//!
//! # Se alimenta del flujo que el humano VE, no de otro
//! Normativo: al hasher se le dan EXACTAMENTE los elementos que acaban en el
//! plan que se enseña y que se resume en
//! [`SyncPlanDone`](norte_proto::methods::SyncPlanDone) — o sea, después de
//! aplicar el `include` de la petición, que filtra la SALIDA del transductor
//! (ver [`plan`](crate::plan())). Hashear el flujo sin filtrar y enseñar el
//! filtrado acuña un testigo para un plan que nadie aprobó, y es la única forma
//! de cablearlo mal que ni el tipo ni los tests pueden atrapar. Por eso el
//! `include` no se siembra: los elementos que quedan YA llevan su efecto, y
//! sembrarlo además invitaría a creer que da igual con qué flujo se alimente.
//! Lo mismo vale para [`SyncCounts`](norte_proto::methods::SyncCounts).
//!
//! # Lo que este hash NO puede hacer solo
//! Un plan cuyo destino es de solo lectura produce EXACTAMENTE un elemento —su
//! bloqueo— sea cual sea el árbol, así que todos ellos hashean igual. Es
//! correcto (esos planes no tienen conclusiones que distinguir) y tiene una
//! consecuencia que quien ejecute debe conocer: **`sync.apply` decide por
//! [`SyncPlanDone::executable`](norte_proto::methods::SyncPlanDone::executable),
//! no por que un hash case**. Un hash que case dice «este es el plan que se te
//! enseñó», nunca «este plan se puede ejecutar».
//!
//! Hay una segunda igualdad legítima, y por el mismo motivo: el `rel` de un paso
//! se mide contra la raíz del lado del que HABLA, y el paso no lleva un campo que
//! diga cuál (ver [`SyncStep::rel`]). Un `Skip` por un listado ilegible del
//! ORIGEN y otro por uno del DESTINO, en el mismo nombre, salen byte a byte
//! iguales y hashean igual. Las dos formas afectadas —ese `Skip` y un bloqueo de
//! solape alcanzado desde un lado o desde el otro— no ESCRIBEN nada, así que
//! ningún par de planes que escriba distinto puede compartir huella; lo que se
//! pierde es una distinción de lectura, no de efecto.
//!
//! # No es un formato persistido
//! El hash identifica un plan RETENIDO en el spool, con el TTL de
//! [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS), y lo produce y
//! lo consume el mismo binario dentro de esa ventana. No hay journals viejos
//! que se invaliden si este framing cambia, al revés que en ADR 0023 — pero
//! cambiarlo sí invalida los planes en vuelo, así que se cambia en un despliegue
//! y no a la ligera.

use std::borrow::Cow;
use std::fmt;

use norte_proto::VPath;
use norte_proto::methods::{
    CompareConfidence, CompareCriteria, CompareCriterion, DescendSide, OnUnknown, PlanHash,
    RelPath, Side, StepReversal, SyncBlocker, SyncBlockerKind, SyncCompareOptions, SyncMode,
    SyncReason, SyncStep, SyncStepKind,
};
use sha2::{Digest, Sha256};

use crate::{PlanItem, SyncOptions};

/// Etiqueta de un paso dentro del digest.
const TAG_STEP: u8 = b'S';
/// Etiqueta de un bloqueo. Distinta de [`TAG_STEP`] para que un
/// [`SyncStepKind::Skip`] y un bloqueo sobre la MISMA `rel` no puedan producir
/// la misma secuencia de bytes.
const TAG_BLOCKER: u8 = b'B';
/// Etiqueta del cierre, delante del número de elementos.
///
/// Sin ella el final se distinguiría de un elemento más solo porque el primer
/// byte del prefijo de longitud del contador (`0x08`) no coincide con ninguna
/// de las otras dos etiquetas — cierto hoy y por accidente.
const TAG_END: u8 = b'E';
// Las tres etiquetas tienen que ser distintas o el flujo deja de ser
// descodificable de una sola manera, que es lo único que impide una colisión.
const _: () = assert!(TAG_END != TAG_STEP && TAG_END != TAG_BLOCKER);
const _: () = assert!(TAG_STEP != TAG_BLOCKER);

/// El acumulador del `plan_hash`, en STREAMING.
///
/// Se construye con la intención del plan, se le da cada elemento en el orden
/// en que el flujo lo produjo y se cierra con [`PlanHasher::finish`].
///
/// ```
/// use norte_proto::VPath;
/// use norte_proto::methods::SyncCompareOptions;
/// use norte_sync::{OnUnknown, PlanHasher, Side, SyncMode, SyncOptions};
///
/// let opts = SyncOptions {
///     source_root: VPath::parse("file:///origen").expect("path"),
///     dest_root: VPath::parse("file:///destino").expect("path"),
///     mode: SyncMode::Update,
///     on_unknown: OnUnknown::Copy,
///     source_side: Side::Left,
///     dest_has_trash: true,
///     dest_trash_restorable: true,
///     dest_writable: true,
/// };
/// let compare = SyncCompareOptions::default();
/// // Un plan VACÍO tiene hash: es el plan «no hay nada que hacer».
/// let vacio = PlanHasher::new(&opts, &compare).finish();
/// assert_eq!(vacio.as_str().len(), norte_proto::methods::PLAN_HASH_LEN);
///
/// // Y la misma intención con OTRO modo no lo comparte.
/// let otro = SyncOptions { mode: SyncMode::Mirror, ..opts };
/// assert_ne!(PlanHasher::new(&otro, &compare).finish(), vacio);
/// ```
#[derive(Debug, Clone)]
pub struct PlanHasher {
    /// El digest, ya sembrado con la intención.
    digest: Sha256,
    /// Cuántos elementos entraron. Se alimenta al CERRAR: un hasher en
    /// streaming no conoce el total al empezar, que es justo lo que lo hace
    /// O(1) en memoria.
    items: u64,
}

impl PlanHasher {
    /// Siembra el digest con la intención del plan: lo que decide QUÉ plan se
    /// pidió, antes de que llegue un solo elemento.
    ///
    /// Va todo, incluido lo que además se refleja en los pasos (la papelera del
    /// destino se ve en cada reversa, el lado del origen en cada `rel`): sembrar
    /// de más no puede crear una colisión, y sembrar de menos deja dos
    /// peticiones distintas compartiendo huella cuando el árbol calla —dos
    /// árboles vacíos producen cero pasos bajo CUALQUIER opción—.
    ///
    /// Las opciones de comparación van APARTE porque no viven en
    /// [`SyncOptions`]: el transductor no las necesita (no compara, transduce),
    /// y quien planifica —`sync.plan`— tiene las dos delante. Un plan hecho con
    /// `hash` encendido no es el mismo que uno hecho solo con tamaño, aunque los
    /// pasos salgan iguales: se aprobó otra cosa.
    #[must_use]
    pub fn new(opts: &SyncOptions, compare: &SyncCompareOptions) -> Self {
        // Se DESESTRUCTURAN a propósito: un campo nuevo en cualquiera de los
        // tres tipos rompe la compilación aquí en vez de quedarse fuera del
        // digest en silencio, que es la clase de omisión que nadie ve hasta que
        // dos planes distintos comparten hash.
        let SyncOptions {
            source_root,
            dest_root,
            mode,
            on_unknown,
            source_side,
            dest_has_trash,
            dest_trash_restorable,
            dest_writable,
        } = opts;
        let SyncCompareOptions {
            criteria,
            max_depth,
            mtime_tolerance_ms,
            follow_symlinks,
            descend_orphans,
        } = compare;
        let CompareCriteria { size, mtime, hash } = criteria;

        let mut digest = Sha256::new();
        // Separación de dominio: este digest no es el del lote de renames ni el
        // de la cadena del journal, y no debe poder confundirse con ninguno.
        feed(&mut digest, b"norte-sync-plan-v1");
        // Las raíces por PIEZAS —scheme, authority, segmentos en bytes— y no
        // por su forma wire: percent-decodificar y volver a codificar es
        // lossless, pero hacer que el digest dependa de que el códec siga siendo
        // canónico es una dependencia que este crate no necesita contraer
        // (regla dura 1: se comparan bytes).
        feed_root(&mut digest, source_root);
        feed_root(&mut digest, dest_root);
        feed_name(&mut digest, &mode_name(*mode));
        feed_name(&mut digest, &on_unknown_name(*on_unknown));
        feed_name(&mut digest, &side_name(*source_side));
        feed_flag(&mut digest, *dest_has_trash);
        feed_flag(&mut digest, *dest_trash_restorable);
        feed_flag(&mut digest, *dest_writable);
        feed_flag(&mut digest, *size);
        feed_flag(&mut digest, *mtime);
        feed_flag(&mut digest, *hash);
        feed_opt_u64(&mut digest, max_depth.map(u64::from));
        feed_u64(&mut digest, u64::from(*mtime_tolerance_ms));
        feed_flag(&mut digest, *follow_symlinks);
        feed_opt_name(&mut digest, descend_orphans.map(descend_side_name));
        Self { digest, items: 0 }
    }

    /// Traga un elemento del flujo, sea lo que sea. Es lo que consume quien
    /// acumula el plan: el flujo produce [`PlanItem`], no dos secuencias.
    pub fn item(&mut self, item: &PlanItem) {
        match item {
            // El testigo del destino se queda FUERA del digest, y a propósito:
            // es de dónde salió la conclusión, no la conclusión. Dos planes con
            // los mismos pasos sobre un destino cuya fecha se movió sin que
            // ningún veredicto cambiara son el mismo plan y merecen la misma
            // aprobación; lo que revalida el testigo lo revalida el ejecutor,
            // paso a paso, y no una huella del plan entero.
            PlanItem::Step { step, dest: _ } => self.step(step),
            PlanItem::Blocker(blocker) => self.blocker(blocker),
        }
    }

    /// Traga UN paso.
    ///
    /// `id` NO entra: es presentación, no conclusión. El panel se ancla a él y
    /// un filtro puede renumerarlo, y una aprobación que se invalidase por
    /// ordenar una lista no estaría protegiendo nada.
    pub fn step(&mut self, step: &SyncStep) {
        // Desestructurado por el mismo motivo que en `new`: un campo nuevo en
        // `SyncStep` tiene que romper la compilación, no salirse del digest.
        let SyncStep {
            id: _,
            kind,
            rel,
            dest_rel,
            size,
            criterion,
            confidence,
            reversal,
            reason,
        } = step;
        self.digest.update([TAG_STEP]);
        feed_name(&mut self.digest, &step_kind_name(*kind));
        feed_rel(&mut self.digest, rel);
        // `dest_rel` es lo único que distingue «sobrescribo el fichero que hay»
        // de «creo un segundo al lado» (issue #152), y el hash es el testigo con
        // el que se autoriza escribir: tiene que entrar, y con su byte de
        // presencia, porque un `rel` raíz también son cero bytes.
        feed_opt_rel(&mut self.digest, dest_rel.as_ref());
        feed_opt_u64(&mut self.digest, *size);
        feed_name(&mut self.digest, &criterion_name(*criterion));
        feed_name(&mut self.digest, &confidence_name(*confidence));
        feed_opt_name(&mut self.digest, reversal.map(reversal_name));
        feed_opt_name(&mut self.digest, reason.map(reason_name));
        self.items = self.items.saturating_add(1);
    }

    /// Traga UN bloqueo.
    ///
    /// Los bloqueos entran TODOS, y eso es lo que distingue este acumulador de
    /// la lista que viaja en
    /// [`SyncPlanDone::blockers`](norte_proto::methods::SyncPlanDone::blockers):
    /// aquella está recortada a
    /// [`SYNC_MAX_BLOCKERS_REPORTED`](norte_proto::methods::SYNC_MAX_BLOCKERS_REPORTED)
    /// y el número de bloqueos NO está acotado —el de
    /// [`SyncBlockerKind::TypeMismatchDir`] crece con el árbol—. Hashear la
    /// lista recortada haría que dos planes que difieren solo a partir del
    /// bloqueo 257 compartieran huella.
    ///
    /// Aprobar un plan bloqueado y aprobar uno limpio son actos distintos
    /// aunque los pasos coincidan, así que un bloqueo cambia el hash.
    pub fn blocker(&mut self, blocker: &SyncBlocker) {
        let SyncBlocker { rel, kind, side } = blocker;
        self.digest.update([TAG_BLOCKER]);
        feed_name(&mut self.digest, &blocker_kind_name(*kind));
        feed_rel(&mut self.digest, rel);
        feed_opt_name(&mut self.digest, side.map(side_name));
        self.items = self.items.saturating_add(1);
    }

    /// Cierra el plan y devuelve su [`PlanHash`]: sha256 en hex MINÚSCULA.
    ///
    /// El número de elementos se alimenta aquí, al final, precedido de su
    /// etiqueta de cierre, porque un hasher en streaming no lo sabe antes (el
    /// del lote de renames lo pone delante porque recibe un `slice`). No hace
    /// falta para que el digest sea inyectivo —cada elemento va etiquetado y con
    /// longitudes— pero ata también el TAMAÑO del plan, que es lo primero que
    /// lee quien aprueba.
    ///
    /// **Se llama cuando el flujo ha terminado en `None`, jamás sobre uno
    /// cortado.** Un plan cancelado a mitad produce un digest indistinguible del
    /// de un plan más corto que sí terminó, y ese digest no debe existir: quien
    /// planifica emite [`SyncPlanDone`](norte_proto::methods::SyncPlanDone) solo
    /// cuando el flujo se agotó sin error, y sin esa notificación no hay hash
    /// que nadie pueda aprobar.
    #[must_use]
    pub fn finish(self) -> PlanHash {
        let mut digest = self.digest;
        digest.update([TAG_END]);
        feed_u64(&mut digest, self.items);
        let bytes: [u8; 32] = digest.finalize().into();
        // El hex lo pone el TIPO (`PlanHash::from_digest`) y no un codificador de
        // este crate: una segunda copia es una segunda ocasión de escribir
        // mayúsculas, que es el detalle que hace que dos escrituras del mismo
        // hash comparen distinto. De paso desaparece el `expect` (regla dura 6).
        PlanHash::from_digest(&bytes)
    }
}

/// Alimenta un campo con su LONGITUD delante: `"ab" + "c"` y `"a" + "bc"` no
/// pueden producir el mismo digest.
///
/// # #174: copiado de `norte_core::hashing::feed`, a propósito
/// Byte a byte el mismo framing, y tiene que seguir siéndolo. No comparten
/// código porque `norte_core::hashing` es `pub(crate)` de un crate que
/// DEPENDE de este (`norte-core` → `norte-sync`, no al revés), así que
/// "extraer hacia arriba" no es un movimiento de código; y porque la copia de
/// `norte-core` es la cadena tamper-evident del journal (ADR 0023) y el ancla
/// de la auditoría (ADR 0025) — no se puede mover, ni relicenciar de
/// AGPL-3.0-only a MIT/Apache-2.0, sin invalidar todo `journal.db` ya
/// escrito. Esta copia SÍ es libre de mudarse a un sitio compartido; la otra
/// no. Ver #151 para la misma frontera de licencia sobre la clave de
/// plegado, que quiere resolver las dos con una sola ADR.
fn feed(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// Un texto, por sus bytes.
fn feed_str(digest: &mut Sha256, text: &str) {
    feed(digest, text.as_bytes());
}

/// El nombre serde de un token de un enum.
fn feed_name(digest: &mut Sha256, name: &str) {
    feed_str(digest, name);
}

/// Un booleano, como un byte con su longitud.
fn feed_flag(digest: &mut Sha256, flag: bool) {
    feed(digest, &[u8::from(flag)]);
}

/// Un entero, en little-endian.
fn feed_u64(digest: &mut Sha256, value: u64) {
    feed(digest, &value.to_le_bytes());
}

/// Un entero OPCIONAL, con byte de presencia.
fn feed_opt_u64(digest: &mut Sha256, value: Option<u64>) {
    match value {
        None => digest.update([0u8]),
        Some(value) => {
            digest.update([1u8]);
            feed_u64(digest, value);
        }
    }
}

/// Un nombre de token OPCIONAL, con byte de presencia.
fn feed_opt_name(digest: &mut Sha256, name: Option<Cow<'_, str>>) {
    match name {
        None => digest.update([0u8]),
        Some(name) => {
            digest.update([1u8]);
            feed_name(digest, &name);
        }
    }
}

/// Una raíz: scheme, authority (con su byte de presencia — `file://` no tiene y
/// `file://x/` sí) y luego sus segmentos, igual que una ruta relativa.
///
/// La authority va BYTE A BYTE, sin plegar: para `mem://` y para un id de
/// conexión de object storage es un testigo opaco, y plegarla juntaría dos
/// conexiones distintas. Es la misma comparación que hace `rel_under`, y tiene
/// que serlo: dos raíces que el transductor considera distintas no pueden
/// hashear igual.
fn feed_root(digest: &mut Sha256, root: &VPath) {
    feed_str(digest, root.scheme());
    feed_opt_name(digest, root.authority().map(Cow::Borrowed));
    let segments: Vec<&[u8]> = root.segments().collect();
    feed_u64(digest, segments.len() as u64);
    for segment in segments {
        feed(digest, segment);
    }
}

/// Una ruta relativa: cuántos segmentos, y luego cada uno por sus BYTES
/// (regla dura 1 — jamás la forma percent-encoded, jamás una cadena plegada).
///
/// El número de segmentos delante y la longitud de cada uno es lo que impide
/// que `a/bc` y `ab/c` colisionen.
fn feed_rel(digest: &mut Sha256, rel: &RelPath) {
    feed_u64(digest, rel.segments().len() as u64);
    for segment in rel.segments() {
        feed(digest, segment.as_bytes());
    }
}

/// Una ruta relativa OPCIONAL, con byte de presencia: la raíz son cero
/// segmentos, o sea cero bytes, así que sin él «ausente» y «presente y vacía»
/// serían el mismo digest — y un `Skip` puede llevar legítimamente un `rel`
/// raíz.
fn feed_opt_rel(digest: &mut Sha256, rel: Option<&RelPath>) {
    match rel {
        None => digest.update([0u8]),
        Some(rel) => {
            digest.update([1u8]);
            feed_rel(digest, rel);
        }
    }
}

/// El nombre de un token que este binario NO conoce: su nombre de `Debug`, con
/// un prefijo que ningún nombre serde puede tener (todos son `snake_case`).
///
/// Solo lo alcanzan los tokens de un `norte-proto` futuro que este crate no
/// haya aprendido, y sigue siendo un NOMBRE: dos variantes nuevas distintas no
/// colisionan entre ellas ni con ninguna conocida.
///
/// Es el punto donde la garantía de desestructurar los STRUCTS no alcanza: un
/// enum de otro crate es `#[non_exhaustive]`, así que el comodín es obligatorio
/// y una variante nueva pasa por aquí en vez de romper la compilación. Es
/// seguro —sigue siendo inyectivo— pero quien añada una variante a `norte-proto`
/// debería añadirle también su brazo aquí, para que el digest hable su nombre
/// de wire y no el de Rust.
fn unknown_name<T: fmt::Debug>(token: &T) -> Cow<'static, str> {
    Cow::Owned(format!("?{token:?}"))
}

/// El nombre serde de [`SyncStepKind`].
fn step_kind_name(kind: SyncStepKind) -> Cow<'static, str> {
    match kind {
        SyncStepKind::CreateDir => Cow::Borrowed("create_dir"),
        SyncStepKind::Copy => Cow::Borrowed("copy"),
        SyncStepKind::Overwrite => Cow::Borrowed("overwrite"),
        SyncStepKind::DeleteTree => Cow::Borrowed("delete_tree"),
        SyncStepKind::Skip => Cow::Borrowed("skip"),
        SyncStepKind::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`StepReversal`].
fn reversal_name(reversal: StepReversal) -> Cow<'static, str> {
    match reversal {
        StepReversal::Delete => Cow::Borrowed("delete"),
        StepReversal::RestoreTrash => Cow::Borrowed("restore_trash"),
        StepReversal::Irreversible => Cow::Borrowed("irreversible"),
        StepReversal::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`SyncReason`].
fn reason_name(reason: SyncReason) -> Cow<'static, str> {
    match reason {
        SyncReason::AmbiguousSource => Cow::Borrowed("ambiguous_source"),
        SyncReason::UnknownConfidence => Cow::Borrowed("unknown_confidence"),
        SyncReason::Unreadable => Cow::Borrowed("unreadable"),
        SyncReason::NoTrashOnTarget => Cow::Borrowed("no_trash_on_target"),
        SyncReason::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`CompareCriterion`].
fn criterion_name(criterion: CompareCriterion) -> Cow<'static, str> {
    match criterion {
        CompareCriterion::Presence => Cow::Borrowed("presence"),
        CompareCriterion::Kind => Cow::Borrowed("kind"),
        CompareCriterion::LinkTarget => Cow::Borrowed("link_target"),
        CompareCriterion::Size => Cow::Borrowed("size"),
        CompareCriterion::Mtime => Cow::Borrowed("mtime"),
        CompareCriterion::Hash => Cow::Borrowed("hash"),
        CompareCriterion::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`CompareConfidence`]. Ojo: `unknown` es un valor REAL
/// del vocabulario y `unrecognised` es el fallback de decode — dos hechos
/// distintos, y por eso dos nombres distintos.
fn confidence_name(confidence: CompareConfidence) -> Cow<'static, str> {
    match confidence {
        CompareConfidence::Certain => Cow::Borrowed("certain"),
        CompareConfidence::Probable => Cow::Borrowed("probable"),
        CompareConfidence::Unknown => Cow::Borrowed("unknown"),
        CompareConfidence::Unrecognised => Cow::Borrowed("unrecognised"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`SyncBlockerKind`].
fn blocker_kind_name(kind: SyncBlockerKind) -> Cow<'static, str> {
    match kind {
        SyncBlockerKind::AmbiguousDest => Cow::Borrowed("ambiguous_dest"),
        SyncBlockerKind::OverlapDetected => Cow::Borrowed("overlap_detected"),
        SyncBlockerKind::DestReadOnly => Cow::Borrowed("dest_read_only"),
        SyncBlockerKind::DirTooLarge => Cow::Borrowed("dir_too_large"),
        SyncBlockerKind::TypeMismatchDir => Cow::Borrowed("type_mismatch_dir"),
        SyncBlockerKind::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`Side`].
fn side_name(side: Side) -> Cow<'static, str> {
    match side {
        Side::Left => Cow::Borrowed("left"),
        Side::Right => Cow::Borrowed("right"),
        Side::Unknown => Cow::Borrowed("unknown"),
    }
}

/// El nombre serde de [`SyncMode`].
fn mode_name(mode: SyncMode) -> Cow<'static, str> {
    match mode {
        SyncMode::Update => Cow::Borrowed("update"),
        SyncMode::Mirror => Cow::Borrowed("mirror"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`OnUnknown`].
fn on_unknown_name(on_unknown: OnUnknown) -> Cow<'static, str> {
    match on_unknown {
        OnUnknown::Copy => Cow::Borrowed("copy"),
        OnUnknown::Skip => Cow::Borrowed("skip"),
        other => unknown_name(&other),
    }
}

/// El nombre serde de [`DescendSide`].
fn descend_side_name(side: DescendSide) -> Cow<'static, str> {
    match side {
        DescendSide::Left => Cow::Borrowed("left"),
        DescendSide::Right => Cow::Borrowed("right"),
        other => unknown_name(&other),
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::VPath;
    use norte_proto::methods::{CompareCriteria, PLAN_HASH_LEN};

    use super::*;

    fn vpath(wire: &str) -> VPath {
        VPath::parse(wire).expect("path")
    }

    fn opts_update() -> SyncOptions {
        SyncOptions {
            source_root: vpath("file:///origen"),
            dest_root: vpath("file:///destino"),
            mode: SyncMode::Update,
            on_unknown: OnUnknown::Copy,
            source_side: Side::Left,
            dest_has_trash: true,
            dest_trash_restorable: true,
            dest_writable: true,
        }
    }

    fn opts_mirror() -> SyncOptions {
        SyncOptions {
            mode: SyncMode::Mirror,
            ..opts_update()
        }
    }

    fn compare_opts() -> SyncCompareOptions {
        SyncCompareOptions::default()
    }

    fn hasher(opts: &SyncOptions) -> PlanHasher {
        PlanHasher::new(opts, &compare_opts())
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    /// Una `rel` desde los BYTES de sus segmentos: NFC y NFD son las dos UTF-8
    /// válido, así que la forma wire no las distingue a ojo.
    fn rel_of(segments: &[&[u8]]) -> RelPath {
        RelPath::new(
            segments
                .iter()
                .map(|b| norte_proto::Segment::new(b.to_vec()).expect("segment"))
                .collect(),
        )
    }

    fn copy_step(rel_wire: &str, size: u64) -> SyncStep {
        SyncStep {
            id: 1,
            kind: SyncStepKind::Copy,
            rel: rel(rel_wire),
            dest_rel: None,
            size: Some(size),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(StepReversal::Delete),
            reason: None,
        }
    }

    fn step_with_id(id: u64) -> SyncStep {
        SyncStep {
            id,
            ..copy_step("a.txt", 10)
        }
    }

    fn skip_step(rel_wire: &str) -> SyncStep {
        SyncStep {
            kind: SyncStepKind::Skip,
            rel: rel(rel_wire),
            size: None,
            reversal: None,
            reason: Some(SyncReason::Unreadable),
            ..copy_step(rel_wire, 0)
        }
    }

    fn blocker(kind: SyncBlockerKind, rel_wire: &str) -> SyncBlocker {
        SyncBlocker {
            rel: rel(rel_wire),
            kind,
            side: Some(Side::Right),
        }
    }

    #[test]
    fn the_hash_covers_the_conclusions_and_not_the_ids() {
        // `id` es presentación: dos planes que hacen lo mismo hashean igual
        // aunque un filtro haya renumerado el panel.
        let mut a = hasher(&opts_update());
        let mut b = hasher(&opts_update());
        a.step(&step_with_id(1));
        b.step(&step_with_id(99));
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn changing_a_step_changes_the_hash() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a.txt", 10));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("a.txt", 11));
        assert_ne!(a.finish(), b.finish(), "size is a conclusion");
    }

    #[test]
    fn changing_the_mode_changes_the_hash_with_identical_steps() {
        let mut a = hasher(&opts_update());
        let mut b = hasher(&opts_mirror());
        a.step(&copy_step("a.txt", 10));
        b.step(&copy_step("a.txt", 10));
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn order_is_part_of_the_plan() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a", 1));
        a.step(&copy_step("b", 2));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("b", 2));
        b.step(&copy_step("a", 1));
        assert_ne!(
            a.finish(),
            b.finish(),
            "CreateDir before Copy is a conclusion too"
        );
    }

    #[test]
    fn a_blocker_is_in_the_hash() {
        // Aprobar un plan bloqueado y aprobar uno limpio son actos distintos
        // aunque los pasos coincidan.
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a", 1));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("a", 1));
        b.blocker(&blocker(SyncBlockerKind::AmbiguousDest, "README"));
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn the_hash_is_lowercase_hex_of_the_documented_length() {
        let h = hasher(&opts_update()).finish();
        assert_eq!(h.as_str().len(), PLAN_HASH_LEN);
        assert!(
            h.as_str()
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }

    /// La etiqueta de clase: un `Skip` y un bloqueo en la MISMA ruta no son el
    /// mismo plan, aunque casi todo lo demás que llevan sea lo mismo.
    #[test]
    fn a_skip_and_a_blocker_at_the_same_rel_do_not_collide() {
        let mut a = hasher(&opts_update());
        a.step(&skip_step("sub/x"));
        let mut b = hasher(&opts_update());
        b.blocker(&blocker(SyncBlockerKind::AmbiguousDest, "sub/x"));
        assert_ne!(a.finish(), b.finish());
    }

    /// Sin prefijo de longitud, `a/bc` y `ab/c` son los mismos bytes pegados
    /// —y son dos ficheros DISTINTOS del árbol de alguien—.
    #[test]
    fn two_paths_that_concatenate_alike_do_not_collide() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a/bc", 1));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("ab/c", 1));
        assert_ne!(a.finish(), b.finish());
    }

    /// Lo mismo entre dos CAMPOS pegados: `rel` + `dest_rel` de un paso no se
    /// pueden leer como otro reparto de los mismos bytes.
    #[test]
    fn a_rel_and_a_dest_rel_cannot_be_read_as_one_another() {
        let mut a = hasher(&opts_update());
        let mut step = copy_step("ab", 1);
        step.dest_rel = Some(rel("c"));
        a.step(&step);
        let mut b = hasher(&opts_update());
        let mut other = copy_step("a", 1);
        other.dest_rel = Some(rel("bc"));
        b.step(&other);
        assert_ne!(a.finish(), b.finish());
    }

    /// El byte de presencia de `dest_rel`: un `Skip` puede llevar un `rel`
    /// RAÍZ, que codifica a cero bytes, así que «ausente» y «presente y vacía»
    /// tienen que separarse aunque un paso bien formado no produzca la
    /// ambigüedad.
    #[test]
    fn an_absent_dest_rel_and_an_empty_one_are_not_the_same_plan() {
        let mut a = hasher(&opts_update());
        a.step(&skip_step("sub/x"));
        let mut b = hasher(&opts_update());
        let mut step = skip_step("sub/x");
        step.dest_rel = Some(RelPath::default());
        b.step(&step);
        assert_ne!(a.finish(), b.finish());
    }

    /// `dest_rel` decide sobre qué fichero se escribe (#152), así que dos
    /// planes que solo difieren en él NO se pueden aprobar con el mismo
    /// testigo.
    #[test]
    fn the_destination_spelling_is_part_of_the_hash() {
        let mut a = hasher(&opts_update());
        let mut nfc = copy_step("x", 1);
        // `café` en NFC…
        nfc.dest_rel = Some(rel_of(&["caf\u{e9}".as_bytes()]));
        a.step(&nfc);
        let mut b = hasher(&opts_update());
        let mut nfd = copy_step("x", 1);
        // …y en NFD: los mismos caracteres, otros BYTES, otro fichero en ext4.
        nfd.dest_rel = Some(rel_of(&["cafe\u{301}".as_bytes()]));
        b.step(&nfd);
        assert_ne!(a.finish(), b.finish());
    }

    /// Un nombre que no es UTF-8 entra por sus bytes y distingue.
    #[test]
    fn a_non_utf8_name_is_hashed_by_its_bytes() {
        let mut a = hasher(&opts_update());
        let mut one = copy_step("x", 1);
        one.rel = rel_of(&[b"informe\xff\xfe.dat"]);
        a.step(&one);
        let mut b = hasher(&opts_update());
        let mut other = copy_step("x", 1);
        other.rel = rel_of(&[b"informe\xfe\xff.dat"]);
        b.step(&other);
        assert_ne!(a.finish(), b.finish());
    }

    /// Cada campo del paso es una conclusión y ninguno se queda fuera.
    #[test]
    fn every_field_of_a_step_moves_the_hash() {
        let base = copy_step("a.txt", 10);
        let baseline = {
            let mut h = hasher(&opts_update());
            h.step(&base);
            h.finish()
        };
        let variants = [
            SyncStep {
                kind: SyncStepKind::Overwrite,
                reversal: Some(StepReversal::RestoreTrash),
                ..base.clone()
            },
            SyncStep {
                rel: rel("b.txt"),
                ..base.clone()
            },
            SyncStep {
                dest_rel: Some(rel("A.TXT")),
                ..base.clone()
            },
            SyncStep {
                size: None,
                ..base.clone()
            },
            SyncStep {
                criterion: CompareCriterion::Mtime,
                ..base.clone()
            },
            SyncStep {
                confidence: CompareConfidence::Unknown,
                ..base.clone()
            },
            SyncStep {
                reversal: Some(StepReversal::Irreversible),
                reason: Some(SyncReason::NoTrashOnTarget),
                ..base.clone()
            },
            SyncStep {
                kind: SyncStepKind::Skip,
                reversal: None,
                reason: Some(SyncReason::AmbiguousSource),
                ..base.clone()
            },
        ];
        for variant in variants {
            let mut h = hasher(&opts_update());
            h.step(&variant);
            assert_ne!(h.finish(), baseline, "no entró en el digest: {variant:?}");
        }
    }

    /// Y cada campo del bloqueo.
    #[test]
    fn every_field_of_a_blocker_moves_the_hash() {
        let base = blocker(SyncBlockerKind::AmbiguousDest, "sub/x");
        let baseline = {
            let mut h = hasher(&opts_update());
            h.blocker(&base);
            h.finish()
        };
        let variants = [
            SyncBlocker {
                kind: SyncBlockerKind::DirTooLarge,
                ..base.clone()
            },
            SyncBlocker {
                rel: rel("sub/y"),
                ..base.clone()
            },
            SyncBlocker {
                side: None,
                ..base.clone()
            },
            SyncBlocker {
                side: Some(Side::Left),
                ..base.clone()
            },
        ];
        for variant in variants {
            let mut h = hasher(&opts_update());
            h.blocker(&variant);
            assert_ne!(h.finish(), baseline, "no entró en el digest: {variant:?}");
        }
    }

    /// La INTENCIÓN se siembra entera: dos peticiones distintas no comparten
    /// huella ni cuando el árbol no produce un solo paso.
    #[test]
    fn every_part_of_the_intention_seeds_the_hash() {
        let base = opts_update();
        let baseline = hasher(&base).finish();
        let variants = [
            SyncOptions {
                source_root: vpath("file:///otro"),
                ..base.clone()
            },
            SyncOptions {
                dest_root: vpath("file:///otro"),
                ..base.clone()
            },
            SyncOptions {
                mode: SyncMode::Mirror,
                ..base.clone()
            },
            SyncOptions {
                on_unknown: OnUnknown::Skip,
                ..base.clone()
            },
            SyncOptions {
                source_side: Side::Right,
                ..base.clone()
            },
            SyncOptions {
                dest_has_trash: false,
                ..base.clone()
            },
            SyncOptions {
                dest_trash_restorable: false,
                ..base.clone()
            },
            SyncOptions {
                dest_writable: false,
                ..base.clone()
            },
        ];
        for variant in variants {
            assert_ne!(
                hasher(&variant).finish(),
                baseline,
                "no sembró el digest: {variant:?}"
            );
        }
    }

    /// Y las opciones de la comparación que hay debajo: un plan hecho leyendo
    /// 40 GB de contenido no es el mismo que uno hecho mirando tamaños, aunque
    /// los pasos salgan iguales.
    #[test]
    fn the_compare_options_seed_the_hash() {
        let opts = opts_update();
        let baseline = PlanHasher::new(&opts, &SyncCompareOptions::default()).finish();
        let variants = [
            SyncCompareOptions {
                criteria: CompareCriteria {
                    hash: true,
                    ..CompareCriteria::default()
                },
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                criteria: CompareCriteria {
                    size: false,
                    ..CompareCriteria::default()
                },
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                criteria: CompareCriteria {
                    mtime: false,
                    ..CompareCriteria::default()
                },
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                max_depth: Some(1),
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                mtime_tolerance_ms: 0,
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                follow_symlinks: true,
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                descend_orphans: Some(DescendSide::Left),
                ..SyncCompareOptions::default()
            },
        ];
        for variant in variants {
            assert_ne!(
                PlanHasher::new(&opts, &variant).finish(),
                baseline,
                "no sembró el digest: {variant:?}"
            );
        }
        // Y los dos lados de `descend_orphans` no son el mismo plan.
        assert_ne!(
            PlanHasher::new(
                &opts,
                &SyncCompareOptions {
                    descend_orphans: Some(DescendSide::Left),
                    ..SyncCompareOptions::default()
                }
            )
            .finish(),
            PlanHasher::new(
                &opts,
                &SyncCompareOptions {
                    descend_orphans: Some(DescendSide::Right),
                    ..SyncCompareOptions::default()
                }
            )
            .finish(),
        );
    }

    /// `max_depth` ausente no es `max_depth: 0` (que es «solo la raíz»).
    #[test]
    fn an_absent_max_depth_is_not_a_zero_one() {
        let opts = opts_update();
        assert_ne!(
            PlanHasher::new(&opts, &SyncCompareOptions::default()).finish(),
            PlanHasher::new(
                &opts,
                &SyncCompareOptions {
                    max_depth: Some(0),
                    ..SyncCompareOptions::default()
                }
            )
            .finish(),
        );
    }

    /// `item` es lo que consume quien acumula el flujo, y tiene que dar
    /// exactamente lo mismo que llamar a mano.
    /// El testigo del destino NO entra en el digest, y hay que fijarlo: si
    /// alguien lo alimenta algún día, el writer hashearía una cosa y
    /// `Spool::open` —que rehace el digest desde los PASOS y no ve el testigo—
    /// otra, y todo plan con una sobrescritura fallaría su propia verificación y
    /// saldría como `PlanStale`. En silencio, y solo en producción.
    #[test]
    fn the_destination_witness_is_not_part_of_the_digest() {
        use norte_proto::EntryKind;

        use crate::DestWitness;

        let step = copy_step("a", 1);
        let mut sin = hasher(&opts_update());
        sin.item(&PlanItem::Step {
            step: step.clone(),
            dest: None,
        });
        let mut con = hasher(&opts_update());
        con.item(&PlanItem::Step {
            step: step.clone(),
            dest: Some(DestWitness {
                kind: EntryKind::File,
                size: Some(99),
                mtime_ms: Some(7),
            }),
        });
        let mut otro = hasher(&opts_update());
        otro.item(&PlanItem::Step {
            step,
            dest: Some(DestWitness {
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            }),
        });
        let (sin, con, otro) = (sin.finish(), con.finish(), otro.finish());
        assert_eq!(sin, con, "poner un testigo no cambia el plan");
        assert_eq!(con, otro, "ni cambiarlo por otro");
    }

    #[test]
    fn feeding_items_and_feeding_halves_agree() {
        let step = copy_step("a", 1);
        let block = blocker(SyncBlockerKind::OverlapDetected, "sub");
        let mut a = hasher(&opts_update());
        a.item(&PlanItem::Step {
            step: step.clone(),
            dest: None,
        });
        a.item(&PlanItem::Blocker(block.clone()));
        let mut b = hasher(&opts_update());
        b.step(&step);
        b.blocker(&block);
        assert_eq!(a.finish(), b.finish());
    }

    /// Un plan con más elementos no puede hashear como uno con menos, ni
    /// siquiera cuando el primero es prefijo del segundo.
    #[test]
    fn a_prefix_of_a_plan_is_not_that_plan() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a", 1));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("a", 1));
        b.step(&copy_step("b", 1));
        assert_ne!(a.finish(), b.finish());
    }

    /// **La consecuencia que Task 8 y Task 10 tienen que conocer.** Un destino
    /// de solo lectura produce UN elemento sea cual sea el árbol, así que todos
    /// esos planes hashean igual: el hash dice «este es el plan que se te
    /// enseñó», y quien ejecuta decide por `executable`.
    #[test]
    fn every_read_only_plan_hashes_alike_so_apply_must_gate_on_executable() {
        let opts = SyncOptions {
            dest_writable: false,
            ..opts_update()
        };
        let read_only = SyncBlocker {
            rel: RelPath::default(),
            kind: SyncBlockerKind::DestReadOnly,
            side: Some(Side::Right),
        };
        let mut a = hasher(&opts);
        a.blocker(&read_only);
        let mut b = hasher(&opts);
        b.blocker(&read_only);
        assert_eq!(
            a.finish(),
            b.finish(),
            "dos árboles distintos, el mismo único elemento"
        );
    }

    /// El framing con longitud, comprobado sobre el mecanismo y no solo sobre
    /// sus usuarios: sin él, dos repartos de los mismos bytes colisionan.
    #[test]
    fn the_length_prefix_is_what_separates_two_adjacent_fields() {
        let mut ab_c = Sha256::new();
        feed(&mut ab_c, b"ab");
        feed(&mut ab_c, b"c");
        let mut a_bc = Sha256::new();
        feed(&mut a_bc, b"a");
        feed(&mut a_bc, b"bc");
        let ab_c: [u8; 32] = ab_c.finalize().into();
        let a_bc: [u8; 32] = a_bc.finalize().into();
        assert_ne!(
            PlanHash::from_digest(&ab_c),
            PlanHash::from_digest(&a_bc),
            "sin prefijo de longitud, estos dos son el mismo digest"
        );
    }

    /// **VECTOR CONGELADO del framing.** Los otros veintitantos tests son
    /// RELATIVOS (`assert_ne!` entre dos digests), así que pasarían igual si el
    /// prefijo de longitud cambiase de `u64` a `u32`, si el byte de presencia
    /// intercambiase 0 y 1, o si dos campos cambiasen de orden — y cualquiera de
    /// esas tres invalida en silencio todos los planes en vuelo.
    ///
    /// A diferencia del vector de `norte_core::hashing`, este SÍ se puede
    /// actualizar: no hay nada en disco que dependa de él (el spool vive
    /// `SYNC_PLAN_TTL_MS` y lo escribe y lo lee el mismo binario). Lo que no se
    /// puede es actualizarlo para que un diff que no sabes explicar se ponga
    /// verde. Si has añadido un campo al digest a propósito, cambia la
    /// constante y dilo en el commit; si no, has roto el framing.
    #[test]
    fn the_framing_is_frozen() {
        let mut h = hasher(&opts_update());
        h.step(&copy_step("a.txt", 10));
        h.blocker(&blocker(SyncBlockerKind::AmbiguousDest, "sub/x"));
        assert_eq!(
            h.finish().as_str(),
            // Cambió al sembrar `dest_trash_restorable` (tarea 11b del plan de
            // sincronización): un campo NUEVO en la intención, a propósito.
            "78f235a59de1d47760539a7b4f79bb35c3cda5e88e6ff230cc89afda9f52e289",
        );
    }

    /// Un token que este binario no conoce sigue siendo un NOMBRE, y dos
    /// desconocidos distintos no colapsan en uno.
    #[test]
    fn an_unknown_token_hashes_as_a_name_and_not_as_a_hole() {
        assert_eq!(step_kind_name(SyncStepKind::Copy), "copy");
        assert_eq!(unknown_name(&SyncStepKind::CreateDir), "?CreateDir");
        assert_ne!(
            unknown_name(&SyncStepKind::CreateDir),
            unknown_name(&SyncStepKind::Copy)
        );
        // Y jamás se confunde con un nombre serde, que es siempre snake_case.
        assert!(unknown_name(&SyncStepKind::Copy).starts_with('?'));
    }
}
