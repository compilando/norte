//! Guest WASM (#30 stage 2b-write, ADR 0032): un provider ESCRIBIBLE sobre un
//! FS plano en memoria (thread-local, mutable) — dogfood del camino de
//! escritura de la interfaz WIT `provider`.
//!
//! El `writer` es transaccional: los bytes se acumulan en un staging propio del
//! recurso y el path final NO existe hasta `commit`. `abort`/soltar sin commit
//! no publican nada. Nombres en bytes crudos (regla 1).

use std::cell::RefCell;
use std::collections::HashMap;

wit_bindgen::generate!({
    world: "norte-provider",
    path: "wit",
});

use exports::norte::plugin::provider::{
    Caps, Entry, EntryKind, Guest, GuestWriter, Page, ProviderConfig, VfsError, Writer,
};

/// Path = sus segmentos (bytes crudos). La raíz es la lista vacía.
type PathSegs = Vec<Vec<u8>>;

/// Un nodo del FS plano.
#[derive(Clone)]
enum Node {
    File(Vec<u8>),
    Dir,
}

thread_local! {
    /// El FS: mapa PLANO de path (segmentos) → nodo. La raíz (`[]`) es un dir
    /// implícito, no una clave.
    static FS: RefCell<HashMap<PathSegs, Node>> = RefCell::new(seed());
}

/// Árbol canónico inicial (mismo que provider-mem, sin los nombres no-VPath).
fn seed() -> HashMap<PathSegs, Node> {
    let mut fs = HashMap::new();
    let d = |s: &[&[u8]]| s.iter().map(|x| x.to_vec()).collect::<Vec<_>>();
    fs.insert(d(&[b"docs"]), Node::Dir);
    fs.insert(
        d(&[b"docs", b"hello.txt"]),
        Node::File(b"hola norte\n".to_vec()),
    );
    fs.insert(d(&[b"docs", b"sub"]), Node::Dir);
    fs.insert(
        d(&[b"docs", b"sub", b"nested.bin"]),
        Node::File(vec![0x00, 0x01, 0x02, 0xff]),
    );
    fs.insert(d(&[b"vacio.txt"]), Node::File(Vec::new()));
    fs
}

/// `true` si `child` es hijo DIRECTO de `parent` (un segmento más, mismo prefijo).
fn is_direct_child(parent: &[Vec<u8>], child: &[Vec<u8>]) -> bool {
    child.len() == parent.len() + 1 && child.starts_with(parent)
}

/// `true` si `p` es un directorio existente (o la raíz).
fn is_dir(fs: &HashMap<PathSegs, Node>, p: &[Vec<u8>]) -> bool {
    p.is_empty() || matches!(fs.get(p), Some(Node::Dir))
}

struct Mem;

impl Guest for Mem {
    fn configure(_cfg: ProviderConfig) -> Result<(), VfsError> {
        // El provider en memoria no establece conexión: no-op.
        Ok(())
    }

    fn capabilities() -> Caps {
        Caps { read_only: false }
    }

    fn stat(p: PathSegs) -> Result<Entry, VfsError> {
        FS.with_borrow(|fs| {
            let name = p.last().cloned().unwrap_or_default();
            if p.is_empty() {
                return Ok(Entry {
                    name,
                    kind: EntryKind::Dir,
                    size: None,
                });
            }
            match fs.get(&p) {
                Some(Node::File(d)) => Ok(Entry {
                    name,
                    kind: EntryKind::File,
                    size: Some(d.len() as u64),
                }),
                Some(Node::Dir) => Ok(Entry {
                    name,
                    kind: EntryKind::Dir,
                    size: None,
                }),
                None => Err(VfsError::NotFound),
            }
        })
    }

    fn list_dir(p: PathSegs, _cursor: Option<Vec<u8>>) -> Result<Page, VfsError> {
        FS.with_borrow(|fs| {
            if !is_dir(fs, &p) {
                return if fs.contains_key(&p) {
                    Err(VfsError::Unsupported) // listar un fichero
                } else {
                    Err(VfsError::NotFound)
                };
            }
            // Una sola página (el árbol de test es pequeño).
            let entries = fs
                .iter()
                .filter(|(k, _)| is_direct_child(&p, k))
                .map(|(k, node)| Entry {
                    name: k.last().cloned().unwrap_or_default(),
                    kind: match node {
                        Node::File(_) => EntryKind::File,
                        Node::Dir => EntryKind::Dir,
                    },
                    size: match node {
                        Node::File(d) => Some(d.len() as u64),
                        Node::Dir => None,
                    },
                })
                .collect();
            Ok(Page {
                entries,
                next_cursor: None,
            })
        })
    }

    fn read(p: PathSegs, offset: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        FS.with_borrow(|fs| match fs.get(&p) {
            Some(Node::File(data)) => {
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(data.len());
                let want = usize::try_from(len).unwrap_or(usize::MAX);
                let end = start.saturating_add(want).min(data.len());
                Ok(data[start..end].to_vec())
            }
            Some(Node::Dir) => Err(VfsError::Unsupported),
            None => Err(VfsError::NotFound),
        })
    }

    // ---- escritura ----

    type Writer = MemWriter;

    fn open_writer(p: PathSegs) -> Result<Writer, VfsError> {
        // El padre debe existir y ser un dir; el path final no puede ser un dir.
        FS.with_borrow(|fs| {
            if matches!(fs.get(&p), Some(Node::Dir)) {
                return Err(VfsError::Conflict);
            }
            let parent = &p[..p.len().saturating_sub(1)];
            if !is_dir(fs, parent) {
                return Err(VfsError::NotFound);
            }
            Ok(())
        })?;
        Ok(Writer::new(MemWriter {
            target: p,
            buf: RefCell::new(Vec::new()),
        }))
    }

    fn make_dir(p: PathSegs) -> Result<(), VfsError> {
        FS.with_borrow_mut(|fs| {
            if fs.contains_key(&p) {
                return Err(VfsError::Conflict);
            }
            let parent = &p[..p.len().saturating_sub(1)];
            if !is_dir(fs, parent) {
                return Err(VfsError::NotFound);
            }
            fs.insert(p, Node::Dir);
            Ok(())
        })
    }

    fn remove(p: PathSegs) -> Result<(), VfsError> {
        FS.with_borrow_mut(|fs| {
            if !fs.contains_key(&p) {
                return Err(VfsError::NotFound);
            }
            // Borra la entrada y (si es dir) todo su subárbol.
            fs.retain(|k, _| k != &p && !k.starts_with(&p));
            Ok(())
        })
    }

    fn rename(src: PathSegs, dst: PathSegs) -> Result<(), VfsError> {
        FS.with_borrow_mut(|fs| {
            if !fs.contains_key(&src) {
                return Err(VfsError::NotFound);
            }
            let dst_parent = &dst[..dst.len().saturating_sub(1)];
            if !is_dir(fs, dst_parent) {
                return Err(VfsError::NotFound);
            }
            // Mueve `src` y su subárbol reescribiendo el prefijo.
            let moved: Vec<(PathSegs, Node)> = fs
                .iter()
                .filter(|(k, _)| *k == &src || k.starts_with(&src))
                .map(|(k, v)| {
                    let mut nk = dst.clone();
                    nk.extend_from_slice(&k[src.len()..]);
                    (nk, v.clone())
                })
                .collect();
            fs.retain(|k, _| k != &src && !k.starts_with(&src));
            for (k, v) in moved {
                fs.insert(k, v);
            }
            Ok(())
        })
    }
}

/// El writer transaccional: acumula en `buf`; `commit` publica en el FS, `abort`
/// descarta. El path final no existe hasta `commit`.
struct MemWriter {
    target: PathSegs,
    buf: RefCell<Vec<u8>>,
}

impl GuestWriter for MemWriter {
    fn write(&self, chunk: Vec<u8>) -> Result<(), VfsError> {
        self.buf.borrow_mut().extend_from_slice(&chunk);
        Ok(())
    }

    fn commit(&self) -> Result<(), VfsError> {
        let bytes = self.buf.borrow().clone();
        FS.with_borrow_mut(|fs| fs.insert(self.target.clone(), Node::File(bytes)));
        Ok(())
    }

    fn abort(&self) -> Result<(), VfsError> {
        self.buf.borrow_mut().clear();
        Ok(())
    }
}

export!(Mem);
