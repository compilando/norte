//! El recorrido: dos raíces entran, un flujo de [`CompareRow`] sale.
//!
//! Es **profundidad primero con pila explícita**, no recursión async: sin un
//! future boxeado por nivel y sin pila reventada en un árbol hondo. La memoria
//! viva son los DOS directorios que se están emparejando más la profundidad de
//! la pila, y nada más.
//!
//! # Por qué directorio contra directorio
//!
//! `fs.list` documenta su orden como «el del provider, sin garantía», así que
//! no hay dos flujos ordenados que fusionar. Se drena un directorio de cada
//! lado, se indexan por su clave de emparejamiento ([`crate::key`]) —que sí
//! ordena—, se fusionan las dos listas de claves, se emiten las filas y se
//! apilan los subdirectorios comunes. De ahí sale el techo de memoria, y de ahí
//! sale [`COMPARE_MAX_DIR_ENTRIES`]: un directorio por encima cuesta SU fila,
//! jamás un OOM que se lleve las otras tres horas de trabajo.
//!
//! # Los errores son filas
//!
//! Un listado ilegible, un directorio desmesurado o una colisión de
//! emparejamiento producen su fila y el walk SIGUE. Lo único que termina el
//! flujo antes de tiempo es la cancelación (regla dura 3), y lo dice con un
//! [`CompareError::Cancelled`] final para que quien lo consuma no tenga que
//! adivinar si el árbol se acabó o se cortó.
//!
//! # Orden de las filas
//!
//! Determinista: por directorio, primero las filas ambiguas de la izquierda,
//! luego las de la derecha, y después el merge-join en orden de CLAVE. Los
//! subdirectorios comunes se apilan al revés para que la pila los saque
//! también en orden de clave. Sin ese determinismo no se puede afirmar que
//! comparar al revés da el espejo exacto, que es como se comprueba que la
//! comparación no tiene un lado favorito.
//!
//! La única asimetría conocida es el ORDEN (no los veredictos) cuando LOS DOS
//! lados fallan a la vez —una clave que colisiona en ambos, dos directorios
//! ilegibles emparejados—: las filas de la izquierda salen primero por
//! convenio, y no hay convenio simétrico posible.

use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};

use futures::StreamExt;
use futures::stream::{self, BoxStream};
use norte_proto::{Entry, EntryKind, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::cascade::{Decision, Prefetched, decide};
use crate::key::{PairName, SideIndex, Sides, index_side, key_for};
use crate::{
    COMPARE_MAX_DIR_ENTRIES, CompareConfidence, CompareCriterion, CompareError, CompareOptions,
    CompareReason, CompareRow, CompareVerdict, Side,
};

/// El flujo que produce [`compare`].
///
/// Es un `BoxStream` y no un `impl Stream` por una razón aburrida y buena: el
/// tipo es CONCRETO, así que `norte-core` puede guardarlo en un struct de Task
/// sin arrastrar parámetros de tipo, y las dos raíces se pueden pasar por
/// referencia sin que su préstamo quede capturado en el tipo de retorno. Una
/// asignación por comparación entera.
pub type CompareStream<'a> = BoxStream<'a, Result<CompareRow, CompareError>>;

/// Compara dos árboles y emite una fila por pareja.
///
/// `left`/`right` son los dos providers y `left_root`/`right_root` las dos
/// raíces; no tienen por qué ser del mismo provider ni del mismo scheme.
/// `cancel` es el token de la Task (regla dura 3): en cuanto se dispara, el
/// flujo suelta lo que tuviera pendiente, emite un
/// [`CompareError::Cancelled`] y termina.
///
/// No muta nada y no lee contenido salvo que `opts.criteria.hash` lo pida.
///
/// Es SIMÉTRICA: comparar al revés da las mismas filas con los lados y los
/// veredictos cambiados de sitio, y nada más.
#[must_use]
pub fn compare<'a>(
    left: &'a dyn Provider,
    left_root: &VPath,
    right: &'a dyn Provider,
    right_root: &VPath,
    opts: CompareOptions,
    cancel: CancellationToken,
) -> CompareStream<'a> {
    let walk = Walk {
        left,
        right,
        opts,
        sides: Sides::from_capabilities(left.capabilities(), right.capabilities()),
        cancel,
        stack: vec![Frame {
            left: synthetic_dir(left_root),
            right: synthetic_dir(right_root),
            depth: 0,
        }],
        pending: VecDeque::new(),
        next_id: 0,
        finished: false,
    };
    stream::unfold(walk, |mut walk| async move {
        let item = walk.step().await?;
        Some((item, walk))
    })
    .boxed()
}

/// La `Entry` de una raíz, que nadie listó: el walk la necesita para poder
/// nombrar el directorio en una fila de error, y una raíz no tiene padre que
/// la haya descrito.
fn synthetic_dir(path: &VPath) -> Entry {
    Entry {
        path: path.clone(),
        kind: EntryKind::Dir,
        size: None,
        mtime_ms: None,
        attrs: BTreeMap::new(),
    }
}

/// Un par de directorios pendiente de emparejar, con su profundidad.
struct Frame {
    left: Entry,
    right: Entry,
    depth: u32,
}

/// El estado del recorrido entre dos llamadas al flujo.
struct Walk<'a> {
    left: &'a dyn Provider,
    right: &'a dyn Provider,
    opts: CompareOptions,
    sides: Sides,
    cancel: CancellationToken,
    /// Pila explícita: profundidad primero sin recursión async.
    stack: Vec<Frame>,
    /// Las filas del último directorio emparejado, aún sin entregar.
    pending: VecDeque<CompareRow>,
    /// Contador monótono de [`CompareRow::id`].
    next_id: u64,
    finished: bool,
}

impl Walk<'_> {
    /// Un paso del flujo: entrega la siguiente fila, emparejando directorios
    /// mientras no tenga ninguna a mano.
    async fn step(&mut self) -> Option<Result<CompareRow, CompareError>> {
        loop {
            // Cancelación ANTES de entregar nada (regla dura 3): ninguna fila
            // sale después del corte, ni siquiera una ya calculada.
            if self.cancel.is_cancelled() {
                if self.finished {
                    return None;
                }
                self.finished = true;
                self.pending.clear();
                self.stack.clear();
                return Some(Err(CompareError::Cancelled));
            }
            if let Some(row) = self.pending.pop_front() {
                return Some(Ok(row));
            }
            if self.finished {
                return None;
            }
            let Some(frame) = self.stack.pop() else {
                self.finished = true;
                return None;
            };
            self.visit(frame).await;
        }
    }

    /// Empareja UN par de directorios: llena [`Walk::pending`] con sus filas y
    /// apila los subdirectorios comunes.
    async fn visit(&mut self, frame: Frame) {
        // Los DOS lados se listan siempre, aunque el primero ya haya fallado:
        // dos directorios rotos son dos hechos, y volverse en el primero
        // dejaría el segundo sin descubrir para siempre.
        let listed = (
            list_all(self.left, &frame.left.path, &self.cancel).await,
            list_all(self.right, &frame.right.path, &self.cancel).await,
        );
        if matches!(listed.0, Err(ListFailure::Cancelled))
            || matches!(listed.1, Err(ListFailure::Cancelled))
        {
            return;
        }
        if let Err(ListFailure::Reason(reason)) = listed.0 {
            self.push_error(Some(frame.left.clone()), None, reason, Side::Left);
        }
        if let Err(ListFailure::Reason(reason)) = listed.1 {
            self.push_error(None, Some(frame.right.clone()), reason, Side::Right);
        }
        // Un listado que falló no se sabe qué contenía, así que la otra parte
        // tampoco se puede emparejar: decir `OnlyRight` de sus entradas sería
        // afirmar una ausencia que nadie ha comprobado.
        let (Ok(lefts), Ok(rights)) = listed else {
            return;
        };

        let left_index = index_side(&lefts, self.sides);
        let right_index = index_side(&rights, self.sides);
        let left_collided = collided_keys(&left_index, self.sides);
        let right_collided = collided_keys(&right_index, self.sides);

        // Las colisiones: UNA fila por entrada implicada, jamás una fusión y
        // jamás una deduplicada (contrato normativo de `CompareVerdict::Ambiguous`).
        for (entry, reason) in left_index.collisions() {
            self.push_ambiguous(Some(entry.clone()), None, reason, Side::Left);
        }
        for (entry, reason) in right_index.collisions() {
            self.push_ambiguous(None, Some(entry.clone()), reason, Side::Right);
        }

        let mut descend: Vec<(Entry, Entry)> = Vec::new();
        let mut lefts_iter = left_index.unique().peekable();
        let mut rights_iter = right_index.unique().peekable();
        loop {
            let order = match (lefts_iter.peek(), rights_iter.peek()) {
                (None, None) => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some((lk, _)), Some((rk, _))) => lk.cmp(rk),
            };
            match order {
                Ordering::Less => {
                    let (key, entry) = lefts_iter.next().expect("peek dijo que había");
                    self.only_on_one_side(
                        Some(entry.clone()),
                        None,
                        right_collided.get(key.as_bytes()).copied(),
                        Side::Right,
                    );
                }
                Ordering::Greater => {
                    let (key, entry) = rights_iter.next().expect("peek dijo que había");
                    self.only_on_one_side(
                        None,
                        Some(entry.clone()),
                        left_collided.get(key.as_bytes()).copied(),
                        Side::Left,
                    );
                }
                Ordering::Equal => {
                    let (_, left_entry) = lefts_iter.next().expect("peek dijo que había");
                    let (_, right_entry) = rights_iter.next().expect("peek dijo que había");
                    let decision = self.verdict_for_pair(left_entry, right_entry).await;
                    let id = self.next_id();
                    let row =
                        decision.into_row(id, Some(left_entry.clone()), Some(right_entry.clone()));
                    self.pending.push_back(row);
                    if left_entry.kind == EntryKind::Dir
                        && right_entry.kind == EntryKind::Dir
                        && self.descends_below(frame.depth)
                    {
                        descend.push((left_entry.clone(), right_entry.clone()));
                    }
                }
            }
        }
        drop(lefts_iter);
        drop(rights_iter);

        // Al revés: la pila es LIFO, así que apilar en orden inverso de clave
        // es lo que hace que se saquen en orden de clave.
        let depth = frame.depth.saturating_add(1);
        for (left, right) in descend.into_iter().rev() {
            self.stack.push(Frame { left, right, depth });
        }
    }

    /// Una entrada que solo aparece en un lado. Si la clave que le tocaba en el
    /// OTRO lado está colisionada, la fila NO es `OnlyLeft`/`OnlyRight`: es
    /// `Ambiguous`.
    ///
    /// El motivo es lo que un plan de sincronización haría con cada una.
    /// `OnlyRight` le dice «cópialo al otro lado», y copiar dentro de un
    /// directorio que ya no sabe distinguir esos dos nombres crea un TERCER
    /// fichero que colisiona. `Ambiguous` hace que ese plan se niegue a actuar,
    /// que es la única respuesta segura mientras nadie deshaga la colisión.
    fn only_on_one_side(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        collided_with: Option<CompareReason>,
        collision_side: Side,
    ) {
        if let Some(reason) = collided_with {
            self.push_ambiguous(left, right, reason, collision_side);
            return;
        }
        let decision = if left.is_some() {
            Decision::only_left()
        } else {
            Decision::only_right()
        };
        let id = self.next_id();
        self.pending.push_back(decision.into_row(id, left, right));
    }

    /// La decisión de UNA pareja emparejada, con lo que exige I/O ya averiguado.
    async fn verdict_for_pair(&self, left: &Entry, right: &Entry) -> Decision {
        // `read_link` SOLO cuando los dos lados son enlaces: si uno no lo es,
        // el rung de kind ya decidió y leer el destino del otro es una llamada
        // al provider a cambio de nada.
        let (left_target, right_target) =
            if left.kind == EntryKind::Symlink && right.kind == EntryKind::Symlink {
                (
                    self.left.read_link(&left.path).await.ok(),
                    self.right.read_link(&right.path).await.ok(),
                )
            } else {
                (None, None)
            };
        let facts = Prefetched::links(left_target.as_deref(), right_target.as_deref());
        let decision = decide(left, right, &self.opts, &facts);

        // ---- costura de C5: el rung de hash ----
        // `decision.needs_hash == true` significa exactamente esto: los rungs
        // baratos dieron la pareja por IGUAL y el llamante pidió hash, así que
        // la decisión NO es final. C5 lee los dos ficheros aquí (chequeando el
        // token por chunk, no por fichero) y vuelve a llamar a `decide` con
        // `facts.with_hash(...)`; una lectura que falle es su fila de
        // `CompareVerdict::Error` con `CompareReason::ReadFailed` y el lado que
        // falló. En C4 ninguna opción enciende el rung.
        debug_assert!(
            !decision.needs_hash,
            "el rung de hash está pedido y nadie lo ha enchufado todavía (C5)"
        );
        decision
    }

    /// ¿Se puede bajar un nivel más desde `depth`?
    fn descends_below(&self, depth: u32) -> bool {
        self.opts
            .max_depth
            .is_none_or(|max| depth.saturating_add(1) <= max)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn push_ambiguous(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        reason: CompareReason,
        side: Side,
    ) {
        let id = self.next_id();
        self.pending.push_back(flagged(
            id,
            left,
            right,
            CompareVerdict::Ambiguous,
            reason,
            side,
        ));
    }

    fn push_error(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        reason: CompareReason,
        side: Side,
    ) {
        let id = self.next_id();
        self.pending.push_back(flagged(
            id,
            left,
            right,
            CompareVerdict::Error,
            reason,
            side,
        ));
    }
}

/// Las dos filas que la cascada no produce: `Ambiguous` y `Error`. Las dos
/// llevan motivo y lado obligatorios, y ningún rung las decidió — de ahí
/// `Presence`/`Unknown`, que es la convención del wire para «aquí no informa el
/// criterio».
fn flagged(
    id: u64,
    left: Option<Entry>,
    right: Option<Entry>,
    verdict: CompareVerdict,
    reason: CompareReason,
    side: Side,
) -> CompareRow {
    let row = CompareRow {
        id,
        left,
        right,
        verdict,
        criterion: CompareCriterion::Presence,
        confidence: CompareConfidence::Unknown,
        newer: None,
        reason: Some(reason),
        side: Some(side),
    };
    debug_assert!(row.reason_is_consistent(), "fila {verdict:?} sin motivo");
    debug_assert!(
        row.sides_are_consistent(),
        "fila {verdict:?} con lados rotos"
    );
    row
}

/// Por qué no se pudo emparejar un directorio.
enum ListFailure {
    /// El motivo que viaja en la fila.
    Reason(CompareReason),
    /// Cancelado a mitad del drenaje: no hay fila, hay final de flujo.
    Cancelled,
}

/// Drena el listado de un directorio ENTERO, con techo.
///
/// El techo se comprueba ANTES de meter la entrada, así que un directorio de
/// exactamente [`COMPARE_MAX_DIR_ENTRIES`] entradas se empareja y uno de una
/// más se rechaza sin haber materializado la de más: el límite es también el
/// techo de memoria, no solo el de la respuesta.
///
/// El token se mira dentro del bucle: un directorio de cientos de miles de
/// entradas no puede hacer esperar a la cancelación hasta que termine de
/// drenarse.
async fn list_all(
    provider: &dyn Provider,
    dir: &VPath,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>, ListFailure> {
    let mut stream = provider
        .list(dir)
        .await
        .map_err(|_| ListFailure::Reason(CompareReason::Unreadable))?;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            return Err(ListFailure::Cancelled);
        }
        // Un error a mitad de listado deja el directorio a MEDIAS, y medio
        // listado emparejado produciría `OnlyLeft` de entradas que sí estaban:
        // vale como ilegible entero.
        let entry = item.map_err(|_| ListFailure::Reason(CompareReason::Unreadable))?;
        if out.len() >= COMPARE_MAX_DIR_ENTRIES {
            return Err(ListFailure::Reason(CompareReason::DirTooLarge));
        }
        out.push(entry);
    }
    Ok(out)
}

/// Las claves COLISIONADAS de un lado, con el motivo de su colisión.
///
/// Solo recorre las entradas que ya colisionan (casi siempre ninguna), no el
/// listado entero. El motivo que se guarda es el de la primera entrada del
/// grupo en orden de listado: un grupo puede tener causas distintas por
/// pareja, y la fila del lado contrario necesita UNA.
fn collided_keys(index: &SideIndex<'_, Entry>, sides: Sides) -> BTreeMap<Vec<u8>, CompareReason> {
    let mut out = BTreeMap::new();
    for (entry, reason) in index.collisions() {
        out.entry(key_for(entry.pair_name(), sides).as_bytes().to_vec())
            .or_insert(reason);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use norte_proto::Segment;
    use norte_testkit::{MemProvider, TarSmith};
    use norte_vfs_archive::{ArchiveProvider, Format};

    use super::*;

    // ---------- utillería de árboles ----------

    /// Trocea `"sub/deep/c.txt"` en segmentos crudos. Los tests hablan `&str`
    /// por comodidad; lo que viaja al provider son BYTES (regla dura 1).
    fn segments(path: &str) -> Vec<Segment> {
        path.split('/')
            .map(|s| Segment::new(s.as_bytes().to_vec()).expect("segmento válido"))
            .collect()
    }

    /// Crea `path` con `content`, materializando sus directorios intermedios.
    async fn seed(mem: &MemProvider, path: &str, content: &[u8]) {
        let segs = segments(path);
        let (name, dirs) = segs.split_last().expect("path no vacío");
        let mut at = MemProvider::root();
        for dir in dirs {
            at = at.join(dir.clone());
            // Ya existe: el árbol lo comparten varios paths sembrados.
            let _ = mem.mkdir(&at).await;
        }
        let file = at.join(name.clone());
        let mut sink = mem.write(&file).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    /// Un árbol con esos ficheros; el contenido de cada uno es su propio path,
    /// así que dos árboles con la misma lista salen idénticos byte a byte.
    async fn tree(paths: &[&str]) -> MemProvider {
        let mem = MemProvider::new();
        for path in paths {
            seed(&mem, path, path.as_bytes()).await;
        }
        mem
    }

    /// Dos árboles idénticos. Los mtimes de `MemProvider` son un reloj LÓGICO
    /// (una unidad por mutación), así que sembrar la misma lista en el mismo
    /// orden da las mismas fechas: nada de este test depende del reloj de
    /// pared.
    async fn twin_trees(paths: &[&str]) -> (MemProvider, MemProvider) {
        (tree(paths).await, tree(paths).await)
    }

    /// Un `wide/` con `n` entradas a la IZQUIERDA y vacío a la derecha. Ancho
    /// de un solo lado a propósito: el techo se comprueba por lado, y sembrar
    /// el doble solo dobla lo que tarda el test.
    async fn twin_trees_with_wide_dir(n: usize) -> (MemProvider, MemProvider) {
        let left = MemProvider::new();
        let right = MemProvider::new();
        let wide = MemProvider::root().join(Segment::new(b"wide".to_vec()).expect("seg"));
        left.mkdir(&wide).await.expect("mkdir");
        right.mkdir(&wide).await.expect("mkdir");
        for i in 0..n {
            let name = Segment::new(format!("e{i:07}").into_bytes()).expect("seg");
            left.mkdir(&wide.join(name)).await.expect("mkdir");
        }
        (left, right)
    }

    /// Árboles con una fila de cada categoría barata.
    async fn trees_that_differ() -> (MemProvider, MemProvider) {
        let left = tree(&["igual.txt", "solo-izq.txt", "sub/dentro.txt"]).await;
        let right = tree(&["igual.txt", "solo-der.txt", "sub/dentro.txt"]).await;
        seed(&left, "tamano.txt", b"aaaa").await;
        seed(&right, "tamano.txt", b"aaaaaaaaaaaa").await;
        (left, right)
    }

    /// El `list` de `dir` falla con E/S; el resto del árbol se lista normal.
    fn deny_list(mem: &MemProvider, dir: &str) {
        let mut at = MemProvider::root();
        for seg in segments(dir) {
            at = at.join(seg);
        }
        mem.faults().fail_list_at(&at);
    }

    // ---------- utillería de filas ----------

    fn compare_with<'a>(
        left: &'a MemProvider,
        right: &'a MemProvider,
        opts: CompareOptions,
    ) -> CompareStream<'a> {
        compare(
            left,
            &MemProvider::root(),
            right,
            &MemProvider::root(),
            opts,
            CancellationToken::new(),
        )
    }

    fn compare_default<'a>(left: &'a MemProvider, right: &'a MemProvider) -> CompareStream<'a> {
        compare_with(left, right, CompareOptions::cheap())
    }

    async fn collect(stream: CompareStream<'_>) -> Vec<CompareRow> {
        stream
            .map(|item| item.expect("ninguna de estas comparaciones se cancela"))
            .collect()
            .await
    }

    /// ¿Alguno de los dos lados de la fila se llama así? Por BYTES: `VPath` no
    /// tiene `as_bytes` porque un nombre no es texto (regla dura 1).
    fn named(row: &CompareRow, name: &[u8]) -> bool {
        [row.left.as_ref(), row.right.as_ref()]
            .into_iter()
            .flatten()
            .any(|entry| entry.path.file_name().map(Segment::as_bytes) == Some(name))
    }

    fn flip(side: Side) -> Side {
        match side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            // `Side` NO es `#[non_exhaustive]` (a diferencia de los cuatro
            // vocabularios de la comparación): un lado que este binario no
            // conoce no tiene espejo, y decir que sí lo tiene sería inventárselo.
            Side::Unknown => Side::Unknown,
        }
    }

    /// Las mismas filas vistas desde el otro lado: entradas, veredicto, lado
    /// más nuevo y lado del motivo, todo cambiado de sitio. Nada más.
    fn mirror(rows: &[CompareRow]) -> Vec<CompareRow> {
        rows.iter()
            .map(|row| CompareRow {
                left: row.right.clone(),
                right: row.left.clone(),
                verdict: match row.verdict {
                    CompareVerdict::OnlyLeft => CompareVerdict::OnlyRight,
                    CompareVerdict::OnlyRight => CompareVerdict::OnlyLeft,
                    other => other,
                },
                newer: row.newer.map(flip),
                side: row.side.map(flip),
                ..row.clone()
            })
            .collect()
    }

    // ---------- los tests del plan ----------

    /// The base case, and the one a user runs after every copy: two identical
    /// trees produce nothing but `Same`, at every depth.
    #[tokio::test]
    async fn identical_trees_are_all_same() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt", "sub/deep/c.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            rows.iter().all(|row| row.verdict == CompareVerdict::Same),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|row| named(row, b"c.txt")), "{rows:#?}");
    }

    /// A directory that exists on one side only is ONE row, not its whole
    /// subtree: the plan will copy it with a recursive `fs.copy`, so
    /// enumerating it buys nothing and costs the walk everything.
    #[tokio::test]
    async fn an_orphan_directory_is_one_row_and_is_not_enumerated() {
        let l = tree(&["only/1.txt", "only/2.txt", "only/deep/3.txt"]).await;
        let r = tree(&[]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::OnlyLeft);
        assert_eq!(
            rows[0].left.as_ref().expect("el lado que sí está").kind,
            EntryKind::Dir
        );
    }

    /// Swapping the sides mirrors the verdicts and nothing else. A comparison
    /// that is not symmetric is a comparison that has a favourite.
    #[tokio::test]
    async fn comparing_the_other_way_round_mirrors_the_verdicts() {
        let (l, r) = trees_that_differ().await;
        let forward = collect(compare_default(&l, &r)).await;
        let backward = collect(compare_default(&r, &l)).await;
        assert!(!forward.is_empty());
        assert_eq!(mirror(&forward), backward);
    }

    /// One unreadable subdirectory must cost ITSELF, not the other 40 000
    /// leaves. This is the difference between a three-hour comparison that
    /// answers and one that dies at the first EACCES.
    #[tokio::test]
    async fn an_unreadable_directory_is_a_row_and_the_walk_continues() {
        let (l, r) = twin_trees(&["ok.txt", "denied/x.txt", "after/y.txt"]).await;
        deny_list(&l, "denied");
        let rows = collect(compare_default(&l, &r)).await;
        let bad = rows
            .iter()
            .find(|row| row.verdict == CompareVerdict::Error)
            .expect("error row");
        assert_eq!(bad.reason, Some(CompareReason::Unreadable));
        assert_eq!(bad.side, Some(Side::Left));
        assert!(
            rows.iter().any(|row| named(row, b"y.txt")),
            "the walk stopped at the error"
        );
    }

    /// A directory over the declared cap costs that directory, not an OOM.
    #[tokio::test]
    async fn a_directory_over_the_cap_is_a_row_not_an_oom() {
        let (l, r) = twin_trees_with_wide_dir(COMPARE_MAX_DIR_ENTRIES + 1).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            rows.iter()
                .any(|row| row.reason == Some(CompareReason::DirTooLarge)),
            "{rows:#?}"
        );
    }

    /// Hard rule 3. Cancelling stops the stream — no row after the cut, no work
    /// after the cut, and nothing to clean up because nothing is written.
    #[tokio::test]
    async fn cancelling_stops_the_stream_cleanly() {
        let (l, r) = twin_trees_with_wide_dir(5_000).await;
        let cancel = CancellationToken::new();
        let mut stream = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            cancel.clone(),
        );
        let first = stream.next().await.expect("at least one row");
        assert!(first.is_ok());
        cancel.cancel();
        let rest = stream.count().await;
        assert!(
            rest < 5_000,
            "the walk kept going after cancellation: {rest} more rows"
        );
    }

    /// Un token que ya venía disparado no empareja NADA: ni un listado, ni una
    /// fila. La cancelación se mira antes de trabajar, no después.
    #[tokio::test]
    async fn un_token_ya_cancelado_no_empareja_nada() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt"]).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let items: Vec<Result<CompareRow, CompareError>> = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            cancel,
        )
        .collect()
        .await;
        assert_eq!(items, vec![Err(CompareError::Cancelled)]);
    }

    /// El drenaje de UN directorio mira el token entrada a entrada.
    ///
    /// Es la mitad de la regla dura 3 que los tests de flujo no pueden ver: un
    /// directorio de cientos de miles de entradas se drena DENTRO de un solo
    /// paso del flujo, así que sin este chequeo la cancelación esperaría a que
    /// terminase. Se prueba sobre `list_all` directamente porque hacerlo por el
    /// flujo exigiría cancelar a mitad de un `await`, que es una carrera.
    #[tokio::test]
    async fn el_drenaje_de_un_directorio_mira_el_token() {
        let mem = tree(&["a.txt", "b.txt"]).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = list_all(&mem, &MemProvider::root(), &cancel).await;
        assert!(
            matches!(outcome, Err(ListFailure::Cancelled)),
            "el drenaje siguió con el token disparado"
        );
        // Y sin cancelar, el mismo listado sí se drena entero.
        let ok = list_all(&mem, &MemProvider::root(), &CancellationToken::new()).await;
        assert!(matches!(ok, Ok(entries) if entries.len() == 2));
    }

    /// `max_depth` bounds the descent and says so by not emitting deeper rows.
    #[tokio::test]
    async fn max_depth_bounds_the_descent() {
        let (l, r) = twin_trees(&["a.txt", "one/b.txt", "one/two/c.txt"]).await;
        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().max_depth(1))).await;
        assert!(rows.iter().any(|row| named(row, b"b.txt")), "{rows:#?}");
        assert!(!rows.iter().any(|row| named(row, b"c.txt")), "{rows:#?}");
    }

    // ---------- lo que el plan no fija ----------

    /// El walk LEE los destinos de los enlaces y se los pasa a la cascada.
    ///
    /// La cascada ya prueba que dos destinos distintos son `Different`; lo que
    /// falta probar aquí es que alguien llama a `read_link`. Sin este test, un
    /// walk que no leyera un solo destino seguiría pasando el test del archivo
    /// —que espera `Unknown` precisamente porque los destinos NO se pueden
    /// leer—: la ausencia de la llamada y la ausencia de la respuesta se ven
    /// igual desde fuera.
    #[tokio::test]
    async fn el_walk_lee_los_destinos_de_los_enlaces() {
        let link = || MemProvider::root().join(Segment::new(b"l".to_vec()).expect("seg"));
        let sembrar = async |target: &'static [u8]| {
            let mem = MemProvider::new();
            mem.symlink(&link(), target, norte_vfs::SymlinkKind::File)
                .await
                .expect("symlink");
            mem
        };

        let l = sembrar(b"../a").await;
        let distinto = sembrar(b"../b").await;
        let rows = collect(compare_default(&l, &distinto)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::LinkTarget,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );

        let igual = sembrar(b"../a").await;
        let rows = collect(compare_default(&l, &igual)).await;
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::LinkTarget,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );
    }

    /// Un enlace contra un fichero no gasta un `read_link`: el rung de kind ya
    /// decidió, y preguntar por el destino del otro es una llamada al provider
    /// a cambio de nada.
    #[tokio::test]
    async fn un_enlace_contra_un_fichero_es_type_mismatch() {
        let l = MemProvider::new();
        l.symlink(
            &MemProvider::root().join(Segment::new(b"x".to_vec()).expect("seg")),
            b"../a",
            norte_vfs::SymlinkKind::File,
        )
        .await
        .expect("symlink");
        let r = tree(&["x"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::TypeMismatch);
        assert_eq!(rows[0].criterion, CompareCriterion::Kind);
    }

    /// El walk sigue DESPUÉS del error, no solo alrededor: `zz` ordena por
    /// detrás de `denied`, así que su fila solo puede existir si el recorrido
    /// continuó tras la fila de error.
    #[tokio::test]
    async fn el_walk_sigue_despues_del_directorio_ilegible() {
        let (l, r) = twin_trees(&["denied/x.txt", "zz/z.txt"]).await;
        deny_list(&l, "denied");
        let rows = collect(compare_default(&l, &r)).await;
        let error_at = rows
            .iter()
            .position(|row| row.verdict == CompareVerdict::Error)
            .expect("la fila de error");
        let z_at = rows
            .iter()
            .position(|row| named(row, b"z.txt"))
            .expect("la hoja de después");
        assert!(error_at < z_at, "{rows:#?}");
    }

    /// El id es monótono y no se repite: la selección del panel se ancla a él.
    #[tokio::test]
    async fn los_ids_son_monotonos_y_unicos() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt", "sub/deep/c.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        let ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
        let mut ordenados = ids.clone();
        ordenados.sort_unstable();
        ordenados.dedup();
        assert_eq!(ids, ordenados, "{ids:?}");
    }

    /// Dos entradas de un mismo lado que colapsan salen como DOS filas
    /// `Ambiguous`, cada una en el campo de SU lado y con `side` nombrando
    /// dónde está la colisión. Jamás se funden y jamás se deduplican.
    #[tokio::test]
    async fn una_colision_de_un_lado_sale_una_fila_por_entrada() {
        let left = tree(&["README", "readme"]).await;
        // Un lado que no distingue caja: emparejar contra él es plegar.
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);
        seed(&right, "README", b"README").await;

        let rows = collect(compare_default(&left, &right)).await;
        let ambiguas: Vec<&CompareRow> = rows
            .iter()
            .filter(|row| row.verdict == CompareVerdict::Ambiguous)
            .collect();
        assert_eq!(
            ambiguas.len(),
            3,
            "dos colisionadas + su contraparte: {rows:#?}"
        );
        assert!(
            ambiguas
                .iter()
                .all(|row| row.reason == Some(CompareReason::CaseFold)
                    && row.side == Some(Side::Left)),
            "{ambiguas:#?}"
        );
        // Las dos de la izquierda llevan SU entrada; la contraparte, la suya.
        let izquierdas = ambiguas.iter().filter(|row| row.left.is_some()).count();
        let derechas = ambiguas.iter().filter(|row| row.right.is_some()).count();
        assert_eq!((izquierdas, derechas), (2, 1), "{ambiguas:#?}");
        assert!(
            ambiguas
                .iter()
                .all(|row| row.left.is_none() || row.right.is_none()),
            "una colisión es de UN lado: {ambiguas:#?}"
        );
    }

    /// La contraparte solitaria de una colisión NO es `OnlyRight`.
    ///
    /// Decirle `OnlyRight` a un plan de sincronización es decirle «cópialo al
    /// otro lado», y copiar dentro de un directorio que ya no sabe distinguir
    /// esos dos nombres crea un TERCER fichero colisionado. `Ambiguous` hace
    /// que ese plan se niegue a actuar, que es la única respuesta segura.
    #[tokio::test]
    async fn la_contraparte_de_una_colision_no_se_ofrece_para_copiar() {
        let left = tree(&["README", "readme"]).await;
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);
        seed(&right, "README", b"README").await;

        let rows = collect(compare_default(&left, &right)).await;
        let contraparte = rows
            .iter()
            .find(|row| row.right.is_some() && row.left.is_none())
            .expect("la fila de la contraparte");
        assert_eq!(contraparte.verdict, CompareVerdict::Ambiguous);
        assert_eq!(contraparte.reason, Some(CompareReason::CaseFold));
        assert_eq!(
            contraparte.side,
            Some(Side::Left),
            "el lado que colisiona es el izquierdo, no el de la fila"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.verdict == CompareVerdict::OnlyRight),
            "{rows:#?}"
        );
    }

    /// Dos directorios ilegibles emparejados son DOS filas, una por lado.
    ///
    /// Volverse en el primer fallo es lo cómodo y deja el segundo sin
    /// descubrir: el usuario arreglaría los permisos de la izquierda y la
    /// comparación siguiente le enseñaría el mismo directorio roto otra vez,
    /// ahora por el otro lado.
    #[tokio::test]
    async fn dos_lados_ilegibles_son_dos_filas() {
        let (l, r) = twin_trees(&["dir/a.txt"]).await;
        deny_list(&l, "dir");
        deny_list(&r, "dir");
        let rows = collect(compare_default(&l, &r)).await;
        let errores: Vec<&CompareRow> = rows
            .iter()
            .filter(|row| row.verdict == CompareVerdict::Error)
            .collect();
        assert_eq!(errores.len(), 2, "{rows:#?}");
        assert_eq!(errores[0].side, Some(Side::Left));
        assert_eq!(errores[1].side, Some(Side::Right));
        // Cada fila lleva el directorio del lado que nombra, y solo ese.
        assert!(errores[0].left.is_some() && errores[0].right.is_none());
        assert!(errores[1].right.is_some() && errores[1].left.is_none());
    }

    /// Un directorio ilegible NO convierte en `OnlyRight` lo que hay enfrente:
    /// nadie ha comprobado esa ausencia.
    #[tokio::test]
    async fn un_listado_roto_no_inventa_ausencias_en_el_otro_lado() {
        let (l, r) = twin_trees(&["dir/a.txt", "dir/b.txt"]).await;
        deny_list(&l, "dir");
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            !rows
                .iter()
                .any(|row| row.verdict == CompareVerdict::OnlyRight),
            "{rows:#?}"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.verdict == CompareVerdict::Error)
                .count(),
            1,
            "{rows:#?}"
        );
    }

    // ---------- el provider que de verdad no puede contestar ----------

    /// Un tar REAL, indexado por el provider archive.
    ///
    /// El contenedor vive en un `MemProvider` porque lo que se está probando es
    /// el archivo, no el filesystem que lo guarda.
    async fn tar_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
        let mem = Arc::new(MemProvider::new());
        let container = MemProvider::root().join(Segment::new(b"f.tar".to_vec()).expect("seg"));
        let mut sink = mem.write(&container).await.expect("write");
        sink.write(Bytes::copy_from_slice(bytes))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
        let root = VPath::archive_compose("tar", &container, &[]).expect("compose");
        let provider = ArchiveProvider::new(mem as Arc<dyn Provider>, Format::Tar, "tar+mem");
        (provider, root)
    }

    /// `Unknown` exigido a un provider que de VERDAD no puede contestar, no a
    /// un mock al que se le dice qué decir.
    ///
    /// El tar lleva un enlace cuyo header no trae destino —los escribe
    /// cualquier productor descuidado, y el provider archive ya tiene su camino
    /// para eso: `read_link` contesta `Corrupt`—. Sin los dos destinos no hay
    /// comparación posible, y la respuesta honesta es `Same`/`LinkTarget`/
    /// `Unknown`: inventarse una diferencia sería tan falso como inventarse una
    /// igualdad, y llamarlo error sería decir que la comparación falló cuando
    /// lo que pasa es que no se sabe.
    #[tokio::test]
    async fn a_real_archive_produces_unknown_rather_than_a_guess() {
        let bytes = TarSmith::new()
            .file(b"a.txt", b"hola")
            .symlink(b"link", b"")
            .build();
        let (zip, zip_root) = tar_provider(&bytes).await;

        let local = MemProvider::new();
        seed(&local, "a.txt", b"hola").await;
        local
            .symlink(
                &MemProvider::root().join(Segment::new(b"link".to_vec()).expect("seg")),
                b"../x",
                norte_vfs::SymlinkKind::File,
            )
            .await
            .expect("symlink");

        let stream = compare(
            &local,
            &MemProvider::root(),
            &zip,
            &zip_root,
            CompareOptions::cheap(),
            CancellationToken::new(),
        );
        let rows = collect(stream).await;

        let row = rows
            .iter()
            .find(|row| named(row, b"link"))
            .expect("the paired row");
        assert_eq!(row.confidence, CompareConfidence::Unknown, "{rows:#?}");
        assert_eq!(row.criterion, CompareCriterion::LinkTarget);
        assert_ne!(
            row.verdict,
            CompareVerdict::Error,
            "unknown is an answer, not a failure"
        );

        // Y lo que el archivo SÍ sabe contestar se contesta.
        let fichero = rows
            .iter()
            .find(|row| named(row, b"a.txt"))
            .expect("la pareja normal");
        assert_ne!(fichero.verdict, CompareVerdict::Error, "{rows:#?}");
        assert_ne!(fichero.confidence, CompareConfidence::Unknown);
    }
}
