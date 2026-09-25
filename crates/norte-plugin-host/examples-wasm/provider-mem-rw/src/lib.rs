//! WASM guest (#30 stage 2b-write, ADR 0032): a WRITABLE provider over a
//! flat in-memory FS (thread-local, mutable) — dogfooding the WIT
//! `provider` interface's write path.
//!
//! The `writer` is transactional: bytes accumulate in the resource's own
//! staging and the final path does NOT exist until `commit`. `abort`/dropping
//! without a commit publishes nothing. Names in raw bytes (rule 1).

use std::cell::RefCell;
use std::collections::HashMap;

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

/// Path = its segments (raw bytes). The root is the empty list.
type PathSegs = Vec<Vec<u8>>;

/// A node of the flat FS.
#[derive(Clone)]
enum Node {
    File(Vec<u8>),
    Dir,
}

thread_local! {
    /// The FS: a FLAT map of path (segments) → node. The root (`[]`) is an
    /// implicit dir, not a key.
    static FS: RefCell<HashMap<PathSegs, Node>> = RefCell::new(seed());
}

/// Initial canonical tree (same as provider-mem, without the non-VPath
/// names).
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

/// `true` if `child` is a DIRECT child of `parent` (one more segment, same
/// prefix).
fn is_direct_child(parent: &[Vec<u8>], child: &[Vec<u8>]) -> bool {
    child.len() == parent.len() + 1 && child.starts_with(parent)
}

/// `true` if `p` is an existing directory (or the root).
fn is_dir(fs: &HashMap<PathSegs, Node>, p: &[Vec<u8>]) -> bool {
    p.is_empty() || matches!(fs.get(p), Some(Node::Dir))
}

struct Mem;

impl Guest for Mem {
    fn configure(_cfg: ProviderConfig) -> Result<(), VfsError> {
        // The in-memory provider establishes no connection: no-op.
        Ok(())
    }

    fn capabilities() -> Caps {
        Caps {
            read_only: false,
            case_sensitive: true,
            case_preserving: true,
        }
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
                    Err(VfsError::Unsupported) // listing a file
                } else {
                    Err(VfsError::NotFound)
                };
            }
            // A single page (the test tree is small).
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

    // ---- write ----

    type Writer = MemWriter;

    fn open_writer(p: PathSegs) -> Result<Writer, VfsError> {
        // The parent must exist and be a dir; the final path cannot be a
        // dir.
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
            // Deletes the entry and (if it is a dir) its whole subtree.
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
            // Moves `src` and its subtree by rewriting the prefix.
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

/// The transactional writer: accumulates in `buf`; `commit` publishes to
/// the FS, `abort` discards. The final path does not exist until
/// `commit`.
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
