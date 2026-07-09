//! [`MemProvider`]: FS simulado en memoria, determinista (`BTreeMap` + reloj
//! lógico), con capabilities configurables e inyección de fallos. Es el banco
//! de pruebas del copy engine y de la suite contractual (spec §12).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{
    Authority, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Scheme, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};

use crate::faults::{Faults, SegPath, seg_path};

/// Tamaño de chunk de los streams de lectura (pequeño a propósito: obliga a
/// los consumidores a manejar multi-chunk incluso con contenidos de test).
const READ_CHUNK: usize = 1024;

#[derive(Debug, Clone)]
enum Node {
    File { content: Vec<u8>, mtime: i64 },
    Dir { mtime: i64 },
}

#[derive(Debug, Default)]
struct Tree {
    /// Nodos por path de segmentos; la raíz es implícita (siempre Dir).
    nodes: BTreeMap<SegPath, Node>,
    /// Reloj lógico: avanza 1 por mutación → mtimes deterministas.
    clock: i64,
}

impl Tree {
    fn tick(&mut self) -> i64 {
        self.clock += 1;
        self.clock
    }
}

/// Provider en memoria para tests. Determinista: mismo guion de operaciones →
/// mismo árbol, mismos mtimes (reloj lógico), mismo orden de listado (orden
/// de bytes del `BTreeMap`).
///
/// # Límites del simulador (léelos antes de escribir tests de colisión)
///
/// - **El fold de caja es ASCII puro** (`eq_ignore_ascii_case`). `Ñ` y `ñ` NO
///   colisionan aquí, pero SÍ en NTFS (`$UpCase`) y APFS (fold Unicode). Un byte
///   trail de multibyte legacy (p. ej. Shift-JIS `83 65`) puede producir una
///   `CaseCollision` espuria contra `83 45`. NO escribas tests que dependan
///   de colisión/no-colisión de caja Unicode contra este simulador.
/// - **No simula insensibilidad a normalización** (APFS: é NFC y NFD son el
///   mismo archivo). Ese eje llegará como knob propio (issue de deuda M0).
/// - El scheme/authority del `VPath` de entrada no se valida: todas las
///   authorities comparten el mismo árbol.
///
/// ```
/// use norte_testkit::MemProvider;
/// use norte_vfs::Provider;
///
/// let mem = MemProvider::new();
/// assert_eq!(mem.scheme(), "mem");
/// ```
pub struct MemProvider {
    caps: Capabilities,
    tree: Arc<Mutex<Tree>>,
    faults: Arc<Faults>,
}

impl MemProvider {
    /// Provider con las capabilities por defecto de un FS "unix-like":
    /// `RENAME_ATOMIC | CASE_SENSITIVE | CASE_PRESERVING`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_flags(
            CapabilityFlags::RENAME_ATOMIC
                | CapabilityFlags::CASE_SENSITIVE
                | CapabilityFlags::CASE_PRESERVING,
        )
    }

    /// Provider con flags a medida (p. ej. sin `CASE_SENSITIVE` para simular
    /// NTFS/APFS — con el límite de que el fold es ASCII, ver doc del tipo —
    /// o con `SERVER_COPY` para probar `copy_native`).
    #[must_use]
    pub fn with_flags(flags: CapabilityFlags) -> Self {
        Self {
            caps: Capabilities {
                flags,
                max_path: None,
            },
            tree: Arc::new(Mutex::new(Tree::default())),
            faults: Arc::new(Faults::default()),
        }
    }

    /// Handle de inyección de fallos (compartible con el test mientras el
    /// provider está en uso).
    #[must_use]
    pub fn faults(&self) -> Arc<Faults> {
        Arc::clone(&self.faults)
    }

    /// La raíz de este provider: `mem:///`.
    ///
    /// # Panics
    /// Nunca: el scheme es constante y válido.
    #[must_use]
    pub fn root() -> VPath {
        VPath::root(Scheme::new("mem").expect("scheme constante válido"), None)
    }

    fn case_sensitive(&self) -> bool {
        self.caps.flags.contains(CapabilityFlags::CASE_SENSITIVE)
    }

    fn lock(&self) -> MutexGuard<'_, Tree> {
        // Invariante: nadie panica con el lock tomado.
        self.tree.lock().expect("tree lock sano")
    }
}

impl Default for MemProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Igualdad con fold ASCII (los límites están documentados en [`MemProvider`]).
fn fold_eq_path(a: &SegPath, b: &SegPath) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Resuelve `key` contra el árbol según la sensibilidad a la caja.
/// Devuelve la clave REAL almacenada (puede diferir en caja).
fn resolve(tree: &Tree, case_sensitive: bool, key: &SegPath) -> Option<SegPath> {
    if tree.nodes.contains_key(key) {
        return Some(key.clone());
    }
    if !case_sensitive {
        return tree.nodes.keys().find(|k| fold_eq_path(k, key)).cloned();
    }
    None
}

/// Canonicaliza `key` respecto a la caja ALMACENADA: cada ancestro adopta la
/// caja real de su Dir (y debe existir como Dir); la hoja conserva la caja
/// pedida (case-preserving). Sin esto, una inserción vía caja distinta crearía
/// huérfanos invisibles para `list` — imposible en un FS real.
///
/// `None` si algún ancestro falta o no es Dir.
fn canonical_key(tree: &Tree, case_sensitive: bool, key: &SegPath) -> Option<SegPath> {
    let mut canon: SegPath = Vec::with_capacity(key.len());
    for (i, seg) in key.iter().enumerate() {
        if i == key.len() - 1 {
            canon.push(seg.clone());
        } else {
            let mut probe = canon.clone();
            probe.push(seg.clone());
            let real = resolve(tree, case_sensitive, &probe)?;
            if !matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
                return None;
            }
            canon = real;
        }
    }
    Some(canon)
}

/// Clasifica una colisión: byte-exacta = `Exists`, solo-por-fold = `CaseCollision`.
fn collision_kind(real: &SegPath, requested: &SegPath) -> ConflictKind {
    if real == requested {
        ConflictKind::Exists
    } else {
        ConflictKind::CaseCollision
    }
}

/// Reconstruye la [`Entry`] de una clave del árbol sobre el scheme y la
/// authority de `base` (la authority se preserva: la identidad del path en el
/// wire no puede cambiar por pasar por el provider).
fn entry_for(base: &VPath, key: &SegPath, node: &Node) -> Entry {
    let authority = base
        .authority()
        .map(|a| Authority::new(a).expect("authority ya validada por VPath"));
    let mut p = VPath::root(
        Scheme::new(base.scheme()).expect("scheme ya validado"),
        authority,
    );
    for seg in key {
        p = p.join(norte_proto::Segment::new(seg.clone()).expect("clave del árbol ya validada"));
    }
    match node {
        Node::File { content, mtime } => Entry {
            path: p,
            kind: EntryKind::File,
            size: Some(content.len() as u64),
            mtime_ms: Some(*mtime),
        },
        Node::Dir { mtime } => Entry {
            path: p,
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: Some(*mtime),
        },
    }
}

#[async_trait]
impl Provider for MemProvider {
    // La firma del trait es `-> &str`; devolver un literal aquí es correcto.
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "mem"
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let cs = self.case_sensitive();
        let tree = self.lock();
        if key.is_empty() {
            return Ok(Entry {
                path: p.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: Some(0),
            });
        }
        let real = resolve(&tree, cs, &key).ok_or(Error::NotFound)?;
        let node = tree.nodes.get(&real).ok_or(Error::NotFound)?;
        Ok(entry_for(p, &real, node))
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let cs = self.case_sensitive();
        let tree = self.lock();
        // El filtro de hijos usa la clave REAL: listar con otra caja debe
        // ver lo mismo que stat (coherencia con el FS simulado).
        let real = if key.is_empty() {
            key
        } else {
            let real = resolve(&tree, cs, &key).ok_or(Error::NotFound)?;
            if !matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            real
        };
        let entries: Vec<Result<Entry, Error>> = tree
            .nodes
            .iter()
            .filter(|(k, _)| k.len() == real.len() + 1 && k.starts_with(&real))
            .map(|(k, node)| Ok(entry_for(p, k, node)))
            .collect();
        Ok(futures::stream::iter(entries).boxed())
    }

    async fn read(&self, p: &VPath) -> Result<ByteStream, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let cs = self.case_sensitive();
        let tree = self.lock();
        let real = resolve(&tree, cs, &key).ok_or(Error::NotFound)?;
        let content = match tree.nodes.get(&real) {
            Some(Node::File { content, .. }) => content.clone(),
            Some(Node::Dir { .. }) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            None => return Err(Error::NotFound),
        };
        drop(tree);

        // Snapshot del fallo: el stream truncará en el byte exacto.
        let fail_at = self.faults.read_fault_for(&key);
        let mut chunks: Vec<Result<Bytes, Error>> = Vec::new();
        let mut emitted = 0usize;
        for chunk in content.chunks(READ_CHUNK) {
            if let Some(n) = fail_at
                && emitted + chunk.len() >= n
            {
                let take = n.saturating_sub(emitted);
                if take > 0 {
                    chunks.push(Ok(Bytes::copy_from_slice(&chunk[..take])));
                }
                chunks.push(Err(Error::Io { retryable: false }));
                return Ok(futures::stream::iter(chunks).boxed());
            }
            emitted += chunk.len();
            chunks.push(Ok(Bytes::copy_from_slice(chunk)));
        }
        Ok(futures::stream::iter(chunks).boxed())
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let cs = self.case_sensitive();
        let tree = self.lock();
        let canon = canonical_key(&tree, cs, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, cs, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        drop(tree);

        Ok(Box::new(MemSink {
            key: canon,
            buffer: Vec::new(),
            fail_at: self.faults.write_fault_for(&key),
            written: 0,
            tree: Arc::clone(&self.tree),
            case_sensitive: cs,
        }))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        let cs = self.case_sensitive();
        let mut tree = self.lock();
        let canon = canonical_key(&tree, cs, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, cs, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        let mtime = tree.tick();
        tree.nodes.insert(canon, Node::Dir { mtime });
        Ok(())
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::Unsupported);
        }
        let cs = self.case_sensitive();
        let mut tree = self.lock();
        let real = resolve(&tree, cs, &key).ok_or(Error::NotFound)?;
        if matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
            let has_children = tree
                .nodes
                .keys()
                .any(|k| k.len() > real.len() && k.starts_with(&real));
            if has_children {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
        }
        tree.nodes.remove(&real);
        tree.tick();
        Ok(())
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let from_key = seg_path(from);
        let to_key = seg_path(to);
        if from_key.is_empty() || to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let cs = self.case_sensitive();
        let mut tree = self.lock();
        let real_from = resolve(&tree, cs, &from_key).ok_or(Error::NotFound)?;
        let canon_to = canonical_key(&tree, cs, &to_key).ok_or(Error::NotFound)?;
        // Mover un dir DENTRO de sí mismo es imposible en cualquier FS (EINVAL).
        if canon_to.len() > real_from.len() && canon_to.starts_with(&real_from) {
            return Err(Error::InvalidPath);
        }
        // El destino puede "existir" solo como el propio origen con otra caja
        // (rename a→A en FS case-insensitive-preserving): permitido.
        if let Some(real_to) = resolve(&tree, cs, &canon_to)
            && real_to != real_from
        {
            return Err(Error::Conflict {
                conflict: collision_kind(&real_to, &canon_to),
            });
        }
        // Mueve el nodo y todo su subárbol.
        let moved: Vec<(SegPath, Node)> = tree
            .nodes
            .iter()
            .filter(|(k, _)| k.starts_with(&real_from))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, _) in &moved {
            tree.nodes.remove(k);
        }
        let mtime = tree.tick();
        for (k, mut node) in moved {
            let mut new_key = canon_to.clone();
            new_key.extend_from_slice(&k[real_from.len()..]);
            let (Node::Dir { mtime: m } | Node::File { mtime: m, .. }) = &mut node;
            *m = mtime;
            tree.nodes.insert(new_key, node);
        }
        Ok(())
    }

    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        if !self.caps.flags.contains(CapabilityFlags::SERVER_COPY) {
            return None;
        }
        Some(self.copy_native_inner(from, to).await)
    }
}

impl MemProvider {
    async fn copy_native_inner(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let from_key = seg_path(from);
        let to_key = seg_path(to);
        if to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let cs = self.case_sensitive();
        let mut tree = self.lock();
        let real_from = resolve(&tree, cs, &from_key).ok_or(Error::NotFound)?;
        let content = match tree.nodes.get(&real_from) {
            Some(Node::File { content, .. }) => content.clone(),
            // copy_native es de UN archivo; árboles los compone el core.
            Some(Node::Dir { .. }) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            None => return Err(Error::NotFound),
        };
        let canon_to = canonical_key(&tree, cs, &to_key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, cs, &canon_to) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon_to),
            });
        }
        let mtime = tree.tick();
        tree.nodes.insert(canon_to, Node::File { content, mtime });
        Ok(())
    }
}

struct MemSink {
    key: SegPath,
    buffer: Vec<u8>,
    fail_at: Option<usize>,
    written: usize,
    tree: Arc<Mutex<Tree>>,
    case_sensitive: bool,
}

#[async_trait]
impl ByteSink for MemSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        if let Some(n) = self.fail_at
            && self.written + chunk.len() >= n
        {
            let take = n.saturating_sub(self.written);
            self.buffer.extend_from_slice(&chunk[..take]);
            self.written += take;
            return Err(Error::Io { retryable: false });
        }
        self.written += chunk.len();
        self.buffer.extend_from_slice(&chunk);
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<(), Error> {
        // Invariante: nadie panica con el lock tomado.
        let mut tree = self.tree.lock().expect("tree lock sano");
        // Re-validación completa: entre write() y commit() pudo desaparecer
        // el padre (→ NotFound, jamás huérfanos) o aparecer una colisión.
        let canon = canonical_key(&tree, self.case_sensitive, &self.key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, self.case_sensitive, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        let mtime = tree.tick();
        tree.nodes.insert(
            canon,
            Node::File {
                content: self.buffer.clone(),
                mtime,
            },
        );
        Ok(())
    }

    async fn abort(self: Box<Self>) -> Result<(), Error> {
        // Nada llegó al árbol: soltar el buffer ES la limpieza.
        Ok(())
    }
}
