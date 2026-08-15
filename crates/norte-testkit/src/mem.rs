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
    File {
        content: Vec<u8>,
        mtime: i64,
        id: u64,
    },
    Dir {
        mtime: i64,
        id: u64,
    },
    Symlink {
        target: Vec<u8>,
        mtime: i64,
        id: u64,
        kind: norte_vfs::SymlinkKind,
    },
}

impl Node {
    /// Identidad del nodo (issue #16): asignada al crear, estable bajo
    /// rename (el nodo se mueve de clave, no se recrea).
    fn id(&self) -> u64 {
        match self {
            Node::File { id, .. } | Node::Dir { id, .. } | Node::Symlink { id, .. } => *id,
        }
    }
}

#[derive(Debug)]
struct Tree {
    /// Nodos por path de segmentos; la raíz es implícita (siempre Dir).
    nodes: BTreeMap<SegPath, Node>,
    /// Staging de resume por destino (ADR 0012): bytes CONSERVADOS por un
    /// `keep` que un `open_resumable` posterior reanuda. Se limpia en
    /// commit/abort.
    partials: BTreeMap<SegPath, Vec<u8>>,
    /// Reloj lógico: avanza 1 por mutación → mtimes deterministas.
    clock: i64,
    /// Siguiente identidad de nodo (0 es la raíz implícita).
    next_id: u64,
}

impl Default for Tree {
    fn default() -> Self {
        Self {
            nodes: BTreeMap::new(),
            partials: BTreeMap::new(),
            clock: 0,
            next_id: 1,
        }
    }
}

impl Tree {
    fn tick(&mut self) -> i64 {
        self.clock += 1;
        self.clock
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
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
/// - **La travesía de symlinks intermedios solo cubre LECTURAS**
///   (stat/list/read/`read_link`/`node_id`): las mutaciones
///   (write/mkdir/remove/rename/symlink) exigen ancestros Dir literales —
///   en POSIX real, mutar a través de un dir-symlink funciona. El copy
///   engine nunca muta vía paths a través de links (las creaciones van al
///   árbol destino real; de un link expandido se borra EL LINK), así que
///   el testkit no lo necesita todavía.
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
    norm: Normalization,
    /// `false` = simula un backend sin identidad estable (`node_id` = None).
    node_ids: bool,
    /// Valor fijo que devuelve `list_skipped` (#93): simula un provider
    /// archive que omitió entradas de su índice. `None` (default) = backend
    /// que lista todo lo que existe.
    list_skipped: Option<u64>,
    /// Papelera LÓGICA (#99, cierra deuda H2): con ella, `trash` mueve la
    /// víctima a `.norte-trash/<id>/payload` y devuelve `Some(payload)`
    /// (destino recuperable) en vez de la papelera "vanish" (`None`). Modela un
    /// provider remoto con `logical_trash` (sftp/object) para probar la
    /// idempotencia y la recuperación del `reversal_ref`.
    logical_trash: bool,
    /// Catálogo sintético de attrs (#108 bloque 2): vacío (default) = provider
    /// sin attrs; [`Self::with_synthetic_attrs`] lo puebla con valores
    /// deterministas y deliberadamente hostiles.
    attr_defs: Vec<norte_proto::AttrInfo>,
    /// Capabilities guionizadas POR UBICACIÓN (ADR 0054): simula un backend
    /// que sirve más de un filesystem tras un scheme — la raíz en ext4 y un
    /// `/usb` en exFAT, o un directorio ext4 en `+F`. Vacío (default) = todas
    /// las ubicaciones responden [`Self::capabilities`].
    caps_at: Arc<Mutex<BTreeMap<SegPath, Capabilities>>>,
    /// Rutas por las que alguien preguntó con `capabilities_at`, en orden.
    /// Costura de test: es la única forma de comprobar que un camino pregunta
    /// por la UBICACIÓN y no por el provider, cuando las dos respuestas
    /// coinciden.
    caps_at_asked: Arc<Mutex<Vec<SegPath>>>,
    tree: Arc<Mutex<Tree>>,
    faults: Arc<Faults>,
}

/// Eje de normalización Unicode del FS simulado (issue #7).
///
/// Límite documentado: con caja Y normalización insensibles a la vez, un
/// nombre que difiera en AMBAS cosas no se pliega (los ejes se evalúan por
/// separado; APFS real los combina). Suficiente para el testkit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Normalization {
    /// Bytes tal cual (ext4): NFC y NFD son nombres DISTINTOS.
    #[default]
    ByteExact,
    /// Insensible preservando bytes (APFS): NFC y NFD resuelven al mismo
    /// nodo; la colisión solo-por-normalización se etiqueta
    /// [`ConflictKind::Normalization`].
    Insensitive,
}

/// Config de resolución de nombres: los dos ejes juntos.
#[derive(Debug, Clone, Copy)]
struct Lookup {
    case_sensitive: bool,
    norm_insensitive: bool,
}

impl MemProvider {
    /// Provider con las capabilities por defecto de un FS "unix-like":
    /// `RENAME_ATOMIC | CASE_SENSITIVE | CASE_PRESERVING`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_flags(
            CapabilityFlags::RENAME_ATOMIC
                | CapabilityFlags::CASE_SENSITIVE
                | CapabilityFlags::CASE_PRESERVING
                | CapabilityFlags::SYMLINKS
                | CapabilityFlags::TRASH,
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
            norm: Normalization::default(),
            node_ids: true,
            list_skipped: None,
            logical_trash: false,
            attr_defs: Vec::new(),
            caps_at: Arc::new(Mutex::new(BTreeMap::new())),
            caps_at_asked: Arc::new(Mutex::new(Vec::new())),
            tree: Arc::new(Mutex::new(Tree::default())),
            faults: Arc::new(Faults::default()),
        }
    }

    /// Guioniza las capabilities de UNA ubicación (ADR 0054): a partir de aquí
    /// `capabilities_at(p)` responde `caps` en vez de la declaración del
    /// backend. Es la costura con la que se prueba un `+F` o un exFAT montado
    /// sin tener ninguno — ningún CI de este proyecto los tiene.
    ///
    /// Solo afecta a la ubicación EXACTA: un hijo suyo sigue respondiendo la
    /// declaración, porque el testkit no simula herencia por mount y fingirla
    /// escondería justo el fallo que #153 describe.
    ///
    /// # Panics
    ///
    /// Si el mutex del guion quedó envenenado por un panic previo — en un
    /// testkit eso ya es un test roto.
    pub fn set_caps_at(&self, p: &VPath, caps: Capabilities) {
        self.caps_at
            .lock()
            .expect("caps_at lock sano")
            .insert(seg_path(p), caps);
    }

    /// ¿Alguien preguntó por la ubicación `p` con `capabilities_at`?
    ///
    /// # Panics
    /// Si el mutex quedó envenenado por un panic previo — en un testkit eso ya
    /// es un test roto.
    #[must_use]
    pub fn was_asked_about(&self, p: &VPath) -> bool {
        self.caps_at_asked
            .lock()
            .expect("caps_at lock sano")
            .contains(&seg_path(p))
    }

    /// Olvida quién preguntó, para que un test pueda separar dos fases (lo que
    /// preguntó el plan de lo que pregunta el undo).
    ///
    /// # Panics
    /// Si el mutex quedó envenenado por un panic previo.
    pub fn forget_who_asked(&self) {
        self.caps_at_asked
            .lock()
            .expect("caps_at lock sano")
            .clear();
    }

    /// Atributos SINTÉTICOS deterministas (#108 bloque 2) con valores
    /// deliberadamente hostiles: dueño no-UTF-8 (`Bytes`), texto con RTL
    /// override + ZWJ, texto ANCHO (CJK + familia emoji ZWJ, #117
    /// encoding-audit L2). Para probar plumbing y render sin un provider
    /// real.
    #[must_use]
    pub fn with_synthetic_attrs(mut self) -> Self {
        use norte_proto::{AttrHint, AttrInfo, AttrType};
        let mk = |id: &str, label: &str, ty, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty,
            hint,
        };
        self.attr_defs = vec![
            mk("mem.owner", "Owner", AttrType::Bytes, AttrHint::Identity),
            mk("mem.note", "Note", AttrType::Text, AttrHint::Opaque),
            mk("mem.mode", "Mode", AttrType::Uint, AttrHint::Mode),
            mk("mem.stamp", "Stamp", AttrType::TimeMs, AttrHint::Timestamp),
            mk("mem.wide", "Wide", AttrType::Text, AttrHint::Opaque),
        ];
        self
    }

    /// Activa la papelera LÓGICA (#99): `trash` mueve a
    /// `.norte-trash/<id>/payload` y devuelve `Some(payload)` en vez de la
    /// papelera "vanish". Requiere la capability `TRASH` (la trae [`Self::new`]).
    #[must_use]
    pub fn with_logical_trash(mut self) -> Self {
        self.logical_trash = true;
        self
    }

    /// Simula un provider de CONTENEDOR que omitió `n` entradas de su índice
    /// (#93): `list_skipped` devuelve `Ok(Some(n))` para cualquier path. Para
    /// testear el plumbing daemon/Backend/frontends sin un archive real.
    #[must_use]
    pub fn with_list_skipped(mut self, n: u64) -> Self {
        self.list_skipped = Some(n);
        self
    }

    /// Simula un backend SIN identidad de nodo estable (object storage,
    /// ftp): `node_id` devuelve `Ok(None)` siempre. Para testear los
    /// caminos degradados del engine (heurísticas, Follow → Unsupported).
    #[must_use]
    pub fn without_node_ids(mut self) -> Self {
        self.node_ids = false;
        self
    }

    /// Inspección de test (issue #18): el kind ALMACENADO del symlink en
    /// `p`, ya resuelto si se creó con [`SymlinkKind`](norte_vfs::SymlinkKind)
    /// `::Unknown`. `None` si no existe o no es symlink. Los providers
    /// reales no exponen esto.
    #[must_use]
    pub fn symlink_kind_of(&self, p: &VPath) -> Option<norte_vfs::SymlinkKind> {
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        let real = resolve_traversing(&tree, lk, &key)?;
        match tree.nodes.get(&real) {
            Some(Node::Symlink { kind, .. }) => Some(*kind),
            _ => None,
        }
    }

    /// Fija el eje de normalización (default: [`Normalization::ByteExact`]).
    ///
    /// ```
    /// use norte_testkit::{MemProvider, Normalization};
    /// let apfs = MemProvider::new().with_normalization(Normalization::Insensitive);
    /// let _ = apfs;
    /// ```
    #[must_use]
    pub fn with_normalization(mut self, norm: Normalization) -> Self {
        self.norm = norm;
        self
    }

    /// Handle de inyección de fallos (compartible con el test mientras el
    /// provider está en uso).
    #[must_use]
    pub fn faults(&self) -> Arc<Faults> {
        Arc::clone(&self.faults)
    }

    /// Cuerpo compartido de `stat`/`stat_with` (#108 bloque 2): jamás
    /// delegar entre ellos vía los defaults del trait (recursión).
    async fn stat_inner(&self, p: &VPath, req: &norte_vfs::AttrRequest) -> Result<Entry, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        if key.is_empty() {
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: p.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: Some(0),
            });
        }
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        let node = tree.nodes.get(&real).ok_or(Error::NotFound)?;
        Ok(entry_for(p, &real, node, &self.attr_defs, req))
    }

    /// Cuerpo compartido de `list`/`list_with` (#108 bloque 2).
    async fn list_inner(
        &self,
        p: &VPath,
        req: &norte_vfs::AttrRequest,
    ) -> Result<EntryStream, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if self.faults.list_fails_for(&key) {
            return Err(Error::Io { retryable: true });
        }
        let lk = self.lookup();
        let tree = self.lock();
        // El filtro de hijos usa la clave REAL: listar con otra caja debe
        // ver lo mismo que stat (coherencia con el FS simulado). Como un
        // opendir de verdad, la travesía sigue symlinks intermedios Y el
        // link final.
        let real = if key.is_empty() {
            key
        } else {
            let mut real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
            if let Some(Node::Symlink { target, .. }) = tree.nodes.get(&real) {
                real = resolve_symlink(&tree, lk, &real, target).map_err(|_| Error::NotFound)?;
            }
            if !matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            real
        };
        // Los paths de los hijos cuelgan del path PEDIDO (con el nombre
        // REAL de la hoja): listar a través de un link debe dar paths
        // utilizables bajo ese link, como en un FS real.
        let entries: Vec<Result<Entry, Error>> = tree
            .nodes
            .iter()
            .filter(|(k, _)| k.len() == real.len() + 1 && k.starts_with(&real))
            .map(|(k, node)| {
                let name = k.last().expect("clave de hijo no vacía").clone();
                let seg = norte_proto::Segment::new(name).expect("clave del árbol ya validada");
                Ok(entry_for_child(p.join(seg), node, &self.attr_defs, req))
            })
            .collect();
        Ok(futures::stream::iter(entries).boxed())
    }

    /// La raíz de este provider: `mem:///`.
    ///
    /// # Panics
    /// Nunca: el scheme es constante y válido.
    #[must_use]
    pub fn root() -> VPath {
        VPath::root(Scheme::new("mem").expect("scheme constante válido"), None)
    }

    fn lookup(&self) -> Lookup {
        Lookup {
            case_sensitive: self.caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
            norm_insensitive: self.norm == Normalization::Insensitive,
        }
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

/// La `.norte-info` en `info_key` decodifica exactamente a `p` (misma
/// víctima): la entrada de papelera es NUESTRA, no una colisión ajena con el
/// mismo id (#99, review rust MAJOR). `info_decode` ya exige mismo
/// scheme+authority; aquí se compara la ruta original completa.
fn info_matches(tree: &Tree, info_key: &SegPath, p: &VPath) -> bool {
    matches!(
        tree.nodes.get(info_key),
        Some(Node::File { content, .. })
            if norte_vfs::trash::info_decode(content, p)
                .is_ok_and(|i| i.original == *p)
    )
}

/// ¿Misma forma NFC segmento a segmento? Solo si ambos son UTF-8 válido.
fn nfc_eq_path(a: &SegPath, b: &SegPath) -> bool {
    use unicode_normalization::UnicodeNormalization;
    a.len() == b.len()
        && a.iter().zip(b).all(
            |(x, y)| match (std::str::from_utf8(x), std::str::from_utf8(y)) {
                (Ok(x), Ok(y)) => x.nfc().eq(y.nfc()),
                _ => x == y,
            },
        )
}

/// Resuelve `key` contra el árbol según los ejes de caja y normalización.
/// Devuelve la clave REAL almacenada (puede diferir en caja o en forma).
fn resolve(tree: &Tree, lk: Lookup, key: &SegPath) -> Option<SegPath> {
    if tree.nodes.contains_key(key) {
        return Some(key.clone());
    }
    if !lk.case_sensitive
        && let Some(k) = tree.nodes.keys().find(|k| fold_eq_path(k, key))
    {
        return Some(k.clone());
    }
    if lk.norm_insensitive {
        return tree.nodes.keys().find(|k| nfc_eq_path(k, key)).cloned();
    }
    None
}

/// Resolución de un path completo con TRAVESÍA de symlinks en los
/// componentes intermedios (semántica POSIX: un FS real resuelve
/// `link/hijo` a través del link). Un nivel de link por componente — las
/// cadenas link→link dan `None`, mismo límite documentado que
/// [`resolve_symlink`]. La HOJA no se sigue (semántica lstat, como `stat`).
fn resolve_traversing(tree: &Tree, lk: Lookup, key: &SegPath) -> Option<SegPath> {
    // Atajo: la clave exacta existe (el caso abrumadoramente común).
    if tree.nodes.contains_key(key) {
        return Some(key.clone());
    }
    let mut canon: SegPath = Vec::new();
    for (i, seg) in key.iter().enumerate() {
        let mut probe = canon.clone();
        probe.push(seg.clone());
        let real = resolve(tree, lk, &probe)?;
        if i < key.len() - 1
            && let Some(Node::Symlink { target, .. }) = tree.nodes.get(&real)
        {
            let resolved = resolve_symlink(tree, lk, &real, target).ok()?;
            if !matches!(tree.nodes.get(&resolved), Some(Node::Dir { .. })) {
                return None;
            }
            canon = resolved;
        } else {
            canon = real;
        }
    }
    Some(canon)
}

/// Canonicaliza `key` respecto a la caja ALMACENADA: cada ancestro adopta la
/// caja real de su Dir (y debe existir como Dir); la hoja conserva la caja
/// pedida (case-preserving). Sin esto, una inserción vía caja distinta crearía
/// huérfanos invisibles para `list` — imposible en un FS real.
///
/// `None` si algún ancestro falta o no es Dir.
fn canonical_key(tree: &Tree, lk: Lookup, key: &SegPath) -> Option<SegPath> {
    let mut canon: SegPath = Vec::with_capacity(key.len());
    for (i, seg) in key.iter().enumerate() {
        if i == key.len() - 1 {
            canon.push(seg.clone());
        } else {
            let mut probe = canon.clone();
            probe.push(seg.clone());
            let real = resolve(tree, lk, &probe)?;
            if !matches!(tree.nodes.get(&real), Some(Node::Dir { .. })) {
                return None;
            }
            canon = real;
        }
    }
    Some(canon)
}

/// Resolución mínima de un symlink de Mem: `target` relativo al PADRE del
/// link, segmentos separados por `/`. Sin `..`, sin absolutos, y las CADENAS
/// symlink→symlink dan `NotFound` (un FS real las seguiría): con eso basta
/// para el testkit — los targets exóticos se testean en el provider real.
fn resolve_symlink(
    tree: &Tree,
    lk: Lookup,
    link: &SegPath,
    target: &[u8],
) -> Result<SegPath, Error> {
    if target.starts_with(b"/") || target.split(|b| *b == b'/').any(|s| s == b"..") {
        return Err(Error::Unsupported);
    }
    let mut key: SegPath = link[..link.len() - 1].to_vec();
    for seg in target.split(|b| *b == b'/').filter(|s| !s.is_empty()) {
        key.push(seg.to_vec());
    }
    resolve(tree, lk, &key).ok_or(Error::NotFound)
}

/// Clasifica una colisión: byte-exacta = `Exists`; misma forma NFC =
/// `Normalization` (issue #8); si no, variante de caja = `CaseCollision`.
fn collision_kind(real: &SegPath, requested: &SegPath) -> ConflictKind {
    if real == requested {
        ConflictKind::Exists
    } else if nfc_eq_path(real, requested) {
        ConflictKind::Normalization
    } else {
        ConflictKind::CaseCollision
    }
}

/// Valores sintéticos DETERMINISTAS por nodo (#108 bloque 2): función pura
/// de (catálogo, petición, kind, mtime). Los valores son deliberadamente
/// hostiles — el masking es problema de los frontends, no del provider.
fn synthetic_attrs(
    defs: &[norte_proto::AttrInfo],
    req: &norte_vfs::AttrRequest,
    node: &Node,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    if defs.is_empty() || req.is_empty() {
        return out;
    }
    let quiere = |id: &str| defs.iter().any(|d| d.id == id) && req.wants(id);
    if quiere("mem.owner") {
        // Dueño NO-UTF-8: bytes crudos, jamás String (regla 1).
        out.insert(
            "mem.owner".to_owned(),
            AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()),
        );
    }
    if quiere("mem.note") {
        // RTL override + ZWJ: humo para el masking de los frontends.
        out.insert(
            "mem.note".to_owned(),
            AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".to_owned()),
        );
    }
    if quiere("mem.wide") {
        // Texto ANCHO (#117 encoding-audit L2): CJK double-width + la
        // MISMA familia emoji ZWJ del corpus (`emoji_zwj_family`, fuente
        // única) — grapheme multi-codepoint para pinear que una celda
        // ancha jamás desplaza la columna vecina en los frontends.
        let familia = crate::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "emoji_zwj_family")
            // El corpus embebido siempre trae la fixture (UTF-8 puro);
            // si algún día se renombrara, el valor queda solo-CJK y los
            // pins de anchura de los frontends lo delatarían.
            .and_then(|n| String::from_utf8(n.bytes).ok())
            .unwrap_or_default();
        out.insert(
            "mem.wide".to_owned(),
            AttrValue::Text(format!("日本語{familia}")),
        );
    }
    if quiere("mem.mode") {
        let mode = match node {
            Node::Dir { .. } => 0o040_755,
            Node::File { .. } => 0o100_644,
            Node::Symlink { .. } => 0o120_777,
        };
        out.insert("mem.mode".to_owned(), AttrValue::Uint(mode));
    }
    if quiere("mem.stamp") {
        let mtime = match node {
            Node::File { mtime, .. } | Node::Dir { mtime, .. } | Node::Symlink { mtime, .. } => {
                *mtime
            }
        };
        out.insert("mem.stamp".to_owned(), AttrValue::TimeMs(mtime));
    }
    out
}

/// [`Entry`] de un hijo con path YA construido (listados: el padre es el
/// path PEDIDO, no la clave canónica — ver `list`).
fn entry_for_child(
    path: VPath,
    node: &Node,
    defs: &[norte_proto::AttrInfo],
    req: &norte_vfs::AttrRequest,
) -> Entry {
    let attrs = synthetic_attrs(defs, req, node);
    match node {
        Node::File { content, mtime, .. } => Entry {
            attrs,
            path,
            kind: EntryKind::File,
            size: Some(content.len() as u64),
            mtime_ms: Some(*mtime),
        },
        Node::Dir { mtime, .. } => Entry {
            attrs,
            path,
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: Some(*mtime),
        },
        Node::Symlink { mtime, .. } => Entry {
            attrs,
            path,
            kind: EntryKind::Symlink,
            size: None,
            mtime_ms: Some(*mtime),
        },
    }
}

/// Reconstruye la [`Entry`] de una clave del árbol sobre el scheme y la
/// authority de `base` (la authority se preserva: la identidad del path en el
/// wire no puede cambiar por pasar por el provider).
fn entry_for(
    base: &VPath,
    key: &SegPath,
    node: &Node,
    defs: &[norte_proto::AttrInfo],
    req: &norte_vfs::AttrRequest,
) -> Entry {
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
    entry_for_child(p, node, defs, req)
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

    async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
        let key = seg_path(p);
        self.caps_at_asked
            .lock()
            .expect("caps_at lock sano")
            .push(key.clone());
        Ok(self
            .caps_at
            .lock()
            .expect("caps_at lock sano")
            .get(&key)
            .copied()
            .unwrap_or(self.caps))
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.stat_inner(p, &norte_vfs::AttrRequest::default()).await
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        self.stat_inner(p, &opt.attrs).await
    }

    async fn node_id(
        &self,
        p: &VPath,
        follow: norte_vfs::FollowLinks,
    ) -> Result<Option<norte_vfs::NodeId>, Error> {
        if !self.node_ids {
            return Ok(None);
        }
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        // La raíz implícita tiene la identidad reservada 0.
        if key.is_empty() {
            return Ok(Some(norte_vfs::NodeId {
                volume: 0,
                index: 0,
            }));
        }
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        let node = tree.nodes.get(&real).ok_or(Error::NotFound)?;
        let node = match (follow, node) {
            (norte_vfs::FollowLinks::Yes, Node::Symlink { target, .. }) => {
                let resolved = resolve_symlink(&tree, lk, &real, target)?;
                match tree.nodes.get(&resolved) {
                    // Cadena link→link: coherente con read() — NotFound
                    // (la resolución mínima de Mem no sigue cadenas).
                    Some(Node::Symlink { .. }) | None => return Err(Error::NotFound),
                    Some(n) => n,
                }
            }
            _ => node,
        };
        Ok(Some(norte_vfs::NodeId {
            volume: 0,
            index: u128::from(node.id()),
        }))
    }

    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        let _ = p;
        Ok(self.list_skipped)
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.list_inner(p, &norte_vfs::AttrRequest::default()).await
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        self.list_inner(p, &opt.attrs).await
    }

    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        &self.attr_defs
    }

    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<ByteStream, Error> {
        self.faults.op_gate().await?;
        self.faults.count_read();
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        let content = match tree.nodes.get(&real) {
            Some(Node::File { content, .. }) => content.clone(),
            // Como un FS real: read() SIGUE el symlink. Resolución mínima
            // (target relativo al padre del link, separado por '/', sin
            // `..`): suficiente para testear la política Follow del engine.
            Some(Node::Symlink { target, .. }) => {
                let resolved = resolve_symlink(&tree, lk, &real, target)?;
                match tree.nodes.get(&resolved) {
                    Some(Node::File { content, .. }) => content.clone(),
                    Some(Node::Dir { .. }) => {
                        return Err(Error::Conflict {
                            conflict: ConflictKind::TypeMismatch,
                        });
                    }
                    _ => return Err(Error::NotFound),
                }
            }
            Some(Node::Dir { .. }) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            None => return Err(Error::NotFound),
        };
        drop(tree);

        // Rango (ADR 0005): pread — offset pasado de EOF = vacío, len se
        // recorta a EOF. El fallo inyectado cuenta bytes DEL STREAM.
        let content: Vec<u8> = match range {
            None => content,
            Some(r) => {
                let start =
                    usize::try_from(r.offset.min(content.len() as u64)).unwrap_or(content.len());
                let end = match r.len {
                    None => content.len(),
                    Some(l) => start.saturating_add(usize::try_from(l).unwrap_or(usize::MAX)),
                }
                .min(content.len());
                content[start..end].to_vec()
            }
        };

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
        let lk = self.lookup();
        let tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon) {
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
            lookup: lk,
            faults: Arc::clone(&self.faults),
        }))
    }

    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        use sha2::{Digest, Sha256};
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        // Sin staging = sin digest (el engine degrada a Length). Con staging,
        // hashea EXACTAMENTE los primeros `len` bytes (`len` jamás excede lo
        // que open_resumable reportó, así que el slice es válido).
        let Some(buffer) = tree.partials.get(&canon) else {
            return Ok(None);
        };
        let n = usize::try_from(len)
            .unwrap_or(buffer.len())
            .min(buffer.len());
        let digest = Sha256::digest(&buffer[..n]);
        Ok(Some(digest.into()))
    }

    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        // El destino final debe seguir sin existir (mismo contrato que write).
        if let Some(real) = resolve(&tree, lk, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        // Reanuda desde el staging conservado, si lo hay.
        let buffer = tree.partials.get(&canon).cloned().unwrap_or_default();
        let already = buffer.len() as u64;
        drop(tree);
        Ok((
            Box::new(MemSink {
                key: canon,
                buffer,
                fail_at: self.faults.write_fault_for(&key),
                written: 0,
                tree: Arc::clone(&self.tree),
                lookup: lk,
                faults: Arc::clone(&self.faults),
            }),
            already,
        ))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.nodes.insert(canon, Node::Dir { mtime, id });
        drop(tree);
        self.ambiguous_gate()
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        if key.is_empty() {
            return Err(Error::Unsupported);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real = resolve(&tree, lk, &key).ok_or(Error::NotFound)?;
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
        drop(tree);
        self.ambiguous_gate()
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let from_key = seg_path(from);
        let to_key = seg_path(to);
        // ANTES de tocar el árbol: un fallo inyectado no aplica su efecto.
        if self.faults.rename_fails_from(&from_key) {
            return Err(Error::Io { retryable: false });
        }
        if from_key.is_empty() || to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real_from = resolve(&tree, lk, &from_key).ok_or(Error::NotFound)?;
        let canon_to = canonical_key(&tree, lk, &to_key).ok_or(Error::NotFound)?;
        // Mover un dir DENTRO de sí mismo es imposible en cualquier FS (EINVAL).
        if canon_to.len() > real_from.len() && canon_to.starts_with(&real_from) {
            return Err(Error::InvalidPath);
        }
        // El destino puede "existir" solo como el propio origen con otra caja
        // (rename a→A en FS case-insensitive-preserving): permitido.
        if let Some(real_to) = resolve(&tree, lk, &canon_to)
            && real_to != real_from
        {
            if !self.faults.renames_clobber() {
                return Err(Error::Conflict {
                    conflict: collision_kind(&real_to, &canon_to),
                });
            }
            // Provider que PISA (fallo inyectado): el destino y su subárbol
            // desaparecen, como haría un posix-rename.
            let victims: Vec<SegPath> = tree
                .nodes
                .keys()
                .filter(|k| k.starts_with(&real_to))
                .cloned()
                .collect();
            for k in victims {
                tree.nodes.remove(&k);
            }
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
            let (Node::Dir { mtime: m, .. }
            | Node::File { mtime: m, .. }
            | Node::Symlink { mtime: m, .. }) = &mut node;
            *m = mtime;
            // El id viaja DENTRO del nodo: rename preserva identidad.
            tree.nodes.insert(new_key, node);
        }
        drop(tree);
        self.ambiguous_gate()
    }

    async fn trash(
        &self,
        p: &VPath,
        id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        if !self.caps.flags.contains(CapabilityFlags::TRASH) {
            return Err(Error::Unsupported);
        }
        self.faults.op_gate().await?;
        let key = seg_path(p);
        // La raíz no se trashea: el path es el problema (como local).
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        if self.logical_trash {
            return self.trash_logical(p, id);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real = resolve(&tree, lk, &key).ok_or(Error::NotFound)?;
        // Papelera lógica del testkit: el subárbol desaparece de la vista
        // (list/restore de verdad = M3 sobre el provider real).
        let victims: Vec<SegPath> = tree
            .nodes
            .keys()
            .filter(|k| k.starts_with(&real))
            .cloned()
            .collect();
        for k in victims {
            tree.nodes.remove(&k);
        }
        tree.tick();
        drop(tree);
        // Papelera "vanish" de test: el subárbol desaparece de la vista, sin
        // destino recuperable expuesto (como la papelera nativa del OS). El
        // `ambiguous_gate` simula el transitorio-tras-efecto (#17/#99): el
        // reintento verá la víctima ausente y `trash_retrying` degrada a
        // `Ok(None)` (sin `reversal_ref`, como la papelera nativa).
        self.ambiguous_gate()?;
        Ok(None)
    }

    /// Solo la papelera LÓGICA del testkit nombra su destino; la "vanish"
    /// imita a la nativa de macOS/Windows y no promete nada.
    fn trash_restorable(&self) -> bool {
        self.logical_trash
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.faults.op_gate().await?;
        let key = seg_path(p);
        let lk = self.lookup();
        let tree = self.lock();
        let real = resolve_traversing(&tree, lk, &key).ok_or(Error::NotFound)?;
        match tree.nodes.get(&real) {
            Some(Node::Symlink { target, .. }) => Ok(target.clone()),
            Some(_) => Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            }),
            None => Err(Error::NotFound),
        }
    }

    async fn symlink(
        &self,
        link: &VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.faults.op_gate().await?;
        let key = seg_path(link);
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let canon = canonical_key(&tree, lk, &key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        // `Unknown` (issue #18): el provider resuelve el kind contra SU
        // árbol, best-effort — roto o irresoluble degrada a File.
        let kind = match kind {
            norte_vfs::SymlinkKind::Unknown => match resolve_symlink(&tree, lk, &canon, target) {
                Ok(resolved) => match tree.nodes.get(&resolved) {
                    Some(Node::Dir { .. }) => norte_vfs::SymlinkKind::Dir,
                    _ => norte_vfs::SymlinkKind::File,
                },
                Err(_) => norte_vfs::SymlinkKind::File,
            },
            explicit => explicit,
        };
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.nodes.insert(
            canon,
            Node::Symlink {
                target: target.to_vec(),
                mtime,
                id,
                kind,
            },
        );
        drop(tree);
        self.ambiguous_gate()
    }

    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        if !self.caps.flags.contains(CapabilityFlags::SERVER_COPY) {
            return None;
        }
        Some(self.copy_native_inner(from, to).await)
    }
}

impl MemProvider {
    /// Puerta de salida de cada mutación puntual: si hay una carga de
    /// [`Faults::ambiguous_mutations`] armada, el efecto YA se aplicó y aun
    /// así se devuelve error transitorio (issue #17).
    fn ambiguous_gate(&self) -> Result<(), Error> {
        if self.faults.take_ambiguous() {
            Err(Error::ProviderUnavailable { retryable: true })
        } else {
            Ok(())
        }
    }

    /// Papelera LÓGICA (#99): mueve la víctima a `.norte-trash/<id>/payload` y
    /// devuelve el destino recuperable. El `id` determinista la hace
    /// IDEMPOTENTE — si la víctima ya no está pero el payload sí, esta op ya
    /// aplicó en un intento transitorio anterior → `Some(payload)` (sin víctima
    /// ni payload = `NotFound` genuino). Dir markers e info se insertan
    /// directamente (sin consumir faults); el movimiento reusa la re-clave de
    /// `rename`; un único [`Self::ambiguous_gate`] al final simula el
    /// transitorio-tras-efecto que `trash_retrying` recupera.
    fn trash_logical(
        &self,
        p: &VPath,
        id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        let paths = norte_vfs::trash::plan(p, &id.as_segment())?;
        let trash_root = paths.dir.parent().ok_or(Error::Unsupported)?;
        let victim_key = seg_path(p);
        let dir_key = seg_path(&paths.dir);
        let root_key = seg_path(&trash_root);
        let info_key = seg_path(&paths.info);
        let payload_key = seg_path(&paths.payload);
        let lk = self.lookup();
        {
            let mut tree = self.lock();
            let Some(real_from) = resolve(&tree, lk, &victim_key) else {
                // Víctima ausente. Idempotencia: payload presente = ya aplicó,
                // pero SOLO si la `.norte-info` de la entrada es NUESTRA (misma
                // víctima). Una entrada AJENA con el mismo id no se reclama
                // (review rust MAJOR): se reporta colisión, no un payload que
                // no es de `p`. Sin payload = `NotFound` genuino.
                if resolve(&tree, lk, &payload_key).is_none() {
                    return Err(Error::NotFound);
                }
                return if info_matches(&tree, &info_key, p) {
                    Ok(Some(paths.payload))
                } else {
                    Err(Error::Conflict {
                        conflict: ConflictKind::Exists,
                    })
                };
            };
            // La entrada `<id>` ya existe con una info AJENA (otra víctima, mismo
            // id): colisión real — no se pisa su `.norte-info` ni se mezcla el
            // árbol. Ausente o nuestra = seguimos (nuestro parcial).
            if tree.nodes.contains_key(&info_key) && !info_matches(&tree, &info_key, p) {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            let mtime = tree.tick();
            // Markers `.norte-trash` y `.norte-trash/<id>` (idempotentes).
            for dkey in [&root_key, &dir_key] {
                if !tree.nodes.contains_key(dkey) {
                    let id = tree.new_id();
                    tree.nodes.insert(dkey.clone(), Node::Dir { mtime, id });
                }
            }
            // `.norte-info` (sobrescribir un parcial es benigno).
            let content = norte_vfs::trash::info_encode(p, id.deleted_ms());
            let info_id = tree.new_id();
            tree.nodes.insert(
                info_key,
                Node::File {
                    content,
                    mtime,
                    id: info_id,
                },
            );
            // Mueve el subárbol víctima → payload preservando identidad.
            let moved: Vec<(SegPath, Node)> = tree
                .nodes
                .iter()
                .filter(|(k, _)| k.starts_with(&real_from))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, _) in &moved {
                tree.nodes.remove(k);
            }
            for (k, mut node) in moved {
                let mut new_key = payload_key.clone();
                new_key.extend_from_slice(&k[real_from.len()..]);
                let (Node::Dir { mtime: m, .. }
                | Node::File { mtime: m, .. }
                | Node::Symlink { mtime: m, .. }) = &mut node;
                *m = mtime;
                tree.nodes.insert(new_key, node);
            }
        }
        // El movimiento YA se aplicó; el transitorio llega DESPUÉS (#17).
        self.ambiguous_gate()?;
        Ok(Some(paths.payload))
    }

    async fn copy_native_inner(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.faults.op_gate().await?;
        // Gate específico (#51): simula el multipart copy largo de S3 — se
        // queda pendiente hasta que el test lo suelte o el caller cancele.
        self.faults.copy_native_gate().await;
        let from_key = seg_path(from);
        let to_key = seg_path(to);
        if to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        let lk = self.lookup();
        let mut tree = self.lock();
        let real_from = resolve(&tree, lk, &from_key).ok_or(Error::NotFound)?;
        let content = match tree.nodes.get(&real_from) {
            Some(Node::File { content, .. }) => content.clone(),
            // copy_native es de UN archivo; árboles (y symlinks, que tienen
            // política propia) los compone el core.
            Some(Node::Dir { .. } | Node::Symlink { .. }) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            None => return Err(Error::NotFound),
        };
        let canon_to = canonical_key(&tree, lk, &to_key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, lk, &canon_to) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon_to),
            });
        }
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.nodes
            .insert(canon_to, Node::File { content, mtime, id });
        Ok(())
    }
}

struct MemSink {
    key: SegPath,
    buffer: Vec<u8>,
    fail_at: Option<usize>,
    written: usize,
    tree: Arc<Mutex<Tree>>,
    lookup: Lookup,
    faults: Arc<Faults>,
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
        let canon = canonical_key(&tree, self.lookup, &self.key).ok_or(Error::NotFound)?;
        if let Some(real) = resolve(&tree, self.lookup, &canon) {
            return Err(Error::Conflict {
                conflict: collision_kind(&real, &canon),
            });
        }
        let mtime = tree.tick();
        let id = tree.new_id();
        tree.partials.remove(&canon);
        tree.nodes.insert(
            canon,
            Node::File {
                content: self.buffer.clone(),
                mtime,
                id,
            },
        );
        drop(tree);
        // El commit YA aplicó (rename staging→final): si hay una carga
        // ambigua armada, devuelve transitorio DESPUÉS del efecto (#32.1) —
        // el "timeout tras rename" de un provider remoto.
        if self.faults.take_ambiguous() {
            return Err(Error::ProviderUnavailable { retryable: true });
        }
        Ok(())
    }

    async fn abort(self: Box<Self>) -> Result<(), Error> {
        // Descarta también el staging conservado (si lo había).
        self.tree
            .lock()
            .expect("tree lock sano")
            .partials
            .remove(&self.key);
        Ok(())
    }

    async fn keep(self: Box<Self>) -> Result<(), Error> {
        // Conserva los bytes para un open_resumable posterior (ADR 0012).
        self.tree
            .lock()
            .expect("tree lock sano")
            .partials
            .insert(self.key.clone(), self.buffer.clone());
        Ok(())
    }
}
