//! Las propiedades del transductor y del `plan_hash`, sobre árboles y planes
//! que nadie escribió a mano.
//!
//! Las pruebas de mesa de `plan.rs` y `hash.rs` fijan CASOS; esto fija lo que
//! tiene que valer para CUALQUIER flujo: un árbol idéntico no planifica nada,
//! `Update` no borra jamás, ningún `rel` sale de un sitio que las filas no
//! nombraron, lo irreversible es exactamente lo destructivo sin papelera, el
//! blanco de un paso es el fichero que EXISTE en el destino, y dos planes
//! distintos no comparten huella.
//!
//! Los nombres salen del corpus feo y, sobre todo, en PAREJAS que se deletrean
//! distinto —`café` NFC contra `café` NFD, `README` contra `readme`—: es ahí
//! donde el emparejamiento de la comparación pliega y donde vive el issue #152,
//! así que un corpus que solo varíe los nombres ENTRE filas no toca la parte
//! peligrosa.

use std::collections::BTreeSet;

use futures::StreamExt;
use futures::executor::block_on;
use norte_proto::methods::{
    CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict,
    SyncCompareOptions,
};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_sync::{
    OnUnknown, PlanHasher, PlanItem, RelPath, Side, StepReversal, SyncBlocker, SyncBlockerKind,
    SyncMode, SyncOptions, SyncReason, SyncStep, SyncStepKind, plan,
};
use proptest::prelude::*;
use tokio_util::sync::CancellationToken;

fn vpath(wire: &str) -> VPath {
    VPath::parse(wire).expect("path")
}

fn source_root() -> VPath {
    vpath("file:///origen")
}

fn dest_root() -> VPath {
    vpath("file:///destino")
}

/// El cableado de un plan, que es lo que las filas NO traen.
#[derive(Debug, Clone, Copy)]
struct Wiring {
    mode: SyncMode,
    on_unknown: OnUnknown,
    source_side: Side,
    trash: bool,
    /// ¿Y esa papelera nombra lo que entierra? Una que no lo hace vuelve
    /// IRREVERSIBLE todo el plan, no solo lo destructivo.
    trash_restorable: bool,
    writable: bool,
}

impl Wiring {
    /// ¿Es el lado DERECHO de las filas el origen? Decide dónde cuelga cada
    /// entrada que se genere.
    fn source_right(self) -> bool {
        self.source_side == Side::Right
    }
}

fn opts_of(w: Wiring) -> SyncOptions {
    SyncOptions {
        source_root: source_root(),
        dest_root: dest_root(),
        mode: w.mode,
        on_unknown: w.on_unknown,
        source_side: w.source_side,
        dest_has_trash: w.trash,
        dest_trash_restorable: w.trash_restorable,
        dest_writable: w.writable,
    }
}

fn opts_update() -> SyncOptions {
    opts_of(Wiring {
        mode: SyncMode::Update,
        on_unknown: OnUnknown::Copy,
        source_side: Side::Left,
        trash: true,
        trash_restorable: true,
        writable: true,
    })
}

fn opts_mirror() -> SyncOptions {
    SyncOptions {
        mode: SyncMode::Mirror,
        ..opts_update()
    }
}

/// Cableados COMPLETOS: los dos modos, las dos políticas de confianza, los dos
/// lados de origen, con y sin papelera, con y sin escritura.
fn wiring() -> impl Strategy<Value = Wiring> {
    (
        prop_oneof![Just(SyncMode::Update), Just(SyncMode::Mirror)],
        prop_oneof![Just(OnUnknown::Copy), Just(OnUnknown::Skip)],
        prop_oneof![Just(Side::Left), Just(Side::Right)],
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(mode, on_unknown, source_side, trash, trash_restorable, writable)| Wiring {
                mode,
                on_unknown,
                source_side,
                trash,
                trash_restorable,
                writable,
            },
        )
}

/// Una ruta bajo `root`, segmento a segmento y por sus BYTES.
fn under(root: &VPath, segments: &[Vec<u8>]) -> VPath {
    segments.iter().fold(root.clone(), |acc, segment| {
        acc.join(Segment::new(segment.clone()).expect("segment"))
    })
}

fn entry(root: &VPath, segments: &[Vec<u8>], kind: EntryKind, size: Option<u64>) -> Entry {
    Entry {
        path: under(root, segments),
        kind,
        size,
        mtime_ms: None,
        attrs: std::collections::BTreeMap::default(),
    }
}

/// Cómo deletrea el DESTINO un nombre del origen cuando la clave de
/// emparejamiento los pliega a uno: NFC contra NFD, y las dos cajas de un mismo
/// nombre. Lo demás se deletrea igual en los dos lados.
fn twin_of(name: &[u8]) -> Vec<u8> {
    match name {
        b"README" => b"readme".to_vec(),
        n if n == "caf\u{e9}".as_bytes() => "cafe\u{301}".as_bytes().to_vec(),
        other => other.to_vec(),
    }
}

fn twin_path(segments: &[Vec<u8>]) -> Vec<Vec<u8>> {
    segments.iter().map(|s| twin_of(s)).collect()
}

/// Un nombre del corpus feo: los dos que tienen gemelo en el destino, un nombre
/// que no es UTF-8 y dos corrientes.
fn name() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(b"a.txt".to_vec()),
        Just(b"sub".to_vec()),
        Just(b"README".to_vec()),
        Just("caf\u{e9}".as_bytes().to_vec()),
        Just(b"informe\xff\xfe.dat".to_vec()),
    ]
}

/// De una a TRES componentes: siempre al menos una, porque una ruta que fuera la
/// raíz misma es otro contrato (`SyncError::RootIsNotAStep`) y lo fijan las
/// pruebas de mesa. Tres es lo que hace falta para que la traducción por
/// ancestro tenga dos niveles que recorrer.
fn segments() -> impl Strategy<Value = Vec<Vec<u8>>> {
    prop::collection::vec(name(), 1..4)
}

fn kind() -> impl Strategy<Value = EntryKind> {
    prop_oneof![
        Just(EntryKind::File),
        Just(EntryKind::Dir),
        Just(EntryKind::Symlink),
    ]
}

fn criterion() -> impl Strategy<Value = CompareCriterion> {
    prop_oneof![
        Just(CompareCriterion::Presence),
        Just(CompareCriterion::Kind),
        Just(CompareCriterion::Size),
        Just(CompareCriterion::Mtime),
        Just(CompareCriterion::Hash),
    ]
}

fn confidence() -> impl Strategy<Value = CompareConfidence> {
    prop_oneof![
        Just(CompareConfidence::Certain),
        Just(CompareConfidence::Probable),
        Just(CompareConfidence::Unknown),
    ]
}

/// Todos los veredictos, el que un daemon N+1 podría mandar incluido.
fn verdict() -> impl Strategy<Value = CompareVerdict> {
    prop_oneof![
        Just(CompareVerdict::Same),
        Just(CompareVerdict::Different),
        Just(CompareVerdict::OnlyLeft),
        Just(CompareVerdict::OnlyRight),
        Just(CompareVerdict::TypeMismatch),
        Just(CompareVerdict::Ambiguous),
        Just(CompareVerdict::Error),
        Just(CompareVerdict::Unknown),
    ]
}

fn reason() -> impl Strategy<Value = Option<CompareReason>> {
    prop_oneof![
        Just(None),
        Just(Some(CompareReason::CaseFold)),
        Just(Some(CompareReason::Normalization)),
        Just(Some(CompareReason::Unreadable)),
        Just(Some(CompareReason::ReadFailed)),
        Just(Some(CompareReason::DirTooLarge)),
    ]
}

fn side() -> impl Strategy<Value = Option<Side>> {
    prop_oneof![Just(None), Just(Some(Side::Left)), Just(Some(Side::Right))]
}

/// Una fila cualquiera, con la entrada del ORIGEN colgando de `source_root` y la
/// del DESTINO de `dest_root` —el lado en el que caen lo decide el cableado— y
/// con la ortografía del destino plegada cuando el nombre tiene gemelo.
fn any_row(source_right: bool) -> impl Strategy<Value = CompareRow> {
    (
        segments(),
        any::<bool>(),
        verdict(),
        criterion(),
        confidence(),
        kind(),
        kind(),
        prop::option::of(0u64..1_000_000),
        reason(),
        side(),
    )
        .prop_map(
            move |(
                segs,
                twinned,
                verdict,
                criterion,
                confidence,
                source_kind,
                dest_kind,
                size,
                reason,
                side,
            )| {
                let dest_segs = if twinned {
                    twin_path(&segs)
                } else {
                    segs.clone()
                };
                let source = entry(&source_root(), &segs, source_kind, size);
                let dest = entry(&dest_root(), &dest_segs, dest_kind, size);
                let (left, right) = if source_right {
                    (dest, source)
                } else {
                    (source, dest)
                };
                let (left, right) = match verdict {
                    CompareVerdict::OnlyLeft => (Some(left), None),
                    CompareVerdict::OnlyRight => (None, Some(right)),
                    _ => (Some(left), Some(right)),
                };
                CompareRow {
                    id: 0,
                    left,
                    right,
                    verdict,
                    criterion,
                    confidence,
                    newer: None,
                    reason,
                    side,
                }
            },
        )
}

/// Un cableado y un flujo de filas coherente con él.
fn scenario() -> impl Strategy<Value = (Wiring, Vec<CompareRow>)> {
    wiring().prop_flat_map(|w| {
        (
            Just(w),
            prop::collection::vec(any_row(w.source_right()), 0..8),
        )
    })
}

/// Filas que dicen todas «iguales, y con certeza»: el árbol comparado consigo
/// mismo, con la misma ortografía en los dos lados.
fn same_rows_strategy(source_right: bool) -> impl Strategy<Value = Vec<CompareRow>> {
    let row = (segments(), kind(), criterion(), 0u64..1_000_000).prop_map(
        move |(segs, kind, criterion, size)| {
            let source = entry(&source_root(), &segs, kind, Some(size));
            let dest = entry(&dest_root(), &segs, kind, Some(size));
            let (left, right) = if source_right {
                (dest, source)
            } else {
                (source, dest)
            };
            CompareRow {
                id: 0,
                left: Some(left),
                right: Some(right),
                verdict: CompareVerdict::Same,
                criterion,
                confidence: CompareConfidence::Certain,
                newer: None,
                reason: None,
                side: None,
            }
        },
    );
    prop::collection::vec(row, 0..8)
}

/// Un cableado y un árbol idéntico a sí mismo, coherentes entre ellos.
fn same_scenario() -> impl Strategy<Value = (Wiring, Vec<CompareRow>)> {
    wiring().prop_flat_map(|w| (Just(w), same_rows_strategy(w.source_right())))
}

/// Filas EMPAREJADAS —las dos entradas siempre— con la ortografía del destino
/// plegada. Son las que ejercitan `dest_rel`: sin pareja no hay segunda
/// ortografía que leer.
fn paired_rows_strategy() -> impl Strategy<Value = Vec<CompareRow>> {
    let row = (
        segments(),
        any::<bool>(),
        prop_oneof![
            Just(CompareVerdict::Same),
            Just(CompareVerdict::Different),
            Just(CompareVerdict::TypeMismatch),
            Just(CompareVerdict::Error),
        ],
        criterion(),
        confidence(),
        kind(),
        prop::option::of(0u64..1_000_000),
    )
        .prop_map(
            |(segs, twinned, verdict, criterion, confidence, kind, size)| {
                let dest_segs = if twinned {
                    twin_path(&segs)
                } else {
                    segs.clone()
                };
                CompareRow {
                    id: 0,
                    left: Some(entry(&source_root(), &segs, kind, size)),
                    right: Some(entry(&dest_root(), &dest_segs, kind, size)),
                    verdict,
                    criterion,
                    confidence,
                    newer: None,
                    reason: (verdict == CompareVerdict::Error).then_some(CompareReason::Unreadable),
                    side: None,
                }
            },
        );
    prop::collection::vec(row, 0..8)
}

/// Una carpeta EMPAREJADA que los dos lados deletrean distinto, y dentro un
/// fichero que solo está en el origen: el corazón del issue #152, en el orden en
/// que el walk lo produce (el padre antes que el hijo).
fn folder_then_child() -> impl Strategy<Value = (Vec<u8>, Vec<u8>, Vec<CompareRow>)> {
    (name(), name()).prop_map(|(folder, leaf)| {
        let dest_folder = twin_of(&folder);
        let pair = CompareRow {
            id: 0,
            left: Some(entry(
                &source_root(),
                std::slice::from_ref(&folder),
                EntryKind::Dir,
                None,
            )),
            right: Some(entry(
                &dest_root(),
                std::slice::from_ref(&dest_folder),
                EntryKind::Dir,
                None,
            )),
            verdict: CompareVerdict::Same,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
        };
        let child = CompareRow {
            id: 1,
            left: Some(entry(
                &source_root(),
                &[folder.clone(), leaf.clone()],
                EntryKind::File,
                Some(10),
            )),
            right: None,
            verdict: CompareVerdict::OnlyLeft,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
        };
        (folder, leaf, vec![pair, child])
    })
}

fn run(rows: Vec<CompareRow>, opts: SyncOptions) -> Vec<PlanItem> {
    block_on(
        plan(
            futures::stream::iter(rows.into_iter().map(Ok)),
            opts,
            CancellationToken::new(),
        )
        .collect::<Vec<_>>(),
    )
    .into_iter()
    .map(|item| item.expect("las filas cuelgan de sus raíces: no hay error que dar"))
    .collect()
}

fn steps(items: &[PlanItem]) -> Vec<SyncStep> {
    items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Step { step, .. } => Some(step.clone()),
            PlanItem::Blocker(_) => None,
        })
        .collect()
}

fn blockers(items: &[PlanItem]) -> Vec<SyncBlocker> {
    items
        .iter()
        .filter_map(|item| match item {
            PlanItem::Blocker(blocker) => Some(blocker.clone()),
            PlanItem::Step { .. } => None,
        })
        .collect()
}

/// La ruta que sale de pegarle `rel` a `root`, en su forma wire.
fn joined(root: &VPath, rel: &RelPath) -> String {
    let segments: Vec<Vec<u8>> = rel
        .segments()
        .iter()
        .map(|segment| segment.as_bytes().to_vec())
        .collect();
    under(root, &segments).to_wire()
}

/// Todas las rutas que las filas de entrada NOMBRARON, en forma wire.
fn reported(rows: &[CompareRow]) -> BTreeSet<String> {
    rows.iter()
        .flat_map(|row| row.left.iter().chain(row.right.iter()))
        .map(|entry| entry.path.to_wire())
        .collect()
}

/// El blanco de un paso en el destino: `dest_root + dest_rel.unwrap_or(rel)`, o
/// sea lo que el ejecutor va a abrir.
fn target(step: &SyncStep) -> String {
    joined(&dest_root(), step.dest_rel.as_ref().unwrap_or(&step.rel))
}

// ---------- el `plan_hash` ----------

fn rel(wire: &str) -> RelPath {
    RelPath::parse_wire(wire).expect("rel")
}

fn hash_of(items: &[PlanItem]) -> norte_proto::methods::PlanHash {
    let mut hasher = PlanHasher::new(&opts_update(), &SyncCompareOptions::default());
    for item in items {
        hasher.item(item);
    }
    hasher.finish()
}

/// Los mismos elementos con el `id` a cero: lo que el hash SÍ debe distinguir.
fn without_ids(items: &[PlanItem]) -> Vec<PlanItem> {
    items
        .iter()
        .map(|item| match item {
            PlanItem::Step { step, dest } => PlanItem::Step {
                step: SyncStep {
                    id: 0,
                    ..step.clone()
                },
                dest: *dest,
            },
            PlanItem::Blocker(blocker) => PlanItem::Blocker(blocker.clone()),
        })
        .collect()
}

/// Elementos elegidos para que colisionen si el framing es flojo: los mismos
/// bytes repartidos de otra forma entre `rel` y `dest_rel`, una `rel` raíz, un
/// `Skip` y un bloqueo en el mismo sitio, y prefijos unos de otros.
fn candidate_item() -> impl Strategy<Value = PlanItem> {
    let step = |kind, rel_wire: &'static str, dest: Option<&'static str>, size| {
        let (reversal, reason) = match kind {
            SyncStepKind::Skip => (None, Some(SyncReason::Unreadable)),
            _ => (Some(StepReversal::Delete), None),
        };
        move |id: u64| PlanItem::Step {
            step: SyncStep {
                id,
                kind,
                rel: rel(rel_wire),
                dest_rel: dest.map(rel),
                size,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal,
                reason,
            },
            dest: None,
        }
    };
    let shapes: Vec<Box<dyn Fn(u64) -> PlanItem>> = vec![
        Box::new(step(SyncStepKind::Copy, "ab", None, Some(1))),
        Box::new(step(SyncStepKind::Copy, "ab", Some("c"), Some(1))),
        Box::new(step(SyncStepKind::Copy, "a", Some("bc"), Some(1))),
        Box::new(step(SyncStepKind::Copy, "a/bc", None, Some(1))),
        Box::new(step(SyncStepKind::Copy, "ab/c", None, Some(1))),
        Box::new(step(SyncStepKind::Copy, "ab", None, None)),
        Box::new(step(SyncStepKind::Overwrite, "ab", None, Some(1))),
        Box::new(step(SyncStepKind::Skip, "sub/x", None, None)),
        Box::new(|id| PlanItem::Step {
            step: SyncStep {
                id,
                kind: SyncStepKind::Skip,
                rel: RelPath::default(),
                dest_rel: Some(RelPath::default()),
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal: None,
                reason: Some(SyncReason::Unreadable),
            },
            dest: None,
        }),
        Box::new(|id| PlanItem::Step {
            step: SyncStep {
                id,
                kind: SyncStepKind::Skip,
                rel: RelPath::default(),
                dest_rel: None,
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal: None,
                reason: Some(SyncReason::Unreadable),
            },
            dest: None,
        }),
        Box::new(|_| {
            PlanItem::Blocker(SyncBlocker {
                rel: rel("sub/x"),
                kind: SyncBlockerKind::AmbiguousDest,
                side: Some(Side::Right),
            })
        }),
        Box::new(|_| {
            PlanItem::Blocker(SyncBlocker {
                rel: rel("sub/x"),
                kind: SyncBlockerKind::TypeMismatchDir,
                side: Some(Side::Left),
            })
        }),
    ];
    let count = shapes.len();
    (0..count, 0u64..4).prop_map(move |(which, id)| shapes[which](id))
}

fn candidate_plan() -> impl Strategy<Value = Vec<PlanItem>> {
    prop::collection::vec(candidate_item(), 0..5)
}

proptest! {
    /// El plan de A contra A está vacío, sea cual sea el cableado. Es la
    /// propiedad que hace que sincronizar un árbol de un millón de ficheros que
    /// ya está sincronizado cueste cero pasos y no un millón de `Skip`.
    #[test]
    fn an_identical_tree_plans_nothing((w, rows) in same_scenario()) {
        // Un destino de solo lectura sí produce algo —su bloqueo—, y eso lo fija
        // otra prueba: aquí lo que se mira es el árbol.
        let opts = SyncOptions { dest_writable: true, ..opts_of(w) };
        prop_assert!(run(rows, opts).is_empty());
    }

    /// `Update` no borra. Es la propiedad por la que el modo existe.
    #[test]
    fn update_never_deletes((w, rows) in scenario()) {
        let opts = SyncOptions { mode: SyncMode::Update, ..opts_of(w) };
        for step in steps(&run(rows, opts)) {
            prop_assert_ne!(step.kind, SyncStepKind::DeleteTree);
        }
    }

    /// Ningún `rel` se escapa de las raíces, y la versión FUERTE de eso: el
    /// plan no INVENTA rutas. Pegado a una de las dos raíces, todo `rel` de un
    /// paso nombra algo que las filas de entrada trajeron —el tipo ya impide el
    /// `..`, lo que esto comprueba es que no se pierde ni se gana un byte— y
    /// ninguno que ACTÚE nombra la raíz misma, que sería el árbol entero.
    ///
    /// `dest_rel` queda fuera a propósito: cuando se compone con la ortografía
    /// recordada de una carpeta (issue #152), nombra una ruta del destino que
    /// NINGUNA fila trajo —la del fichero nuevo dentro de la carpeta que los dos
    /// lados deletrean distinto— y eso es justo lo que tiene que hacer.
    #[test]
    fn rel_never_escapes((w, rows) in scenario()) {
        let paths = reported(&rows);
        for step in steps(&run(rows, opts_of(w))) {
            let from_source = joined(&source_root(), &step.rel);
            let from_dest = joined(&dest_root(), &step.rel);
            prop_assert!(
                paths.contains(&from_source) || paths.contains(&from_dest),
                "el paso nombra una ruta que ninguna fila trajo: {from_source} / {from_dest}",
            );
            if step.kind != SyncStepKind::Skip {
                prop_assert!(!step.rel.is_root(),
                    "un paso que actúa sobre la raíz actúa sobre el árbol entero");
            }
        }
    }

    /// `Irreversible` aparece si y solo si el paso no puede volver, en las dos
    /// formas que eso tiene: destruir algo sin papelera donde ponerlo, o
    /// cualquier paso contra una papelera que no NOMBRA lo que entierra (sin
    /// `reversal_ref` el undo no acierta ni desenterrando ni borrando, #65). Y
    /// va SIEMPRE con su motivo (regla dura 4).
    #[test]
    fn irreversible_iff_the_step_cannot_come_back((w, rows) in scenario()) {
        let opts = opts_of(w);
        let trash = opts.dest_has_trash;
        let muda = trash && !opts.dest_trash_restorable;
        for step in steps(&run(rows, opts)) {
            let destructive =
                matches!(step.kind, SyncStepKind::Overwrite | SyncStepKind::DeleteTree);
            let actua = destructive
                || matches!(step.kind, SyncStepKind::CreateDir | SyncStepKind::Copy);
            let irreversible = step.reversal == Some(StepReversal::Irreversible);
            prop_assert_eq!(
                irreversible,
                (destructive && !trash) || (actua && muda),
                "{:?}",
                step
            );
            prop_assert!(!irreversible || step.reason.is_some(),
                "un paso irreversible debe su motivo");
        }
    }

    /// La forma de cada BLOQUEO se sostiene sola. Los pasos ya los afirma un
    /// `debug_assert` dentro del transductor; los bloqueos no los afirma nadie,
    /// y `TypeMismatchDir` sin lado no tiene frase que pintar.
    #[test]
    fn every_blocker_is_shaped_consistently((w, rows) in scenario()) {
        for blocker in blockers(&run(rows, opts_of(w))) {
            prop_assert!(blocker.shape_is_consistent(), "{:?}", blocker);
        }
    }

    /// **El blanco de un paso es el fichero que EXISTE en el destino.** Con la
    /// pareja delante —que es cuando se puede saber—, `dest_root +
    /// dest_rel.unwrap_or(rel)` es exactamente la ruta de la entrada del
    /// destino, byte a byte: sobre ext4 eso es la diferencia entre sobrescribir
    /// el `café` que hay y crear un segundo al lado (issue #152).
    #[test]
    fn a_paired_step_targets_the_entry_that_exists(rows in paired_rows_strategy(), mirror in any::<bool>()) {
        let opts = if mirror { opts_mirror() } else { opts_update() };
        let destinations: BTreeSet<String> = rows
            .iter()
            .filter_map(|row| row.right.as_ref())
            .map(|entry| entry.path.to_wire())
            .collect();
        for step in steps(&run(rows, opts)) {
            prop_assert!(destinations.contains(&target(&step)),
                "el paso escribiría en una ruta que en el destino no existe: {}", target(&step));
        }
    }

    /// Y lo que solo está en el ORIGEN hereda la ortografía de su carpeta: la
    /// otra mitad del #152, la que no se lee de la fila sino que se recuerda.
    #[test]
    fn a_child_of_a_folded_folder_lands_inside_it((folder, leaf, rows) in folder_then_child()) {
        let items = run(rows, opts_update());
        let steps = steps(&items);
        prop_assert_eq!(steps.len(), 1, "{:?}", items);
        let expected = joined(
            &dest_root(),
            &RelPath::new(
                [twin_of(&folder), leaf]
                    .into_iter()
                    .map(|s| Segment::new(s).expect("segment"))
                    .collect(),
            ),
        );
        prop_assert_eq!(target(&steps[0]), expected);
    }

    /// **La pregunta que un hasher no puede contestar a mano: dos planes
    /// distintos, ¿dos huellas distintas?** Los candidatos están elegidos para
    /// colisionar si el framing es flojo (los mismos bytes repartidos de otra
    /// forma, una `rel` raíz contra un `dest_rel` ausente, un `Skip` y un bloqueo
    /// en el mismo sitio, prefijos unos de otros). La igualdad va en las DOS
    /// direcciones: distintos ⟹ huellas distintas, e iguales-salvo-`id` ⟹ misma
    /// huella, que es lo que pinea que `id` sea lo ÚNICO que se queda fuera.
    #[test]
    fn the_digest_separates_any_two_different_plans(a in candidate_plan(), b in candidate_plan()) {
        prop_assert_eq!(hash_of(&a) == hash_of(&b), without_ids(&a) == without_ids(&b),
            "\na = {:?}\nb = {:?}", a, b);
    }
}
