//! Plugin catalog (ADR 0022 D5/D6): discovers local `.wasm` files and their
//! manifests, and orders them by category for the extensions manager.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::config_values::resolve_settings;
use crate::manifest::{Category, Manifest, ManifestError};

/// Level of an invocable action (ADR 0022 D5): the three are NEVER mixed.
/// The catalog covers `Plugin`; `Script` (Lua) and `BuiltIn` (native
/// commands) are supplied by the UI from its own sources and shown in
/// separate groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Third-party WASM, sandboxed by capabilities.
    Plugin,
    /// User Lua, user permissions (no sandbox).
    Script,
    /// Native command of the binary.
    BuiltIn,
}

/// An installed plugin: its manifest + local state.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Validated manifest.
    pub manifest: Manifest,
    /// Plugin directory (`~/.config/norte/plugins/<id>/`).
    pub dir: PathBuf,
    /// Enabled by the user.
    pub enabled: bool,
    /// The user approved the declared capabilities (if not, `⚠ unapproved`
    /// and the host does not load it — ADR 0022 D4).
    pub approved: bool,
    /// EFFECTIVE `[config]` values (P2 decision 3): the manifest schema's
    /// defaults, with `dir/config.toml` overlaid and already
    /// validated — [`crate::resolve_settings`] runs in `load_dir` and, if
    /// it fails, the plugin goes to `errors` instead of here (see
    /// [`Catalog::load_dir`]). Canonical string encoding (decision 4).
    /// Empty if the manifest does not declare `[config]`.
    pub settings: BTreeMap<String, String>,
    /// What is in `<dir>/help.md` at discovery time (H3e).
    pub help: HelpPresence,
    /// Sha256 hex of `<dir>/plugin.wasm` at DISCOVERY time, or `None` if
    /// there is no binary to hash (#241).
    ///
    /// Goes into [`Self::approval_anchor`], and that is its whole reason:
    /// the manifest digest closes the approve↔run TOCTOU on the
    /// `plugin.toml` side, and left the other side wide open. Whoever
    /// could change the `.wasm` without touching the `.toml` — an
    /// installer, a compromised package, any process of the user's — kept
    /// the capabilities a human approved for OTHER code.
    ///
    /// Computed ONCE, at discovery, not on every call: the approval is
    /// checked on every preview and every columns page, and reading
    /// megabytes there would mean paying for the hash in the paint loop.
    pub wasm_digest: Option<String>,
}

impl PluginEntry {
    /// The anchor of a human approval: manifest **and** binary (#241).
    ///
    /// [`Manifest::approval_digest`] answers "is it still asking for the
    /// same thing, and firing the same way?". It was missing the other
    /// half: "is it still the same code?". Without it, changing
    /// `plugin.wasm` while leaving `plugin.toml` untouched kept the
    /// approval — which is exactly the confused-deputy the digest exists
    /// to close, coming in through the bundle's other door.
    ///
    /// A plugin with no binary anchors only the manifest: there is no code
    /// that could change unnoticed, because there is no code.
    ///
    /// **Bumping this invalidates every approval already given**, and it
    /// is deliberate: the question the human answered did not include "and
    /// this binary", so their answer does not cover what is now being
    /// asked.
    #[must_use]
    pub fn approval_anchor(&self) -> String {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(
            b"norte-plugin-approval:v2
",
        );
        let manifest = self.manifest.approval_digest();
        h.update((manifest.len() as u64).to_le_bytes());
        h.update(manifest.as_bytes());
        match &self.wasm_digest {
            None => h.update([0u8]),
            Some(d) => {
                h.update([1u8]);
                h.update((d.len() as u64).to_le_bytes());
                h.update(d.as_bytes());
            }
        }
        crate::capability::hex_lower(&h.finalize())
    }
}

/// What discovery found in `<dir>/help.md` (H3e).
///
/// A TRI-STATE and not two flags: "passes the guard" implies "exists",
/// never the other way around, so two bools would have a fourth,
/// impossible combination (verified and absent) that someone would end up
/// constructing.
///
/// Resolved HERE, at discovery, not in `plugin.list`: that listing runs on
/// the async reactor and under the global plugins lock, so the guard's
/// three syscalls (one `is_file` and two `canonicalize`) per plugin would
/// block every other connection over a directory that might be on autofs
/// or NFS, and `plugin.list` is OPEN to an agent. Discovery is already I/O
/// and already runs off the reactor, so this is its place — like `name`,
/// `commands` or `capabilities`, which are also snapshots taken at
/// discovery time.
///
/// NEVER a read of the content: the whole catalog is walked on every
/// `plugin.list` (the registry is ephemeral per call), and reading 64 KiB
/// per plugin there would mean paying for the content on every listing
/// just to decide whether to paint a node in a side bar. The content is
/// read on demand, in `plugin.help`.
///
/// It is a cached HINT, and that is why it can go stale with no
/// consequence: whoever serves the content re-applies the guard on read,
/// so a stale [`Self::Servable`] hands out nothing from outside. All it
/// reveals is that, at discovery time, there was a regular, non-escaped
/// file at the FIXED path `<dir>/help.md`, which whoever asks does not
/// choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpPresence {
    /// No `help.md`.
    Absent,
    /// There is a `help.md` but the host will NOT serve it: it does not
    /// pass the escape guard ([`verified_child`]) — a symlink that leaves
    /// the plugin's directory, or a broken one. Exists for DIAGNOSTICS
    /// (`norte doctor` reports it), never for the wire: distinguishing it
    /// there would be a path oracle.
    Unservable,
    /// There is a `help.md` and it passes the guard. The state that
    /// crosses the wire as `PluginInfo::has_help`.
    Servable,
}

impl HelpPresence {
    /// Did the author put a `help.md` there, whether we serve it or not?
    /// The LOOSE question, the diagnostic one.
    #[must_use]
    pub fn is_present(self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// Is there a page the host is going to serve? The STRICT question,
    /// the one that crosses the wire.
    #[must_use]
    pub fn is_servable(self) -> bool {
        matches!(self, Self::Servable)
    }
}

/// The bytes of `<dir>/plugin.wasm`, through the same guard it is run with
/// ([`verified_child`]) — hashing one file and running another would be
/// worse than not hashing at all.
///
/// `None` when there is no servable binary: a plugin without `.wasm` runs
/// nothing, so there is no code to anchor.
fn read_wasm(dir: &Path) -> WasmRead {
    let Some(path) = verified_child(dir, "plugin.wasm") else {
        return WasmRead::Absent;
    };
    // The cap BEFORE reading: `std::fs::metadata`, not `read` — a sparse
    // file of several GiB installs at no cost and would be read whole on
    // every discovery. The same cap the runtime applies when instantiating.
    match std::fs::metadata(&path).map(|m| m.len()) {
        Ok(len) if len > crate::MAX_ARTIFACT_BYTES => WasmRead::TooLarge(len),
        Ok(_) => std::fs::read(&path).map_or(WasmRead::Absent, WasmRead::Bytes),
        Err(_) => WasmRead::Absent,
    }
}

/// What is in `<dir>/plugin.wasm`, without having read it if it doesn't fit.
enum WasmRead {
    /// No servable binary (absent, symlink out, unreadable).
    Absent,
    /// There is one, and it is larger than [`crate::MAX_ARTIFACT_BYTES`]:
    /// not read.
    TooLarge(u64),
    /// The bytes.
    Bytes(Vec<u8>),
}

/// The digest of a binary as the catalog anchors it in
/// [`PluginEntry::wasm_digest`]: sha256 in lowercase hex. Public so that
/// whoever is about to INSTANTIATE some bytes can check they are the ones a
/// human approved, instead of trusting that the path did not change
/// between discovery and load.
///
/// ```
/// use norte_plugin_host::wasm_digest_of;
/// assert_eq!(wasm_digest_of(b"").len(), 64);
/// assert_ne!(wasm_digest_of(b"a"), wasm_digest_of(b"b"));
/// ```
#[must_use]
pub fn wasm_digest_of(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    crate::capability::hex_lower(&h.finalize())
}

/// Resolves the `<dir>/help.md` tri-state (H3e). The LOOSE `is_file`
/// follows links on purpose — presence, not permission — and the guard
/// decides whether it is also servable.
fn help_presence(dir: &Path) -> HelpPresence {
    if verified_child(dir, "help.md").is_some() {
        HelpPresence::Servable
    } else if dir.join("help.md").is_file() {
        HelpPresence::Unservable
    } else {
        HelpPresence::Absent
    }
}

/// `<dir>/<name>` canonicalized, ONLY if the real file falls INSIDE `dir`.
///
/// The common form of issue #69's guard: a file the host reads or runs
/// from a plugin's directory must not be able to resolve outside it VIA
/// SYMLINK. `None` if it does not exist, is not a file, does not
/// canonicalize (broken link) or escapes.
///
/// Lives here, not in `norte-core` next to its callers, so that ONE single
/// implementation exists: the catalog needs the verdict at discovery time
/// (see [`PluginEntry::help`]) and `norte-core` needs it when reading or
/// running. Copying the guard to avoid the dependency would be much worse
/// than keeping it here — two copies of a security guard diverge.
///
/// # What it does NOT cover (stated, not implied)
///
/// - **Symlinks only.** A HARDLINK has no target path: `<dir>/x`
///   canonicalizes to itself and passes the guard even if its inode is
///   `~/.ssh/id_ed25519`'s. A bind mount, likewise. No PATH-BASED guard can
///   see them, so "cannot escape its directory" is a stronger claim than
///   what this delivers: what it delivers is "cannot escape via symlink".
/// - **It is a POINT-IN-TIME observation, not a handle.** Returning the
///   canonical path avoids re-resolving the INTERMEDIATE components, but
///   the kernel resolves the whole path on every `open`, final component
///   included: whoever can write to the directory can change that last
///   component between the check and the open (TOCTOU). A canonical path
///   freezes nothing. It is OUTSIDE the threat model — whoever writes
///   there can already replace the whole bundle, same trust boundary —
///   but it is stated rather than assumed.
///
/// `O_NOFOLLOW` would close that final-component race and is DISCARDED
/// knowingly: it would also forbid a symlink INTERNAL to the directory,
/// which a plugin organizing its own files with links legitimately uses
/// (there is a test that pins it). Do not re-litigate without that case in
/// hand.
///
/// ```
/// use norte_plugin_host::verified_child;
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("help.md"), "hello").unwrap();
/// assert!(verified_child(dir.path(), "help.md").is_some());
/// assert!(verified_child(dir.path(), "absent.md").is_none());
/// ```
#[must_use]
pub fn verified_child(dir: &Path, name: &str) -> Option<PathBuf> {
    let child = dir.join(name);
    if !child.is_file() {
        return None;
    }
    let canon_child = child.canonicalize().ok()?;
    let canon_dir = dir.canonicalize().ok()?;
    canon_child.starts_with(&canon_dir).then_some(canon_child)
}

/// A manifest that failed to load, with its cause (to warn in the manager
/// instead of vanishing silently).
#[derive(Debug)]
pub struct LoadError {
    /// Culprit directory.
    pub dir: PathBuf,
    /// The cause.
    pub error: ManifestError,
}

/// The catalog of discovered plugins + those that failed to load.
#[derive(Debug, Default)]
pub struct Catalog {
    /// Valid plugins, ordered by category then by id.
    pub plugins: Vec<PluginEntry>,
    /// Invalid manifests (shown as an error, not hidden).
    pub errors: Vec<LoadError>,
}

impl Catalog {
    /// Discovers plugins at `root/<id>/plugin.toml`. Not async I/O: the
    /// host calls it once at startup (or via `spawn_blocking` from async
    /// context). A nonexistent `root` = empty catalog (not an error).
    #[must_use]
    pub fn load_dir(root: &Path) -> Catalog {
        let mut cat = Catalog::default();
        let Ok(entries) = std::fs::read_dir(root) else {
            return cat;
        };
        // Valid manifests are collected separately so they can be
        // DEDUPLICATED by id before accepting them: two directories with
        // the same `plugin.id` are a confused-deputy vector (issue #69) —
        // the second could claim the first's approval. ALL colliding ones
        // are rejected (fail-closed).
        let mut parsed: Vec<(Manifest, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let toml_path = dir.join("plugin.toml");
            let Ok(src) = std::fs::read_to_string(&toml_path) else {
                continue; // no plugin.toml means no plugin (not an error)
            };
            match Manifest::from_toml(&src) {
                Ok(manifest) => parsed.push((manifest, dir)),
                Err(error) => cat.errors.push(LoadError { dir, error }),
            }
        }
        // Occurrence count per id: an id that appears more than once is
        // ambiguous and is rejected in all of its directories.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (manifest, _) in &parsed {
            *counts.entry(manifest.id.clone()).or_insert(0) += 1;
        }
        for (manifest, dir) in parsed {
            if counts.get(&manifest.id).copied().unwrap_or(0) > 1 {
                let id = manifest.id.clone();
                cat.errors.push(LoadError {
                    dir,
                    error: ManifestError::DuplicateId(id),
                });
            } else {
                // P2 decision 3: `[config]` VALUES are resolved and
                // validated HERE, at discovery — fail-closed at the
                // catalog level (same treatment as `DuplicateId`): a
                // `config.toml` that fails to validate excludes the WHOLE
                // plugin, it never loads with half-way values.
                let settings = match resolve_settings(&manifest, &dir) {
                    Ok(settings) => settings,
                    Err(error) => {
                        cat.errors.push(LoadError {
                            dir,
                            error: ManifestError::from(error),
                        });
                        continue;
                    }
                };
                // The binary is read ONCE: the digest that anchors the
                // approval (#241) and the WIT packages it names (ADR 0094)
                // both come from those bytes. A guest built against a
                // different version is listed as broken with both
                // versions, instead of loading and dying inside wasmtime
                // naming an interface.
                let bytes = match read_wasm(&dir) {
                    WasmRead::Absent => None,
                    WasmRead::Bytes(b) => Some(b),
                    WasmRead::TooLarge(len) => {
                        cat.errors.push(LoadError {
                            dir,
                            error: ManifestError::ArtifactTooLarge {
                                len,
                                cap: crate::MAX_ARTIFACT_BYTES,
                            },
                        });
                        continue;
                    }
                };
                // A plugin with both an invalid `config.toml` AND a
                // mismatched binary reports the former: the values are
                // resolved first, and one cause per entry is enough for
                // the human to act.
                //
                // The packages the binary names are not stored: a plugin
                // that reaches `plugins` has already proven there is no
                // mismatch, and one that has one goes to `errors` with
                // both versions.
                let wit = bytes
                    .as_deref()
                    .map(crate::wit_packages)
                    .unwrap_or_default();
                if let Some(m) = crate::wit_mismatch(&wit) {
                    cat.errors.push(LoadError {
                        dir,
                        error: ManifestError::WitMismatch {
                            package: m.package,
                            built_against: m.built_against,
                            served: m.served,
                        },
                    });
                    continue;
                }
                cat.plugins.push(PluginEntry {
                    help: help_presence(&dir),
                    wasm_digest: bytes.as_deref().map(wasm_digest_of),
                    manifest,
                    dir,
                    enabled: false,
                    approved: false,
                    settings,
                });
            }
        }
        cat.plugins.sort_by(|a, b| {
            a.manifest
                .category
                .as_str()
                .cmp(b.manifest.category.as_str())
                .then_with(|| a.manifest.id.cmp(&b.manifest.id))
        });
        cat
    }

    /// Groups plugins by category, in stable order — the basis of the
    /// manager's ordered view (ADR 0022 D5). Only categories with some
    /// plugin.
    #[must_use]
    pub fn by_category(&self) -> Vec<(Category, Vec<&PluginEntry>)> {
        const ORDER: [Category; 7] = [
            Category::Previewer,
            Category::Provider,
            Category::Command,
            Category::Columns,
            Category::Hook,
            Category::Decorator,
            Category::Renamer,
        ];
        ORDER
            .into_iter()
            .filter_map(|cat| {
                let group: Vec<&PluginEntry> = self
                    .plugins
                    .iter()
                    .filter(|p| p.manifest.category == cat)
                    .collect();
                (!group.is_empty()).then_some((cat, group))
            })
            .collect()
    }
}
