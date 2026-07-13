//! Índice en RAM de un archivo comprimido: árbol plano de entradas con
//! validación estructural de nombres y límites anti-bomba (ADR 0018 C2/D2).

use std::collections::{BTreeSet, HashMap};

use norte_proto::{Entry, EntryKind, Error, Segment, VPath};

/// Límites de construcción del índice (ADR 0018 D2). Constantes en v1;
/// configurables = issue. Overridables en tests vía
/// [`ArchiveProvider::with_limits`](crate::ArchiveProvider::with_limits).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Tope de entradas indexadas (las omitidas por hostiles no cuentan).
    pub max_entries: usize,
    /// Tope de bytes del nombre COMPLETO de una entrada.
    pub max_name_bytes: usize,
    /// Tope de componentes de path de una entrada.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 500_000,
            max_name_bytes: 4_096,
            max_depth: 64,
        }
    }
}

/// Dónde viven los bytes de una entrada dentro del contenedor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Locator {
    /// tar: los datos son CONTIGUOS y sin comprimir — `read` es un range
    /// passthrough al provider interior.
    Tar { offset: u64, size: u64 },
    /// zip: índice de la entrada en el central directory — `read`
    /// descomprime en un hilo blocking (stored/deflate).
    Zip { index: usize },
}

/// Un nodo del árbol virtual.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime_ms: Option<i64>,
    /// `None` en dirs, symlinks y entradas listables-pero-no-legibles
    /// (método de compresión no soportado, cifradas): `read` → `Unsupported`.
    pub locator: Option<Locator>,
    /// Target crudo de un symlink de tar.
    pub link_target: Option<Vec<u8>>,
}

impl Node {
    pub(crate) fn dir(mtime_ms: Option<i64>) -> Self {
        Self {
            kind: EntryKind::Dir,
            size: None,
            mtime_ms,
            locator: None,
            link_target: None,
        }
    }
}

/// Clave del árbol: los componentes del path interior, en bytes.
pub(crate) type InnerPath = Vec<Vec<u8>>;

/// Índice completo de UN contenedor, ligado a una generación del exterior.
pub(crate) struct ArchiveIndex {
    pub nodes: HashMap<InnerPath, Node>,
    /// dir interior → nombres de sus hijos directos (orden por bytes:
    /// determinista y O(log n) por inserción — un tar plano de 500k
    /// entradas no puede costar O(n²), hallazgo B1 de fase 8d).
    pub children: HashMap<InnerPath, BTreeSet<Vec<u8>>>,
    /// Entradas omitidas por nombre hostil/límites por-entrada (señal; el
    /// detalle va por `tracing::warn!`).
    pub skipped: u64,
    /// `(mtime_ms, size)` del contenedor al indexar — la invalidación.
    pub generation: (Option<i64>, Option<u64>),
}

impl ArchiveIndex {
    pub(crate) fn new(generation: (Option<i64>, Option<u64>)) -> Self {
        Self {
            nodes: HashMap::new(),
            children: HashMap::new(),
            skipped: 0,
            generation,
        }
    }

    /// Valida y trocea el nombre crudo de una entrada. `None` = hostil
    /// (con el motivo ya avisado por `warn!`).
    fn split_name(&mut self, raw: &[u8], limits: &Limits) -> Option<(InnerPath, bool)> {
        // debug! por entrada (un tar hostil trae MILLONES): el warn!
        // agregado con el total lo emite build_index al final.
        let mut hostile = |why: &str| {
            tracing::debug!(name = ?String::from_utf8_lossy(raw), why, "entrada omitida");
            self.skipped += 1;
            None
        };
        if raw.is_empty() {
            return hostile("nombre vacío");
        }
        if raw.len() > limits.max_name_bytes {
            return hostile("nombre demasiado largo");
        }
        if raw.first() == Some(&b'/') {
            return hostile("path absoluto");
        }
        let is_dir = raw.last() == Some(&b'/');
        let body = if is_dir { &raw[..raw.len() - 1] } else { raw };
        let mut parts: InnerPath = Vec::new();
        for comp in body.split(|&b| b == b'/') {
            if comp.is_empty() || comp == b"." || comp == b".." {
                return hostile("componente `.`/`..`/vacío (traversal)");
            }
            if comp == b"!" {
                return hostile("componente `!` (marcador ADR 0018, indireccionable)");
            }
            if Segment::new(comp.to_vec()).is_err() {
                return hostile("componente inválido como segmento VPath");
            }
            parts.push(comp.to_vec());
        }
        if parts.is_empty() {
            return hostile("nombre sin componentes");
        }
        if parts.len() > limits.max_depth {
            return hostile("profundidad excesiva");
        }
        Some((parts, is_dir))
    }

    fn add_child(&mut self, parent: InnerPath, name: Vec<u8>) {
        self.children.entry(parent).or_default().insert(name);
    }

    /// Materializa los ancestros de `path` como dirs implícitos. Un File
    /// preexistente en posición de ancestro ASCIENDE a dir (mismo criterio
    /// "gana dir" que las colisiones directas: sin esto, `file a` seguido
    /// de `a/hijo` dejaría el subárbol invisible — auditoría 8e, H4).
    fn ensure_parents(&mut self, path: &[Vec<u8>]) {
        for depth in 0..path.len().saturating_sub(1) {
            let dir: InnerPath = path[..=depth].to_vec();
            let node = self.nodes.entry(dir).or_insert_with(|| Node::dir(None));
            if node.kind != EntryKind::Dir {
                *node = Node::dir(node.mtime_ms);
                self.skipped += 1;
                tracing::debug!("file en posición de ancestro asciende a dir");
            }
            self.add_child(path[..depth].to_vec(), path[depth].clone());
        }
    }

    /// Inserta una entrada del contenedor. Nombres hostiles se omiten
    /// (skip+warn, ADR 0018 C2); superar `max_entries` corta el indexado.
    ///
    /// Reglas de colisión: última gana (semántica zip); un dir gana a un
    /// file en el mismo path (patrón de ataque conocido) y un dir jamás es
    /// degradado a file.
    pub(crate) fn insert_entry(
        &mut self,
        raw_name: &[u8],
        node: Node,
        limits: &Limits,
    ) -> Result<(), Error> {
        let Some((path, trailing_slash)) = self.split_name(raw_name, limits) else {
            return Ok(());
        };
        // `nombre/` manda sobre el kind declarado (zips reales lo hacen así).
        let node = if trailing_slash && node.kind != EntryKind::Dir {
            Node::dir(node.mtime_ms)
        } else {
            node
        };
        self.ensure_parents(&path);
        match self.nodes.get(&path) {
            Some(prev) if prev.kind == EntryKind::Dir && node.kind != EntryKind::Dir => {
                // Un dir (explícito o implícito con hijos) jamás degrada a
                // file: perderíamos el subárbol (patrón de ataque).
                tracing::debug!(
                    name = ?String::from_utf8_lossy(raw_name),
                    "entrada file colisiona con dir: gana el dir"
                );
                self.skipped += 1;
                return Ok(());
            }
            Some(_) => {
                tracing::debug!(
                    name = ?String::from_utf8_lossy(raw_name),
                    "entrada duplicada: última gana"
                );
            }
            None => {}
        }
        self.nodes.insert(path.clone(), node);
        let (parent, name) = (
            path[..path.len() - 1].to_vec(),
            path.last().expect("path no vacío").clone(),
        );
        self.add_child(parent, name);
        // Presupuesto DESPUÉS de insertar: los dirs implícitos también
        // cuentan (una bomba de paths profundos no se cuela por ahí). El
        // build entero se descarta al primer exceso.
        if self.nodes.len() > limits.max_entries {
            tracing::warn!(max = limits.max_entries, "índice supera max_entries");
            return Err(Error::Io { retryable: false });
        }
        Ok(())
    }

    /// El `Entry` wire de un nodo (o de la raíz sintética del contenedor).
    pub(crate) fn entry_for(&self, at: &VPath, inner: &[Vec<u8>]) -> Result<Entry, Error> {
        if inner.is_empty() {
            return Ok(Entry {
                path: at.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: self.generation.0,
            });
        }
        let node = self.nodes.get(inner).ok_or(Error::NotFound)?;
        Ok(Entry {
            path: at.clone(),
            kind: node.kind,
            size: node.size,
            mtime_ms: node.mtime_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_node() -> Node {
        Node {
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: Some(0),
            locator: Some(Locator::Tar { offset: 0, size: 1 }),
            link_target: None,
        }
    }

    fn idx() -> ArchiveIndex {
        ArchiveIndex::new((Some(0), Some(100)))
    }

    fn key(parts: &[&[u8]]) -> InnerPath {
        parts.iter().map(|p| p.to_vec()).collect()
    }

    #[test]
    fn inserta_y_materializa_padres() {
        let mut i = idx();
        i.insert_entry(b"a/b/c.txt", file_node(), &Limits::default())
            .expect("ok");
        assert_eq!(i.nodes[&key(&[b"a"])].kind, EntryKind::Dir);
        assert_eq!(i.nodes[&key(&[b"a", b"b"])].kind, EntryKind::Dir);
        assert_eq!(i.nodes[&key(&[b"a", b"b", b"c.txt"])].kind, EntryKind::File);
        assert!(i.children[&key(&[])].contains(b"a".as_slice()));
        assert!(i.children[&key(&[b"a", b"b"])].contains(b"c.txt".as_slice()));
        assert_eq!(i.skipped, 0);
    }

    #[test]
    fn omite_traversal_y_absolutos_y_marcador() {
        let mut i = idx();
        let l = Limits::default();
        for hostile in [
            b"../evil".as_slice(),
            b"/etc/passwd",
            b"a/../b",
            b"a//b",
            b"a/./b",
            b"",
            b"!",
            b"a/!/b",
            b"nul\x00byte",
            b"/",
            b"a//",
        ] {
            i.insert_entry(hostile, file_node(), &l)
                .expect("skip, no err");
        }
        assert!(i.nodes.is_empty(), "nada hostil entra al árbol");
        assert_eq!(i.skipped, 11);
    }

    #[test]
    fn backslash_y_bytes_crudos_se_preservan() {
        // `\` NO es separador (regla 1: bytes tal cual); cp437 crudo entra.
        let mut i = idx();
        let l = Limits::default();
        i.insert_entry(b"dir\\file", file_node(), &l).expect("ok");
        i.insert_entry(b"CAF\x82.TXT", file_node(), &l).expect("ok");
        assert!(i.nodes.contains_key(&key(&[b"dir\\file"])));
        assert!(i.nodes.contains_key(&key(&[b"CAF\x82.TXT"])));
        assert_eq!(i.skipped, 0);
    }

    #[test]
    fn duplicado_ultima_gana() {
        let mut i = idx();
        let l = Limits::default();
        i.insert_entry(b"x", file_node(), &l).expect("ok");
        let mut segundo = file_node();
        segundo.size = Some(99);
        i.insert_entry(b"x", segundo, &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"x"])].size, Some(99));
        assert_eq!(i.children[&key(&[])].len(), 1, "sin hijos duplicados");
    }

    #[test]
    fn dir_gana_a_file() {
        let mut i = idx();
        let l = Limits::default();
        // file primero, dir después: el dir lo reemplaza.
        i.insert_entry(b"a", file_node(), &l).expect("ok");
        i.insert_entry(b"a/", file_node(), &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"a"])].kind, EntryKind::Dir);
        // dir primero (implícito por hijo), file después: gana el dir.
        i.insert_entry(b"b/hijo", file_node(), &l).expect("ok");
        i.insert_entry(b"b", file_node(), &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"b"])].kind, EntryKind::Dir);
        assert!(i.nodes.contains_key(&key(&[b"b", b"hijo"])));
        // FILE primero, hijo después (H4): el file ASCIENDE a dir y el
        // subárbol es visible.
        i.insert_entry(b"c", file_node(), &l).expect("ok");
        i.insert_entry(b"c/hijo", file_node(), &l).expect("ok");
        assert_eq!(i.nodes[&key(&[b"c"])].kind, EntryKind::Dir);
        assert!(i.nodes.contains_key(&key(&[b"c", b"hijo"])));
        assert!(i.children[&key(&[b"c"])].contains(b"hijo".as_slice()));
    }

    #[test]
    fn max_entries_corta_con_io() {
        let mut i = idx();
        let l = Limits {
            max_entries: 2,
            ..Limits::default()
        };
        i.insert_entry(b"uno", file_node(), &l).expect("ok");
        i.insert_entry(b"dos", file_node(), &l).expect("ok");
        assert_eq!(
            i.insert_entry(b"tres", file_node(), &l).unwrap_err(),
            Error::Io { retryable: false }
        );
    }

    #[test]
    fn limites_por_entrada_omiten() {
        let mut i = idx();
        let l = Limits {
            max_name_bytes: 8,
            max_depth: 2,
            ..Limits::default()
        };
        i.insert_entry(b"nombre-larguisimo", file_node(), &l)
            .expect("skip");
        i.insert_entry(b"a/b/c", file_node(), &l).expect("skip");
        assert!(i.nodes.is_empty());
        assert_eq!(i.skipped, 2);
    }
}
