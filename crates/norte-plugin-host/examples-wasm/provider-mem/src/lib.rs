//! WASM guest (#30 stage 2, ADR 0032): a READ-ONLY provider over a FIXED
//! in-memory tree — dogfooding the WIT `provider` interface.
//!
//! Tests the `Provider` trait's async→sync projection: paginated `list`
//! (pages of 2) and ranged `read`. Names travel as RAW BYTES (rule 1) —
//! includes a hostile one (`a\xff\xfeb`) to pin the round trip. The tree is
//! the canonical one from `readonly_provider_contract!` (a subset).

wit_bindgen::generate!({
    world: "norte:provider/norte-provider",
    path: "wit",
    // `host-log`/`host-config` live in ANOTHER package since the split
    // (ADR 0041 decision 4); wit-bindgen requires explicitly deciding what
    // to do with imports from outside the world's package.
    generate_all,
});

use exports::norte::provider::provider::{
    Caps, Entry, EntryKind, Guest, GuestWriter, Page, ProviderConfig, VfsError, Writer,
};

struct Mem;

/// A node of the fixed tree: file (with content) or directory (with
/// children and their type). Names in raw bytes.
enum Node {
    File(&'static [u8]),
    Dir(&'static [(&'static [u8], NodeKind)]),
}

#[derive(Clone, Copy)]
enum NodeKind {
    File,
    Dir,
}

/// Hostile names seeded under `/hostile`: non-UTF-8 bytes (`a\xff\xfeb`)
/// and a name with an INTERIOR `/` (`a/b` — a single segment) that tests
/// that the segment model does NOT treat `/` as a separator. Content =
/// its bytes.
const HOSTILE: &[u8] = b"a\xff\xfeb";
const HOSTILE_SLASH: &[u8] = b"a/b";

/// Resolves a path (segments) to the tree's node, or `None` if it does not
/// exist.
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

/// Size as u64 of a `&[u8]` content.
fn len_u64(b: &[u8]) -> u64 {
    b.len() as u64
}

impl Guest for Mem {
    fn configure(_cfg: ProviderConfig) -> Result<(), VfsError> {
        // The in-memory provider establishes no connection: no-op.
        Ok(())
    }

    fn capabilities() -> Caps {
        Caps {
            read_only: true,
            case_sensitive: true,
            case_preserving: true,
        }
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
            Some(Node::File(_)) => return Err(VfsError::Unsupported), // list of a file
            None => return Err(VfsError::NotFound),
        };
        // Real pagination (pages of 2) to exercise the cursor. The cursor
        // is OPAQUE (bytes): this guest encodes the start index in 4 LE
        // bytes; a cursor with another length is garbage → CursorExpired.
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
            // A child's size is resolved by looking at its node (a stat
            // call would give the same; file/dir is enough here).
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
            Some(Node::Dir(_)) => return Err(VfsError::Unsupported), // read of a dir
            None => return Err(VfsError::NotFound),
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let want = usize::try_from(len).unwrap_or(usize::MAX);
        let end = start.saturating_add(want).min(data.len());
        Ok(data[start..end].to_vec())
    }

    // ---- write: this guest is READ-ONLY → everything Unsupported (#30
    // stage 2b-write). The writer resource is declared but never
    // constructed.
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

/// Unreachable writer of a read-only guest (never constructed).
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
