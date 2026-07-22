//! Guest WASM (#30 stage 2, ADR 0032): un provider READ-ONLY sobre un árbol
//! FIJO en memoria — dogfood de la interfaz WIT `provider`.
//!
//! Prueba la proyección async→sync del trait `Provider`: `list` paginado
//! (páginas de 2) y `read` por rango. Los nombres viajan como BYTES CRUDOS
//! (regla 1) — incluye uno hostil (`a\xff\xfeb`) para pinear el round-trip.
//! El árbol es el canónico de `readonly_provider_contract!` (subconjunto).

wit_bindgen::generate!({
    world: "norte-provider",
    path: "wit",
});

use exports::norte::plugin::provider::{
    Caps, Entry, EntryKind, Guest, GuestWriter, Page, ProviderConfig, VfsError, Writer,
};

struct Mem;

/// Un nodo del árbol fijo: fichero (con contenido) o directorio (con hijos y su
/// tipo). Nombres en bytes crudos.
enum Node {
    File(&'static [u8]),
    Dir(&'static [(&'static [u8], NodeKind)]),
}

#[derive(Clone, Copy)]
enum NodeKind {
    File,
    Dir,
}

/// Nombres hostiles sembrados en `/hostile`: bytes no-UTF8 (`a\xff\xfeb`) y un
/// nombre con un `/` INTERIOR (`a/b` — un solo segmento) que prueba que el
/// modelo de segmentos NO trata el `/` como separador. Contenido = sus bytes.
const HOSTILE: &[u8] = b"a\xff\xfeb";
const HOSTILE_SLASH: &[u8] = b"a/b";

/// Resuelve un path (segmentos) al nodo del árbol, o `None` si no existe.
fn resolve(path: &[Vec<u8>]) -> Option<Node> {
    match path.len() {
        0 => Some(Node::Dir(&[
            (b"docs", NodeKind::Dir),
            (b"vacio.txt", NodeKind::File),
            (b"hostile", NodeKind::Dir),
        ])),
        _ => resolve_named(path),
    }
}

fn resolve_named(path: &[Vec<u8>]) -> Option<Node> {
    let seg = |i: usize| path.get(i).map(Vec::as_slice);
    match (path.len(), seg(0)) {
        (1, Some(b"vacio.txt")) => Some(Node::File(b"")),
        (1, Some(b"docs")) => Some(Node::Dir(&[
            (b"hello.txt", NodeKind::File),
            (b"sub", NodeKind::Dir),
        ])),
        (1, Some(b"hostile")) => Some(Node::Dir(&[
            (HOSTILE, NodeKind::File),
            (HOSTILE_SLASH, NodeKind::File),
        ])),
        (2, Some(b"docs")) => match seg(1) {
            Some(b"hello.txt") => Some(Node::File(b"hola norte\n")),
            Some(b"sub") => Some(Node::Dir(&[(b"nested.bin", NodeKind::File)])),
            _ => None,
        },
        (2, Some(b"hostile")) if seg(1) == Some(HOSTILE) => Some(Node::File(HOSTILE)),
        (2, Some(b"hostile")) if seg(1) == Some(HOSTILE_SLASH) => Some(Node::File(HOSTILE_SLASH)),
        (3, Some(b"docs")) if seg(1) == Some(b"sub") && seg(2) == Some(b"nested.bin") => {
            Some(Node::File(&[0x00, 0x01, 0x02, 0xff]))
        }
        _ => None,
    }
}

fn entry(name: &[u8], kind: EntryKind, size: Option<u64>) -> Entry {
    Entry {
        name: name.to_vec(),
        kind,
        size,
    }
}

/// Tamaño en u64 de un contenido `&[u8]`.
fn len_u64(b: &[u8]) -> u64 {
    b.len() as u64
}

impl Guest for Mem {
    fn configure(_cfg: ProviderConfig) -> Result<(), VfsError> {
        // El provider en memoria no establece conexión: no-op.
        Ok(())
    }

    fn capabilities() -> Caps {
        Caps { read_only: true }
    }

    fn stat(p: Vec<Vec<u8>>) -> Result<Entry, VfsError> {
        match resolve(&p) {
            Some(Node::File(data)) => {
                let name = p.last().cloned().unwrap_or_default();
                Ok(entry(&name, EntryKind::File, Some(len_u64(data))))
            }
            Some(Node::Dir(_)) => {
                let name = p.last().cloned().unwrap_or_default();
                Ok(entry(&name, EntryKind::Dir, None))
            }
            None => Err(VfsError::NotFound),
        }
    }

    fn list_dir(p: Vec<Vec<u8>>, cursor: Option<Vec<u8>>) -> Result<Page, VfsError> {
        let children = match resolve(&p) {
            Some(Node::Dir(kids)) => kids,
            Some(Node::File(_)) => return Err(VfsError::Unsupported), // list de un fichero
            None => return Err(VfsError::NotFound),
        };
        // Paginación real (páginas de 2) para ejercitar el cursor. El cursor es
        // OPACO (bytes): este guest codifica el índice de inicio en 4 bytes LE;
        // un cursor con otra longitud es basura → CursorExpired.
        const PAGE: usize = 2;
        let start = match cursor {
            None => 0usize,
            Some(bytes) => match <[u8; 4]>::try_from(bytes.as_slice()) {
                Ok(b) => u32::from_le_bytes(b) as usize,
                Err(_) => return Err(VfsError::CursorExpired),
            },
        };
        let mut out = Vec::new();
        for (name, kind) in children.iter().skip(start).take(PAGE) {
            // El size de un hijo se resuelve mirando su nodo (una llamada de
            // stat lo daría igual; aquí basta con file/dir).
            let (ek, size) = match kind {
                NodeKind::File => {
                    let mut child = p.clone();
                    child.push(name.to_vec());
                    let size = match resolve(&child) {
                        Some(Node::File(d)) => Some(len_u64(d)),
                        _ => None,
                    };
                    (EntryKind::File, size)
                }
                NodeKind::Dir => (EntryKind::Dir, None),
            };
            out.push(entry(name, ek, size));
        }
        let next = start + out.len();
        let next_cursor = if next < children.len() {
            Some((next as u32).to_le_bytes().to_vec())
        } else {
            None
        };
        Ok(Page {
            entries: out,
            next_cursor,
        })
    }

    fn read(p: Vec<Vec<u8>>, offset: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        let data = match resolve(&p) {
            Some(Node::File(d)) => d,
            Some(Node::Dir(_)) => return Err(VfsError::Unsupported), // read de un dir
            None => return Err(VfsError::NotFound),
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let want = usize::try_from(len).unwrap_or(usize::MAX);
        let end = start.saturating_add(want).min(data.len());
        Ok(data[start..end].to_vec())
    }

    // ---- escritura: este guest es READ-ONLY → todo Unsupported (#30 stage
    // 2b-write). El writer resource se declara pero jamás se construye.
    type Writer = NoWriter;

    fn open_writer(_s: Vec<Vec<u8>>) -> Result<Writer, VfsError> {
        Err(VfsError::Unsupported)
    }

    fn make_dir(_s: Vec<Vec<u8>>) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }

    fn remove(_s: Vec<Vec<u8>>) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }

    fn rename(_src: Vec<Vec<u8>>, _dst: Vec<Vec<u8>>) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }
}

/// Writer inalcanzable de un guest read-only (nunca se construye).
struct NoWriter;

impl GuestWriter for NoWriter {
    fn write(&self, _chunk: Vec<u8>) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }
    fn commit(&self) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }
    fn abort(&self) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }
}

export!(Mem);
