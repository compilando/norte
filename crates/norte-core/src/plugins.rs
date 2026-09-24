//! Daemon plugin registry (M4-P3): discovers the local catalog
//! ([`norte_plugin_host::Catalog`]), merges in the approved/enabled state the
//! user persists, and exposes it over the protocol ([`norte_proto::methods`]).
//!
//! The catalog's [`PluginEntry`](norte_plugin_host::PluginEntry) is ALWAYS born
//! `approved = false` / `enabled = false` (the discoverer does not know the
//! user's state): the state's truth lives in `plugins-state.toml` and this
//! registry is what merges it in.
//!
//! ## `plugins-state.toml` format
//!
//! A plugin's id is reverse-DNS (`org.norte.demo`) — WITH DOTS. Written raw as
//! a header (`[org.norte.demo]`), TOML would read it as nested tables
//! (`org` → `norte` → `demo`), NOT as a literal key. That is why the state
//! goes under a `[plugins]` table with the key QUOTED:
//!
//! ```toml
//! [plugins]
//! "org.norte.demo" = { approved = true, enabled = false }
//! ```
//!
//! `toml_edit` quotes a key with dots when re-emitting, so the
//! discover → persist → discover round trip keeps the id intact.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use norte_plugin_host::Catalog;
use norte_proto::methods::{
    PluginColumnInfo, PluginCommandInfo, PluginInfo, PluginListResult, PluginLoadError,
};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

/// The RAW bytes of an `OsStr`, or `None` if this platform does not have them.
///
/// On Unix they are the literal bytes and there is nothing more to say.
///
/// Outside Unix it returns `None` **on purpose**, not the lossy form. Sending
/// lossy would be worse than sending nothing: the receiver treats `dir_bytes`
/// as raw, so bytes that were already converted would give it back
/// `lossy = false, masked = false` —an altered name declaring itself
/// faithful— and would also DISABLE the fallback heuristic, which today is
/// the only thing that flags a loose Windows substitute. With `None` the
/// receiver falls back to `dir` and to that heuristic, which is exactly what
/// it did before #265.
///
/// The correct conversion there is WTF-8 (the convention
/// `norte_proto::methods::Volume::label` documents), and it will arrive with
/// the rest of Windows support.
// On Unix the `None` case does not exist —the `cfg` removes it— and clippy
// sees an `Option` that is always `Some`. Outside Unix it is the only branch,
// and it is the one that makes the wire field correct.
#[cfg_attr(unix, allow(clippy::unnecessary_wraps))]
fn bytes_de(s: &std::ffi::OsStr) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(s.as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        let _ = s;
        None
    }
}

/// State the user sets on a discovered plugin. Absent = both `false`
/// (discovered but neither approved nor enabled).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginState {
    /// A human approved the declared capabilities.
    pub approved: bool,
    /// A human has it enabled.
    pub enabled: bool,
    /// Digest (hex sha256) of the capabilities the human saw when approving
    /// (issue #69, TOCTOU confused-deputy defense). `None` = approval without
    /// an anchored digest (state inherited from before this defense, or not
    /// approved): treated fail-closed as NOT matching, forcing
    /// re-consent. Set to the manifest's CURRENT digest when approving; wiped
    /// when revoking.
    pub approved_digest: Option<String>,
}

/// Failure running a plugin command. The first three variants are the
/// fail-closed consent verdict (unknown / not approved / disabled); the last
/// two are artifact or runtime failures.
#[derive(Debug, thiserror::Error)]
pub enum PluginRunError {
    /// No plugin discovered with that id.
    #[error("unknown plugin: {0}")]
    Unknown(String),
    /// The plugin exists but its kind does not export `command` (a decorator,
    /// some columns, a provider, a renamer): there is nothing to run. This is
    /// said BEFORE instantiating —instantiating it with the `norte-plugin`
    /// world failed inside wasmtime and came out as "internal error", and a
    /// 0.66 client that sees a renamer as a command (0.67.0) paid for one
    /// instantiation per click just to receive that.
    #[error("plugin {0} does not run commands")]
    NotRunnable(String),
    /// The plugin exists but a human has not approved its capabilities.
    #[error("plugin not approved: {0}")]
    NotApproved(String),
    /// The plugin is approved but disabled.
    #[error("plugin disabled: {0}")]
    Disabled(String),
    /// The plugin has no `plugin.wasm` in its directory. Carries the ID (not
    /// the absolute path: it would reveal the user's home to an agent calling
    /// `plugin.run_command` — consistent with `list()`'s redaction,
    /// security-reviewer M4-P4).
    #[error("plugin {0} has no binary (plugin.wasm)")]
    NoBinary(String),
    /// The WASM runtime failed to compile, instantiate, or run the component.
    #[error("runtime: {0}")]
    Runtime(#[from] norte_plugin_host::RuntimeError),
}

/// Failure setting ONE `[config]` value via [`PluginRegistry::set_config`]
/// (0.28.0, G3c). Like [`ConfigValueError`](norte_plugin_host::ConfigValueError)
/// (which it wraps in [`Self::Invalid`]), NO variant carries the submitted
/// VALUE — only the key (issue #73, same criterion).
#[derive(Debug, thiserror::Error)]
pub enum PluginConfigSetError {
    /// No plugin discovered with that id.
    #[error("unknown plugin: {0}")]
    Unknown(String),
    /// `key` is not declared in the manifest's `[config]`.
    #[error("unknown config key: {0}")]
    UnknownKey(String),
    /// The value does not validate against the key's type/range/enum.
    #[error("invalid value: {0}")]
    Invalid(#[from] norte_plugin_host::ConfigValueError),
    /// I/O failure persisting or re-resolving after writing.
    #[error("i/o: {0}")]
    Io(#[source] io::Error),
}

/// Byte cap the core reads from a file when PREVIEWING (1 MiB, anti-DoS): the
/// daemon's handler reads at most this much and hands it to the guest. The
/// M4-P2 guest ADDITIONALLY has its own limit; this is the first barrier, on
/// the host side, so as not to load a huge file into memory just because
/// someone asked for its preview.
pub(crate) const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// Cap on the width in cells a client can ask a previewer for (D4, proto
/// 0.66.0). The field is a HINT and the guest is confined (memory and clock
/// bounded), so a `u32::MAX` breaks nothing; but a hostile client has no
/// reason to be able to make every previewer in the catalog spend its whole
/// budget rescaling a photo nobody is going to see. Wider than any terminal.
pub(crate) const PREVIEW_MAX_COLUMNS: u32 = 1024;

/// Clamps the requested width to [`PREVIEW_MAX_COLUMNS`]; `None` stays
/// `None`. The one funnel between the wire (or the embedded arm) and the
/// guest.
pub(crate) fn clamp_preview_columns(columns: Option<u32>) -> Option<u32> {
    columns.map(|c| c.min(PREVIEW_MAX_COLUMNS))
}

/// Decodes a file's CAPPED bytes to hand them to the previewer (§6.2, #29):
/// text detected (by `norte-encoding`) travels as UTF-8 — never raw bytes the
/// guest would assume are UTF-8 — and a binary (no text encoding) falls back
/// to the bytes as is (a text guest will do its own lossy conversion).
/// `bytes` is ALREADY capped to [`PREVIEW_MAX_BYTES`].
///
/// Returns `(content, lossy)`: `lossy` is `true` if the text decoding was
/// LOSSY (`had_errors` — invalid bytes → `�`), so the frontend can flag it in
/// preview mode the same way the raw viewer already flags its own
/// `had_errors` (#101, `PluginPreview::lossy` on the wire). A binary (no text
/// encoding) is never lossy: its bytes travel raw.
pub(crate) fn decode_for_preview(bytes: Vec<u8>) -> (Vec<u8>, bool) {
    // `< CAP` = the file fit whole (if `== CAP` it may have been truncated:
    // treated as incomplete, the safe direction — at most the last multibyte
    // char is dropped, it is never corrupted into `�`).
    let complete = (bytes.len() as u64) < PREVIEW_MAX_BYTES;
    match norte_encoding::detect(&bytes) {
        norte_encoding::Detection::Text { encoding, .. } => {
            let decoded = norte_encoding::decode(&bytes, encoding, complete);
            // NOT cut by lines, on purpose (ADR 0141 review): a silent cut
            // gave a styled view that looked like the whole file and hid
            // whatever came after the cut line. A result over ten thousand
            // lines is still rejected by the host and the viewer falls back
            // to the raw view, which is whole.
            (decoded.text.into_bytes(), decoded.had_errors)
        }
        norte_encoding::Detection::Binary => (bytes, false),
    }
}

/// Guesses the mimetype by EXTENSION (light heuristic, no sniffing
/// dependency). A file with no recognizable extension →
/// `application/octet-stream` (no `text/*` previewer will claim it). Does NOT
/// read the content. `pub(crate)` for the daemon's handler.
pub(crate) fn guess_mimetype(path: &norte_proto::VPath) -> &'static str {
    let ext = path
        .file_name()
        .map(norte_proto::Segment::as_bytes)
        .and_then(|n| std::str::from_utf8(n).ok())
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()));
    match ext.as_deref() {
        Some("txt" | "rs" | "toml" | "log" | "csv" | "ini" | "conf") => "text/plain",
        // Its own type, so a Markdown previewer can claim it EXACTLY while a
        // `text/*` highlighter keeps everything else (D3).
        Some("md" | "markdown") => "text/markdown",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        // Pictures (D4): an image previewer claims them by exact type.
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Converts the plugin runtime's `render-styled` lines
/// (`Vec<Vec<norte_plugin_host::previewer_iface::Span>>`) to the WIRE type
/// (`Vec<Vec<norte_proto::methods::SpanWire>>`, G3a, ADR 0037). This single
/// conversion is shared by `Backend::plugin_preview_styled`'s EMBEDDED arm and
/// the daemon's `plugin.preview_styled` handler (`daemon::server`), so it is
/// not duplicated.
///
/// `role` travels UNVALIDATED (a responsibility boundary, amendment to ADR
/// 0037 decision 3 in the ADR itself: headless `norte-core` does NOT depend on
/// `norte-theme`, owner of the closed `Role` set — validating here would
/// require that structural dependency just for this surface). The text is
/// NOT masked here either: `norte-core` is headless (rule 7, no display), the
/// per-span masking is the FRONTEND's responsibility (same criterion as
/// `PluginPreview::output`, which is also not masked in the core). The size
/// caps (lines/spans/bytes) were ALREADY applied in `render_styled_preview`
/// (`norte-plugin-host::runtime::cap_styled_text`, POST guest return) — this
/// function only reshapes the type, it does not cap again.
pub(crate) fn to_wire_lines(
    lines: Vec<Vec<norte_plugin_host::previewer_iface::Span>>,
) -> Vec<Vec<norte_proto::methods::SpanWire>> {
    lines
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|s| norte_proto::methods::SpanWire {
                    text: s.text,
                    role: s.role,
                    fg: s.fg.map(|(r, g, b)| [r, g, b]),
                    bg: s.bg.map(|(r, g, b)| [r, g, b]),
                })
                .collect()
        })
        .collect()
}

/// Converts a page's VISIBLE paths to the RAW entries that cross to the WIT
/// `decorator::decorate`/`columns::column-values` (ADR 0037 decision 2): the
/// BASENAME in raw bytes (rule 1), NEVER the full path. A privacy decision,
/// not just a shape one: a decorator/columns sees the name of each visible
/// entry, not where it lives in the tree — the same criterion the real guest
/// (`examples-wasm/decorator-demo`, T2) already assumes in its contract
/// (`decorator_wit_e2e_positional_roundtrip_wasm_real` passes basenames like
/// `b"module.rs"`, not paths). POSITIONAL 1:1 with `paths` — an entry WITHOUT
/// a file name (root path) delivers an empty basename, it is never omitted,
/// so as not to break the positional contract.
pub(crate) fn paths_to_basenames(paths: &[norte_proto::VPath]) -> Vec<Vec<u8>> {
    paths
        .iter()
        .map(|p| {
            p.file_name()
                .map(|s| norte_proto::Segment::as_bytes(s).to_vec())
                .unwrap_or_default()
        })
        .collect()
}

/// The entries a DECORATOR guest sees (ADR 0105): the name of each path and
/// its class, POSITIONAL with `paths`. `kinds` is what the frontend listed;
/// it can arrive empty (a 0.71 client) or short, and then what is missing is
/// `other` —the class a guest treats as a file—, never an error: the class is
/// cosmetic for the icon, not a condition of the batch.
pub(crate) fn paths_to_entries(
    paths: &[norte_proto::VPath],
    kinds: &[norte_proto::EntryKind],
) -> Vec<norte_plugin_host::decorator_iface::Entry> {
    use norte_plugin_host::decorator_iface::EntryKind as Wit;
    if !kinds.is_empty() && kinds.len() != paths.len() {
        // Empty is a 0.71 client; short is a 0.72 client with an error, and
        // degrading silently would hide it forever.
        tracing::warn!(
            paths = paths.len(),
            kinds = kinds.len(),
            "decorate: kinds does not match paths, what is missing is treated as other"
        );
    }
    paths_to_basenames(paths)
        .into_iter()
        .enumerate()
        .map(|(i, name)| norte_plugin_host::decorator_iface::Entry {
            name,
            kind: match kinds.get(i) {
                Some(norte_proto::EntryKind::File) => Wit::File,
                Some(norte_proto::EntryKind::Dir) => Wit::Dir,
                Some(norte_proto::EntryKind::Symlink) => Wit::Symlink,
                Some(norte_proto::EntryKind::Other) | None => Wit::Other,
            },
        })
        .collect()
}

/// A decorator's slot, from the manifest to the wire (ADR 0105).
pub(crate) fn slot_to_wire(
    slot: norte_plugin_host::DecoratorSlot,
) -> norte_proto::methods::DecorationSlot {
    match slot {
        norte_plugin_host::DecoratorSlot::Badge => norte_proto::methods::DecorationSlot::Badge,
        norte_plugin_host::DecoratorSlot::Icon => norte_proto::methods::DecorationSlot::Icon,
    }
}

/// Converts the raw BATCH a DECORATOR guest returns
/// (`DecoratorInstance::decorate`) to the wire type
/// (`Vec<norte_proto::methods::DecorationWire>`), VALIDATING the 1:1
/// positional contract (ADR 0037 decision table 1) before reshaping: if
/// `out.len() != expected_len` the guest violated the contract (a plugin bug,
/// or a runtime that skipped `cap_total_bytes` some other way) — `None`
/// fail-closed (the caller DISCARDS that whole plugin's decorations, with a
/// warning; the rest of the page is painted the same, the same fallback
/// criterion as a previewer that does not apply). `role` travels UNVALIDATED
/// (the same responsibility boundary as [`to_wire_lines`] — the frontend, not
/// headless `norte-core`, knows `norte_theme::Role`).
pub(crate) fn decorations_to_wire_checked(
    out: Vec<norte_plugin_host::decorator_iface::Decoration>,
    expected_len: usize,
) -> Option<Vec<norte_proto::methods::DecorationWire>> {
    if out.len() != expected_len {
        return None;
    }
    Some(
        out.into_iter()
            .map(|d| norte_proto::methods::DecorationWire {
                badge: d.badge,
                role: d.role,
            })
            .collect(),
    )
}

/// Validates the 1:1 positional contract of the raw BATCH a COLUMNS guest
/// returns (`ColumnsInstance::column_values`): `None` fail-closed if
/// `out.len() != expected_len` (see [`decorations_to_wire_checked`], same
/// criterion). It already has the wire shape (`Vec<Option<String>>`) — this
/// function only GUARDS the contract, it does not reshape.
pub(crate) fn column_values_checked(
    out: Vec<Option<String>>,
    expected_len: usize,
) -> Option<Vec<Option<String>>> {
    (out.len() == expected_len).then_some(out)
}

/// Converts ONE [`PluginRegistry::config_keys`] entry to its wire form
/// (0.28.0, G3c, `plugin.get_config`): `kind`/`default`/`min`/`max`/`values`/
/// `description` come from the SCHEMA (`spec`), `value` from the already
/// resolved effective one (a separate parameter, not from the schema).
/// `default` is encoded with the SAME canonical criterion as
/// `norte_plugin_host::resolve_settings` (`bool` → `"true"`/`"false"`, `int`
/// → decimal) so `default`/`value` are directly comparable by a frontend.
pub(crate) fn config_key_to_wire(
    key: String,
    spec: &norte_plugin_host::ConfigKeySpec,
    value: String,
) -> norte_proto::methods::PluginConfigKeyWire {
    use norte_plugin_host::ConfigKeySpec;
    use norte_proto::methods::PluginConfigKeyWire;
    match spec {
        ConfigKeySpec::String {
            default,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "string".into(),
            default: default.clone(),
            min: None,
            max: None,
            values: Vec::new(),
            description: description.clone(),
            value,
        },
        ConfigKeySpec::Bool {
            default,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "bool".into(),
            default: default.to_string(),
            min: None,
            max: None,
            values: Vec::new(),
            description: description.clone(),
            value,
        },
        ConfigKeySpec::Int {
            default,
            min,
            max,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "int".into(),
            default: default.to_string(),
            min: *min,
            max: *max,
            values: Vec::new(),
            description: description.clone(),
            value,
        },
        ConfigKeySpec::Enum {
            default,
            values,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "enum".into(),
            default: default.clone(),
            min: None,
            max: None,
            values: values.clone(),
            description: description.clone(),
            value,
        },
    }
}

/// Does the glob `pat` (`text/*` or the exact `application/json`) match `mime`?
fn mimetype_matches(pat: &str, mime: &str) -> bool {
    match pat.strip_suffix("/*") {
        Some(prefix) => mime.split('/').next() == Some(prefix),
        None => pat == mime,
    }
}

/// Ceiling on the bytes handed to a thumbnail guest (ADR 0107): a photo, not a
/// video. The guest's sandbox has 64 MiB of memory and has to decode what it
/// receives.
pub const THUMBNAIL_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Result of [`PluginRegistry::resolve_previewer`]: `(id, name, wasm_path,
/// capabilities, settings)` — factored into an alias (instead of a 5-element
/// tuple in-line) because clippy `type_complexity` asks for it; `settings` is
/// P2 Task 4a, see the method's rustdoc.
pub type ResolvedPreviewer = (
    String,
    String,
    // Path AND approved fingerprint (ADR 0142): the runtime rejects bytes
    // that are not the approved ones.
    norte_plugin_host::WasmArtifact,
    norte_plugin_host::Capabilities,
    BTreeMap<String, String>,
);

/// Result of an item from [`PluginRegistry::resolve_decorators`] or
/// [`PluginRegistry::resolve_columns`]: the same `(id, name, wasm_path,
/// capabilities, settings)` shape as [`ResolvedPreviewer`] — the same alias
/// instead of repeating the 5-element tuple (clippy `type_complexity`). A
/// decorator additionally comes with its SLOT (ADR 0105), separately, the way
/// hooks come with their events.
pub type ResolvedDecorator = ResolvedPreviewer;

/// Result of [`PluginRegistry::resolve_provider`]: the consented provider
/// plugin that serves a scheme, with what is needed to instantiate it WITHOUT
/// trusting the disk again.
///
/// A struct and not the other resolvers' tuple because it carries two more
/// things they do not need: the digest of the binary the human approved
/// (whoever instantiates compares the bytes it reads against it) and the
/// contribution's default port (what network access is granted to).
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    /// Plugin id.
    pub id: String,
    /// Readable name (third-party text).
    pub name: String,
    /// Canonical path to `plugin.wasm`, verified inside the directory, with
    /// its approved fingerprint (ADR 0142).
    pub wasm: norte_plugin_host::WasmArtifact,
    /// Digest of `plugin.wasm` as the catalog anchored it on discovery: what
    /// the approval covers (#241). Whoever instantiates MUST read the bytes,
    /// hash them, and compare — a path is not a promise.
    pub wasm_digest: String,
    /// Manifest capabilities (the sandbox enforces them).
    pub capabilities: norte_plugin_host::Capabilities,
    /// Resolved `[config]` values, for `set_settings`.
    pub settings: BTreeMap<String, String>,
    /// `default-port` of the contribution that declares the scheme, if it
    /// carries one.
    pub default_port: Option<u16>,
}

/// The job of reading ONE already-resolved plugin's `help.md`, ready to run
/// outside the reactor (H3e). Obtained with [`PluginRegistry::help_job`] and
/// consumed with [`HelpJob::read`].
///
/// It is OPAQUE: it carries the `dir` the catalog stored on discovery inside,
/// and does not expose it. That is the whole point — the caller gets
/// something it can move to a `spawn_blocking` without ever having received a
/// path it could re-derive from the id that came over the wire, so the escape
/// guard stays entirely inside the registry instead of becoming an obligation
/// of the caller.
#[derive(Debug, Clone)]
pub(crate) struct HelpJob {
    dir: PathBuf,
}

impl HelpJob {
    /// Verifies and READS, capped, the plugin's `help.md`. With no readable
    /// page (absent, unreadable, or escaping the directory) returns the blank
    /// page, indistinguishable from an empty `help.md`: help is cosmetic and
    /// has no reason to distinguish those cases — the one that distinguishes
    /// them is `norte doctor`.
    ///
    /// The escape guard ([`norte_plugin_host::verified_child`]) is applied
    /// HERE, not when the job is built: it is three syscalls and this method
    /// runs in `spawn_blocking`, while building it is pure memory and happens
    /// under the registry's lock.
    ///
    /// THE CAP IS APPLIED WHEN READING, not when decoding. The guard checks
    /// that there is a regular file and NOTHING about its size, so a plugin
    /// can send `help.md` as a SPARSE 100 GiB file —a few bytes in a
    /// tarball— and a single `plugin.help` call would try to reserve 100
    /// GiB: aborting on a reservation failure, or the OOM killer taking down
    /// the daemon with its journal and every task in flight. Since the method
    /// is OPEN to an agent and the plugin needs neither approval nor
    /// enablement, this would be the first uncapped read an agent could
    /// trigger in the daemon. At most `max_bytes + 1` bytes are read: the
    /// extra byte is what lets
    /// [`norte_help::cut_and_decode_untrusted`] see that there was more and
    /// mark `truncated` honestly, instead of serving a cut file as if it were
    /// complete.
    ///
    /// The text it returns is NOT masked: it carries verbatim whatever
    /// terminal dangers the plugin wrote (ESC, C0 controls, bidi overrides).
    /// It is parsed with `norte_help::parse_untrusted`, which masks while
    /// building the model; it is never painted or logged raw.
    ///
    /// SYNCHRONOUS I/O: the async caller puts it in `spawn_blocking` (rule 2).
    #[must_use]
    pub(crate) fn read(self) -> norte_proto::methods::PluginHelpResult {
        use std::io::Read as _;

        let cap = u64::try_from(norte_help::Limits::untrusted().max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let bytes = norte_plugin_host::verified_child(&self.dir, "help.md")
            .and_then(|p| {
                let f = std::fs::File::open(p).ok()?;
                let mut buf = Vec::new();
                // A failure mid-read degrades to a blank page, the same as a
                // `help.md` that cannot be opened: serving what was read up
                // to the error would present it as complete.
                f.take(cap).read_to_end(&mut buf).ok()?;
                Some(buf)
            })
            .unwrap_or_default();
        let s = norte_help::cut_and_decode_untrusted(&bytes);
        norte_proto::methods::PluginHelpResult {
            markdown: s.markdown,
            truncated: s.truncated,
            lossy: s.lossy,
        }
    }
}

/// Plugin registry: discovered catalog + merged persisted state.
#[derive(Debug)]
pub struct PluginRegistry {
    config_dir: PathBuf,
    state: BTreeMap<String, PluginState>,
    catalog: Catalog,
}

impl PluginRegistry {
    /// Name of the state file inside `config_dir`.
    const STATE_FILE: &'static str = "plugins-state.toml";

    /// Discovers the catalog at `config_dir/plugins/<id>/plugin.toml` and
    /// merges in the state from `config_dir/plugins-state.toml`.
    ///
    /// A nonexistent `config_dir/plugins` = empty catalog (not an error). An
    /// absent `plugins-state.toml` = empty state.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidData`] if `plugins-state.toml` exists but is not
    /// valid TOML; any other I/O error reading it propagates as is.
    pub fn discover(config_dir: &Path) -> io::Result<Self> {
        let catalog = Catalog::load_dir(&config_dir.join("plugins"));
        let state = Self::read_state(&config_dir.join(Self::STATE_FILE))?;
        Ok(Self {
            config_dir: config_dir.to_path_buf(),
            state,
            catalog,
        })
    }

    /// An EMPTY registry anchored at `config_dir`, without touching the FS: a
    /// catalog with no plugins and no merged state. The daemon uses it as a
    /// fallback if discovery fails (e.g. a corrupt `plugins-state.toml`): a
    /// broken state file must not prevent startup. Persisting over it
    /// re-creates the state from scratch under `config_dir`.
    #[must_use]
    pub fn empty(config_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            state: BTreeMap::new(),
            catalog: Catalog::default(),
        }
    }

    /// What a plugin OFFERS, in manifest order: first the commands, then the
    /// renamers (0.67.0, ADR 0095), and then the organizers (0.77.0, phase
    /// 8), each with its `kind`.
    ///
    /// The three classes travel in the SAME list because they answer the
    /// same question —"what does this plugin offer me"—, and the palette
    /// paints them together with a label saying which one it is. What
    /// changes between them is which method each row dispatches to, and that
    /// is exactly what `kind` carries.
    ///
    /// It is DISCOVERY: it is not gated by approved or enabled, same as the
    /// columns and the panels — what a plugin offers is exactly what a human
    /// looks at BEFORE approving it.
    #[must_use]
    fn comandos_de(c: &norte_plugin_host::Contributions) -> Vec<PluginCommandInfo> {
        c.command
            .iter()
            .map(|c| PluginCommandInfo {
                id: c.id.clone(),
                title: c.title.clone(),
                kind: norte_proto::methods::PluginCommandKind::Command,
            })
            .chain(c.renamer.iter().map(|r| PluginCommandInfo {
                id: r.id.clone(),
                title: r.title.clone(),
                kind: norte_proto::methods::PluginCommandKind::Renamer,
            }))
            .chain(c.organizer.iter().map(|o| PluginCommandInfo {
                id: o.id.clone(),
                title: o.title.clone(),
                kind: norte_proto::methods::PluginCommandKind::Organizer,
            }))
            .collect()
    }

    /// The discovered catalog merged with the persisted state, in the
    /// protocol's shape.
    #[must_use]
    pub fn list(&self) -> PluginListResult {
        let plugins = self
            .catalog
            .plugins
            .iter()
            .map(|e| {
                let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
                PluginInfo {
                    id: e.manifest.id.clone(),
                    name: e.manifest.name.clone(),
                    publisher: e.manifest.publisher.clone(),
                    version: e.manifest.version.clone(),
                    category: e.manifest.category.as_str().to_string(),
                    // The manifest's badges PLUS the scheme a provider claims
                    // (`provider:webdav`): that is what approving grants
                    // —standing in front of `webdav://`— and until now the
                    // human approved a provider without seeing for which
                    // scheme.
                    capabilities: e
                        .manifest
                        .capabilities
                        .badges()
                        .into_iter()
                        .chain(
                            e.manifest
                                .contributions
                                .provider
                                .iter()
                                .map(|c| format!("provider:{}", c.scheme))
                                // And a hook's events (ADR 0100), for the
                                // same reason: what the plugin is going to
                                // RECEIVE —the path of each mutation of that
                                // class— is what the human approves, and a
                                // hook with no capabilities cannot be
                                // approved over an empty list.
                                .chain(
                                    e.manifest
                                        .contributions
                                        .hook
                                        .iter()
                                        .map(|h| format!("hook:{}", h.on)),
                                ),
                        )
                        .collect(),
                    // EFFECTIVE approval (issue #69): `approved` in the file
                    // but with the capabilities digest MATCHING the current
                    // manifest's. If the capabilities changed on disk after
                    // approving, the UI sees `approved = false` and asks for
                    // consent again.
                    approved: Self::approval_is_current(&st, e),
                    enabled: st.enabled,
                    // (P1) manifest `description` is cosmetic/untrusted, same
                    // as `name`; `commands` mirrors `Contributions.command` in
                    // MANIFEST ORDER (not sorted — matches how the digest
                    // treats contribution order as significant, spec §6).
                    description: e.manifest.description.clone(),
                    commands: Self::comandos_de(&e.manifest.contributions),
                    // (G3c, 0.28.0) columns mirrors `Contributions.columns`
                    // the SAME way `commands` mirrors `Contributions.command`
                    // above: manifest order, discovery-only (NOT gated on
                    // approved/enabled — a plugin's contributed columns are
                    // metadata a human inspects BEFORE approving, same as
                    // `commands`/`capabilities` already are).
                    columns: e
                        .manifest
                        .contributions
                        .columns
                        .iter()
                        .map(|c| PluginColumnInfo {
                            id: c.id.clone(),
                            header: c.header.clone(),
                        })
                        .collect(),
                    // And the panels (0.74.0, phase 3), with the SAME
                    // criterion as the columns: manifest order and pure
                    // discovery, not gated by approved or enabled. What
                    // slots a plugin asks for is exactly what a human looks
                    // at BEFORE approving it.
                    panels: e
                        .manifest
                        .contributions
                        .panel
                        .iter()
                        .map(|p| norte_proto::methods::PluginPanelInfo {
                            kind: p.kind.clone(),
                            title: p.title.clone(),
                            min_cols: p.min_cols,
                            min_rows: p.min_rows,
                        })
                        .collect(),
                    // The anchor the human is LOOKING AT (#282): it is what
                    // it returns when confirming, and what the daemon
                    // compares against its own before granting. It covers
                    // `category` and `contributions` —when and how it
                    // fires— besides the capabilities, i.e. exactly what the
                    // painted list does NOT say.
                    manifest_digest: Some(norte_plugin_host::PluginEntry::approval_anchor(e)),
                    // (H3e, 0.34.0) NOT gated by approved/enabled — a
                    // plugin's documentation is exactly what a human reads
                    // BEFORE approving it, same criterion as
                    // `capabilities`/`commands`/`columns`.
                    //
                    // The WIRE flag is the STRICT one of the two: the
                    // catalog's `is_present` is an `is_file` that FOLLOWS
                    // links (presence, not permission — its own comment
                    // says so), while `is_servable` already passed the SAME
                    // guard the reader will apply. If they diverge, the
                    // pair (`has_help: true`, `markdown: ""`) is exactly the
                    // oracle "that path exists and is a regular file", and
                    // both halves are read by an agent via `plugin.list` +
                    // `plugin.help`, neither gated by policy. And even
                    // without the agent, the sidebar would paint a node that
                    // opens blank.
                    //
                    // It is READ, not computed: `list()` runs in the async
                    // reactor and under the global plugins lock
                    // (`handle_plugin_list` calls it synchronously from
                    // `dispatch`), so applying the guard here would be
                    // three syscalls per plugin blocking every other
                    // connection over a directory that may be on autofs or
                    // NFS — and `plugin.list` is OPEN to an agent. The
                    // verdict is computed at DISCOVERY time, where the I/O
                    // already lives outside the reactor.
                    has_help: e.help.is_servable(),
                }
            })
            .collect();
        let errors = self
            .catalog
            .errors
            .iter()
            .map(|e| {
                // Only the plugin directory's NAME, never the absolute path:
                // it would reveal the user's home (`~/.config/norte/...`) to
                // an agent calling `plugin.list`. The basename is enough for
                // a human to identify the broken plugin.
                // `file_name()` is `None` for a path ending in `..`; falling
                // back there to `as_os_str()` would send the ABSOLUTE path,
                // which is exactly what the field's rustdoc promises never
                // happens (reveals the user's home to an agent calling
                // `plugin.list`).
                let base = e.dir.file_name().unwrap_or_else(|| "?".as_ref());
                PluginLoadError {
                    dir: base.to_string_lossy().into_owned(),
                    // And the BYTES alongside it (#265): the
                    // `to_string_lossy` above puts `U+FFFD`, which is NOT a
                    // terminal danger, so no heuristic on the receiver's
                    // side can recover that a conversion happened. With the
                    // bytes it can do that itself and flag it, the usual
                    // rule.
                    dir_bytes: bytes_de(base),
                    reason: e.error.to_string(),
                }
            })
            .collect();
        PluginListResult { plugins, errors }
    }

    /// The directories that did NOT load, with their TYPED cause (unlike
    /// [`Self::list`], which flattens it to text for the wire). For whoever
    /// diagnoses locally —`norte doctor`— and wants to distinguish a broken
    /// manifest from a binary compiled against a different WIT (ADR 0094).
    #[must_use]
    pub fn load_errors(&self) -> &[norte_plugin_host::LoadError] {
        &self.catalog.errors
    }

    /// The config directory where `plugins-state.toml` lives. The daemon uses
    /// it to persist OUTSIDE the lock (rule 2): capture the dir under the
    /// lock and write in `spawn_blocking`.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// EFFECTIVE `[config]` values (P2) for `id`: the manifest schema's
    /// defaults with `config.toml` already overlaid and validated — resolved
    /// at discovery time ([`norte_plugin_host::Catalog::load_dir`], which
    /// excludes from `errors` any plugin whose `config.toml` does not
    /// validate, so what arrives here is ALWAYS valid). `None` if `id` is not
    /// in the catalog — NEVER because of an empty/absent `[config]`, which
    /// gives `Some` of an empty map (same criterion as
    /// [`norte_plugin_host::Manifest::config`]).
    ///
    /// Host-side ONLY (P2 decision 5): it does not cross the wire directly —
    /// it is consumed by `norte doctor` (which runs embedded) and, since
    /// G3c, by [`Self::config_keys`] (which DOES cross the wire via
    /// `plugin.get_config`).
    #[must_use]
    pub fn settings_of(&self, id: &str) -> Option<&BTreeMap<String, String>> {
        self.catalog
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .map(|p| &p.settings)
    }

    /// `id`'s `help.md`, CAPPED for the wire (H3e).
    ///
    /// `None` if `id` is not in the catalog. That is what makes the call
    /// safe: `id` comes from the WIRE and is used as a LOOKUP KEY against the
    /// discovered plugins, never composed into a path — the path comes from
    /// the `dir` the catalog stored on discovery, so a `../` in the id never
    /// touches the filesystem, it just fails the lookup.
    ///
    /// The file must CANONICALIZE INSIDE the plugin's directory
    /// ([`norte_plugin_host::verified_child`], the same guard as
    /// `plugin.wasm`): a `help.md` that is a symlink to `~/.ssh/id_ed25519`
    /// or to `/etc/…` is read as if there were no page. The reason is that
    /// this crosses the wire and an AGENT can request it: without the guard,
    /// `plugin.help` would be an arbitrary file read OUTSIDE the policy
    /// engine and its scopes.
    ///
    /// What makes the opening safe is NOT an approval: `help_of` is NOT
    /// gated by `approved`/`enabled` (the documentation is exactly what is
    /// read BEFORE approving), so the plugin's directory was DISCOVERED, not
    /// consented to. What is safe is the conjunction of three things: the
    /// content is CAPPED (`HelpJob::read`), the path is NOT controlled by
    /// the caller (it comes from the catalog, not from the wire), and the
    /// guard prevents it from pointing outside the directory where the human
    /// already dropped the bundle.
    ///
    /// A known plugin ALWAYS returns `Some`, even if its `help.md` is
    /// missing, unreadable, or escapes the directory: in those cases
    /// `markdown` is the empty string. Help is cosmetic and has no reason to
    /// be distinguished from "blank page" — the one that DOES distinguish it
    /// is `norte doctor`, which gates on [`Self::announces_help`] (the LAX
    /// flag, without the guard) and takes the content from here, and so can
    /// report the file as absent, unreadable, or escaped; from the reader's
    /// side, a `help.md` that points outside is indistinguishable from an
    /// author who wrote nothing, and that deserves a warning.
    ///
    /// The text it returns is NOT masked: it carries verbatim whatever
    /// terminal dangers the plugin wrote (ESC, C0 controls, bidi overrides).
    /// It is parsed with `norte_help::parse_untrusted`, which masks while
    /// building the model; it is never painted or logged raw.
    ///
    /// SYNCHRONOUS I/O: the async caller goes through `help_job` +
    /// `spawn_blocking` (rule 2), which also takes the verification out of
    /// the lock.
    #[must_use]
    pub fn help_of(&self, id: &str) -> Option<norte_proto::methods::PluginHelpResult> {
        self.help_job(id).map(HelpJob::read)
    }

    /// The job of reading `id`'s `help.md`, resolved against the catalog but
    /// WITHOUT touching the disk yet (H3e). `None` if `id` is not discovered.
    ///
    /// It is the half of [`Self::help_of`] that can be done under a lock:
    /// here there is only an in-memory lookup. The verification (three
    /// syscalls) and the read live in [`HelpJob::read`], which the async
    /// caller runs in `spawn_blocking` with the lock already released (rule
    /// 2).
    ///
    /// Returns an OPAQUE value on purpose: the `dir` it carries inside is not
    /// accessible, so whoever receives it cannot re-derive a path from the
    /// wire's id nor skip the guard. The guarantee stays entirely inside the
    /// registry.
    #[must_use]
    pub(crate) fn help_job(&self, id: &str) -> Option<HelpJob> {
        let entry = self.catalog.plugins.iter().find(|e| e.manifest.id == id)?;
        Some(HelpJob {
            dir: entry.dir.clone(),
        })
    }

    /// `true` if `id` carries a `help.md` file, WITHOUT applying the escape
    /// guard (H3e): the LAX flag, the `is_file` that follows links.
    ///
    /// It exists because there are two distinct questions and one alone does
    /// not serve both. `PluginInfo::has_help`, which crosses the WIRE, is the
    /// STRICT one (the same guard as the reader: announcing `true` and
    /// serving `""` would be a path oracle). A local DIAGNOSIS needs the lax
    /// one: "the author put in a `help.md` and the host refuses to serve it"
    /// is exactly the finding that needs to be reported, and with the strict
    /// one that case disappears without a trace — it becomes indistinguishable
    /// from a plugin that was never documented.
    ///
    /// Nothing that answers over the wire should use it.
    #[must_use]
    pub fn announces_help(&self, id: &str) -> bool {
        self.catalog
            .plugins
            .iter()
            .any(|e| e.manifest.id == id && e.help.is_present())
    }

    /// `id`'s `[config]` schema + EFFECTIVE values, PAIRED in the manifest's
    /// key order (0.28.0, G3c): the source that feeds `plugin.get_config` —
    /// each `(key, spec, value)` is translated 1:1 to a `PluginConfigKeyWire`
    /// in `norte-core/daemon/server.rs`. `None` if `id` is not in the catalog
    /// (same criterion as [`Self::settings_of`]); an empty/absent `[config]`
    /// gives `Some(vec![])`, never `None` — the catalog DOES know the
    /// plugin, it just declares no key.
    ///
    /// Invariant: `entry.settings` (resolved by
    /// [`norte_plugin_host::resolve_settings`] on discovery) ALWAYS contains
    /// a value for every key in `entry.manifest.config` — an
    /// `unwrap_or_default` would cover a violation of that invariant without
    /// panicking (defense in depth, should never trigger in practice).
    #[must_use]
    pub fn config_keys(
        &self,
        id: &str,
    ) -> Option<Vec<(String, norte_plugin_host::ConfigKeySpec, String)>> {
        let entry = self.catalog.plugins.iter().find(|p| p.manifest.id == id)?;
        Some(
            entry
                .manifest
                .config
                .iter()
                .map(|(key, spec)| {
                    let value = entry.settings.get(key).cloned().unwrap_or_default();
                    (key.clone(), spec.clone(), value)
                })
                .collect(),
        )
    }

    /// Validates `value` against `id`'s `[config.<key>]` schema (the SAME
    /// validation as `config.toml`, via
    /// [`norte_plugin_host::encode_wire_value`]) and, if it passes, persists
    /// + RE-RESOLVES `settings_of`/[`Self::config_keys`] IN MEMORY so a
    /// future instantiation (or a `plugin.get_config` right after) sees the
    /// new value (0.28.0, G3c). Never persists if validation fails (spec S2:
    /// "validated against the schema BEFORE writing").
    ///
    /// # Errors
    /// [`PluginConfigSetError::Unknown`] if `id` is not in the catalog;
    /// [`PluginConfigSetError::UnknownKey`] if `key` is not declared in
    /// `[config]`; [`PluginConfigSetError::Invalid`] if the value does not
    /// validate against the key's type/range/enum;
    /// [`PluginConfigSetError::Io`] if the write or the re-resolution after
    /// writing fails.
    pub fn set_config(
        &mut self,
        id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), PluginConfigSetError> {
        let idx = self
            .catalog
            .plugins
            .iter()
            .position(|p| p.manifest.id == id)
            .ok_or_else(|| PluginConfigSetError::Unknown(id.to_string()))?;
        let spec = self.catalog.plugins[idx]
            .manifest
            .config
            .get(key)
            .cloned()
            .ok_or_else(|| PluginConfigSetError::UnknownKey(key.to_string()))?;
        norte_plugin_host::encode_wire_value(key, &spec, value)
            .map_err(PluginConfigSetError::Invalid)?;
        norte_plugin_host::persist_plugin_setting_typed(&self.config_dir, id, key, &spec, value)
            .map_err(PluginConfigSetError::Io)?;
        let manifest = self.catalog.plugins[idx].manifest.clone();
        let dir = self.catalog.plugins[idx].dir.clone();
        let refreshed = norte_plugin_host::resolve_settings(&manifest, &dir).map_err(|e| {
            PluginConfigSetError::Io(io::Error::other(format!(
                "re-resolving config after write: {e}"
            )))
        })?;
        self.catalog.plugins[idx].settings = refreshed;
        Ok(())
    }

    /// `id`'s expected binary path: `<config_dir>/plugins/<id>/plugin.wasm`.
    /// For a caller that only needs to check PRESENCE without loading the
    /// WASM runtime (e.g. `norte doctor`, H2) — keeps that caller from
    /// duplicating the layout with its own `config_dir.join("plugins")...`.
    /// It is NOT the same path as `Self::verified_wasm` (which additionally
    /// canonicalizes and verifies the binary does not escape the plugin's
    /// directory via a symlink, issue #69 — a defense this pure path
    /// computation does not apply) nor does it consult the catalog: by
    /// convention (`PluginEntry::dir`'s own rustdoc) a discovered plugin's
    /// directory is `plugins/<id>/`, but this function does not verify that,
    /// it only assumes it.
    #[must_use]
    pub fn wasm_path(&self, id: &str) -> PathBuf {
        self.config_dir.join("plugins").join(id).join("plugin.wasm")
    }

    /// Copy of the approved/enabled state, to persist outside the lock (the
    /// daemon moves it to `spawn_blocking` alongside [`Self::config_dir`],
    /// rule 2).
    #[must_use]
    pub fn state_snapshot(&self) -> BTreeMap<String, PluginState> {
        self.state.clone()
    }

    /// Mutates a discovered plugin's `approved` state IN MEMORY, WITHOUT I/O.
    ///
    /// Returns `true` if the plugin exists in the catalog (and it was
    /// applied), or `false` if the id is unknown — in which case nothing is
    /// touched (the state is not dirtied with phantom plugins). Persistence
    /// is the caller's responsibility (daemon: `persist_state` in
    /// `spawn_blocking`; embedded: [`Self::set_approval`]).
    pub fn set_approval_in_memory(&mut self, id: &str, approved: bool) -> bool {
        // The digest of the capabilities the human is looking at RIGHT NOW is
        // anchored (issue #69): if `plugin.toml` changes afterward, the
        // digest will stop matching and `resolve_*` will ask for consent
        // again. Requires the id to exist in the catalog (otherwise there is
        // no manifest to digest).
        let Some(digest) = self.manifest_digest(id) else {
            return false;
        };
        let st = self.state.entry(id.to_string()).or_default();
        st.approved = approved;
        // The seen digest is saved when approving; it is wiped when
        // revoking (a future re-approval will anchor it again).
        st.approved_digest = approved.then_some(digest);
        true
    }

    /// Mutates the `enabled` state IN MEMORY. Identical semantics to
    /// [`Self::set_approval_in_memory`].
    pub fn set_enabled_in_memory(&mut self, id: &str, enabled: bool) -> bool {
        if !self.is_known(id) {
            return false;
        }
        self.state.entry(id.to_string()).or_default().enabled = enabled;
        true
    }

    /// Forgets IN MEMORY a plugin that [`uninstall`] just deleted from disk:
    /// it leaves the catalog and its state is left disabled and unapproved,
    /// which is exactly what `uninstall` left written in
    /// `plugins-state.toml`.
    ///
    /// Returns `true` if it was in the catalog. A BROKEN plugin —which
    /// `uninstall` deletes just the same— is not in `plugins` but in
    /// `errors`, and is forgotten from there: otherwise the daemon kept
    /// announcing it as "failed to load" until restart, the same corpse
    /// under another name. Without this the daemon kept listing what was
    /// deleted, and decorating with it, until restart.
    ///
    /// An id that is not an id touches nothing: the state entry that gets
    /// inserted is written to `plugins-state.toml` on the next persist, as a
    /// key, and this function is `pub`.
    pub fn forget_in_memory(&mut self, id: &str) -> bool {
        if !norte_plugin_host::is_valid_plugin_id(id) {
            return false;
        }
        let before = self.catalog.plugins.len();
        self.catalog.plugins.retain(|e| e.manifest.id != id);
        self.catalog
            .errors
            .retain(|e| e.dir.file_name() != Some(std::ffi::OsStr::new(id)));
        self.state.insert(id.to_owned(), PluginState::default());
        self.catalog.plugins.len() != before
    }

    /// Sets a discovered plugin's `approved` state and persists it, all on
    /// the SAME thread. It is the API for EMBEDDED use, which already runs
    /// inside a `spawn_blocking` (the frontend's backend). The daemon does
    /// NOT use this: it separates the mutation
    /// ([`Self::set_approval_in_memory`]) from the persistence
    /// (`persist_state`) so as not to block the reactor (rule 2).
    ///
    /// Returns `Ok(true)` if the plugin exists (and it was
    /// applied+persisted), or `Ok(false)` if the id is unknown — without
    /// persisting anything.
    ///
    /// # Errors
    /// I/O errors re-reading or writing `plugins-state.toml`, or
    /// [`io::ErrorKind::InvalidData`] if the existing file is corrupt TOML.
    pub fn set_approval(&mut self, id: &str, approved: bool) -> io::Result<bool> {
        if !self.set_approval_in_memory(id, approved) {
            return Ok(false);
        }
        persist_state(&self.config_dir, &self.state)?;
        Ok(true)
    }

    /// Sets a discovered plugin's `enabled` state and persists it (EMBEDDED
    /// use). Return semantics identical to [`Self::set_approval`].
    ///
    /// # Errors
    /// Same as [`Self::set_approval`].
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> io::Result<bool> {
        if !self.set_enabled_in_memory(id, enabled) {
            return Ok(false);
        }
        persist_state(&self.config_dir, &self.state)?;
        Ok(true)
    }

    /// Validates consent (fail-closed) and RESOLVES a plugin's `.wasm` +
    /// capabilities + ALREADY resolved `[config]`, WITHOUT running it. It is
    /// CHEAP (catalog/state read in memory + an `is_file`): meant to run
    /// under the daemon's `Mutex<PluginRegistry>`, which afterward runs the
    /// HEAVY part (`PluginRuntime::instantiate` + `run_command`, which
    /// compiles the WASM component) OUTSIDE the lock, in a `spawn_blocking`
    /// (rule 2). The `.wasm` is `<dir>/plugin.wasm` by convention (ADR 0022
    /// D6); the capabilities are the MANIFEST's (M4-P2's sandbox enforces
    /// them).
    ///
    /// `settings` (P2 Task 4a) are the `[config]` values ALREADY resolved
    /// (Task 2) — the caller must pass them to
    /// `PluginInstance::set_settings` BEFORE invoking the command so the
    /// guest sees them via `host-config` (Task 3); [`Self::run_command`]
    /// already does this, and so does the daemon's
    /// `handle_plugin_run_command` (which resolves under lock and runs
    /// outside it, unable to reuse `run_command` directly).
    ///
    /// # Errors
    /// [`PluginRunError`] `Unknown`/`NotApproved`/`Disabled`/`NoBinary`
    /// according to the consent verdict; never `Runtime` (it runs nothing).
    pub fn resolve_runnable(
        &self,
        id: &str,
    ) -> Result<
        (
            norte_plugin_host::WasmArtifact,
            norte_plugin_host::Capabilities,
            BTreeMap<String, String>,
        ),
        PluginRunError,
    > {
        let entry = self
            .catalog
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| PluginRunError::Unknown(id.to_string()))?;
        // Only the `norte-plugin` world exports `command`; the other kinds
        // have nothing to run, and saying so here avoids instantiating for
        // nothing.
        if !matches!(
            entry.manifest.category,
            norte_plugin_host::Category::Command | norte_plugin_host::Category::Previewer
        ) {
            return Err(PluginRunError::NotRunnable(id.to_string()));
        }
        let st = self.state.get(id).cloned().unwrap_or_default();
        // Fail-closed: without a current approval whose digest MATCHES the
        // current capabilities (issue #69), it is treated as not approved —
        // even if the `approved` flag is still `true` on disk (the manifest
        // changed after approving).
        if !Self::approval_is_current(&st, entry) {
            return Err(PluginRunError::NotApproved(id.to_string()));
        }
        if !st.enabled {
            return Err(PluginRunError::Disabled(id.to_string()));
        }
        let wasm =
            Self::verified_wasm(entry).ok_or_else(|| PluginRunError::NoBinary(id.to_string()))?;
        Ok((
            wasm,
            entry.manifest.capabilities.clone(),
            entry.settings.clone(),
        ))
    }

    /// Resolves the APPROVED and ENABLED previewer that declares `mime`,
    /// returning `(id, name, wasm_path, capabilities, settings)`; `None` if
    /// none applies. Fail-closed: a non-consented previewer is never chosen.
    /// Cheap: the caller reads the file's bytes and runs outside the lock.
    ///
    /// **Exact before glob** (D3, ADR 0037 amendment): a plugin that declares
    /// `text/markdown` beats one that declares `text/*` for a `.md`, wherever
    /// it is in the catalog's order; among equals, the first by catalog
    /// order (`category, id`). Without this, which one painted a Markdown
    /// was decided by the alphabet of the ids.
    ///
    /// `settings` (P2 Task 4a) are the `[config]` values ALREADY resolved
    /// ([`Self::settings_of`]) — the caller must pass them to
    /// `PluginInstance::set_settings` BEFORE `render_preview` so the guest
    /// sees them via `host-config` (Task 3), the same as [`Self::run_command`]
    /// already does for commands.
    #[must_use]
    pub fn resolve_previewer(&self, mime: &str) -> Option<ResolvedPreviewer> {
        // Two passes: the exact one wins over the wildcard even if it comes
        // later.
        let exact = self.previewer_matching(|pat| pat == mime);
        exact.or_else(|| self.previewer_matching(|pat| mimetype_matches(pat, mime)))
    }

    /// The first consented THUMBNAIL plugin that matches `mime` (ADR 0107),
    /// with the same rules as [`Self::resolve_previewer`]: the exact
    /// declaration beats the wildcard one, and only ones approved with a
    /// current digest and enabled are considered.
    #[must_use]
    pub fn resolve_thumbnailer(&self, mime: &str) -> Option<ResolvedPreviewer> {
        let exact = self.thumbnailer_matching(|pat| pat == mime);
        exact.or_else(|| self.thumbnailer_matching(|pat| mimetype_matches(pat, mime)))
    }

    /// The consented plugin that paints that panel, if there is one (0.74.0,
    /// phase 3).
    ///
    /// By id AND kind, not by order: a panel opens by its slot name
    /// (`plugin:<id>:<kind>`), so there is nothing here to resolve by
    /// priority — either that plugin offers that panel, or there is no
    /// frame.
    ///
    /// Fail-closed with the current digest, like the others: a plugin whose
    /// manifest changed after being approved does not paint until consent is
    /// given again.
    #[must_use]
    pub fn resolve_panel(&self, plugin_id: &str, kind: &str) -> Option<ResolvedPreviewer> {
        self.catalog.plugins.iter().find_map(|e| {
            if e.manifest.id != plugin_id {
                return None;
            }
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            if !e
                .manifest
                .contributions
                .panel
                .iter()
                .any(|p| p.kind == kind)
            {
                return None;
            }
            let wasm = Self::verified_wasm(e)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                e.settings.clone(),
            ))
        })
    }

    fn thumbnailer_matching(
        &self,
        matches_pat: impl Fn(&str) -> bool,
    ) -> Option<ResolvedPreviewer> {
        self.catalog.plugins.iter().find_map(|e| {
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            let handles = e
                .manifest
                .contributions
                .thumbnail
                .iter()
                .flat_map(|c| c.mimetypes.iter())
                .any(|pat| matches_pat(pat));
            if !handles {
                return None;
            }
            let wasm = Self::verified_wasm(e)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                e.settings.clone(),
            ))
        })
    }

    /// The first consented previewer, in catalog order, with some mimetype
    /// declaration that satisfies `matches_pat`.
    fn previewer_matching(&self, matches_pat: impl Fn(&str) -> bool) -> Option<ResolvedPreviewer> {
        self.catalog.plugins.iter().find_map(|e| {
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            // Fail-closed with a current digest (issue #69): a previewer
            // whose manifest changed after approval is NOT chosen until
            // consent is given again.
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            let handles = e
                .manifest
                .contributions
                .previewer
                .iter()
                .flat_map(|c| c.mimetypes.iter())
                .any(|pat| matches_pat(pat));
            if !handles {
                return None;
            }
            let wasm = Self::verified_wasm(e)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                e.settings.clone(),
            ))
        })
    }

    /// Resolves ALL APPROVED and ENABLED decorators (ADR 0037 decision 2),
    /// unlike [`Self::resolve_previewer`] (which chooses the FIRST one that
    /// matches): a page is decorated with the overlay of ALL consented
    /// `decorator` plugins — a "modified by git" badge and a "under review"
    /// one can coexist on the same entry. Filters by `category == Decorator`
    /// (unlike `resolve_previewer`, which does not filter by category
    /// because previewer/command share the SAME `norte-plugin` world;
    /// decorator has its OWN `norte-decorator` world, so only a plugin whose
    /// binary implements it should enter here). Order: the catalog's
    /// (`category, id` — deterministic, see [`Catalog::load_dir`]).
    #[must_use]
    pub fn resolve_decorators(&self) -> Vec<(ResolvedDecorator, norte_plugin_host::DecoratorSlot)> {
        self.catalog
            .plugins
            .iter()
            .filter_map(|e| {
                if e.manifest.category != norte_plugin_host::Category::Decorator {
                    return None;
                }
                let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
                if !Self::approval_is_current(&st, e) || !st.enabled {
                    return None;
                }
                let wasm = Self::verified_wasm(e)?;
                // The FIRST contribution says the slot: a decorator declares
                // one, and if it declared two with different slots there
                // would be no way to know which of its responses goes to
                // which.
                let slot = e
                    .manifest
                    .contributions
                    .decorator
                    .first()
                    .map_or(norte_plugin_host::DecoratorSlot::Badge, |d| d.slot);
                Some((
                    (
                        e.manifest.id.clone(),
                        e.manifest.name.clone(),
                        wasm,
                        e.manifest.capabilities.clone(),
                        e.settings.clone(),
                    ),
                    slot,
                ))
            })
            .collect()
    }

    /// Resolves the APPROVED and ENABLED provider plugin that declares
    /// `scheme` in `contributions.provider[].scheme`: the one that serves
    /// `scheme://`.
    ///
    /// Until now a provider was declared, approved and enabled, and NOBODY
    /// resolved it: the `ConnectionManager` matched schemes by hand against
    /// the core's providers and an embedded FTP guest. This is the half of
    /// the registry that was missing; the other half is the manager asking.
    ///
    /// First one that matches, like [`Self::resolve_columns`]: two consented
    /// plugins claiming the same scheme is a user configuration collision,
    /// and the catalog's order (`category, id`) at least makes it
    /// deterministic. Filters by `category == Provider`, the same dedicated
    /// world reasoning as [`Self::resolve_decorators`]: only a binary that
    /// implements `norte-provider` should be instantiated as one.
    ///
    /// The core's schemes ([`norte_plugin_host::CORE_SCHEMES`]) are NEVER
    /// served from here, even if a catalog entry declares them: the manifest
    /// already rejects them when parsing, and this is the second gate, the
    /// one that can be tested without going through the first.
    ///
    /// A plugin that declares the scheme but is not consented to is logged:
    /// the answer to the user is `Unsupported` —the same as a scheme nobody
    /// serves— and the log panel is where the reason is read.
    #[must_use]
    pub fn resolve_provider(&self, scheme: &str) -> Option<ResolvedProvider> {
        if norte_plugin_host::CORE_SCHEMES.contains(&scheme) {
            return None;
        }
        self.catalog.plugins.iter().find_map(|e| {
            if e.manifest.category != norte_plugin_host::Category::Provider {
                return None;
            }
            let contrib = e
                .manifest
                .contributions
                .provider
                .iter()
                .find(|c| c.scheme == scheme)?;
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                tracing::warn!(
                    plugin = %e.manifest.id,
                    scheme,
                    "declares the scheme but is not approved and enabled"
                );
                return None;
            }
            let wasm = Self::verified_wasm(e)?;
            let wasm_digest = e.wasm_digest.clone()?;
            Some(ResolvedProvider {
                id: e.manifest.id.clone(),
                name: e.manifest.name.clone(),
                wasm,
                wasm_digest,
                capabilities: e.manifest.capabilities.clone(),
                settings: e.settings.clone(),
                default_port: contrib.default_port,
            })
        })
    }

    /// Resolves the APPROVED and ENABLED `columns` plugin that declares the
    /// `column_id` column in `contributions.columns[].id` (M4 declared the
    /// contribution, ADR 0037 backs it with WIT/host). Unlike
    /// `resolve_decorators`, here the first one that matches DOES suffice (a
    /// column with that id is contributed by at most one plugin that makes
    /// sense — two plugins declaring the SAME column id is a user
    /// configuration collision, not something this method should resolve by
    /// mixing values). Filters by `category == Columns`, the same dedicated
    /// world reasoning as [`Self::resolve_decorators`].
    #[must_use]
    pub fn resolve_columns(&self, column_id: &str) -> Option<ResolvedDecorator> {
        self.resolve_columns_of(None, column_id)
    }

    /// The consented `renamer` plugin `plugin_id` that declares `renamer_id`
    /// (C3, ADR 0095): THAT one or none, approved and on, with its `.wasm`
    /// verified against the approved digest. Same shape as
    /// [`Self::resolve_columns_of`] with the plugin required.
    #[must_use]
    pub fn resolve_renamer(&self, plugin_id: &str, renamer_id: &str) -> Option<ResolvedDecorator> {
        self.catalog.plugins.iter().find_map(|e| {
            if e.manifest.category != norte_plugin_host::Category::Renamer
                || e.manifest.id != plugin_id
            {
                return None;
            }
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            if !e
                .manifest
                .contributions
                .renamer
                .iter()
                .any(|r| r.id == renamer_id)
            {
                return None;
            }
            let wasm = Self::verified_wasm(e)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                self.settings_of(&e.manifest.id)
                    .cloned()
                    .unwrap_or_default(),
            ))
        })
    }

    /// The consented `organizer` plugin `plugin_id` that declares
    /// `organizer_id` (phase 8): THAT one or none, approved and on, with its
    /// `.wasm` verified against the approved digest. The exact same shape as
    /// [`Self::resolve_renamer`], with its category.
    #[must_use]
    pub fn resolve_organizer(
        &self,
        plugin_id: &str,
        organizer_id: &str,
    ) -> Option<ResolvedDecorator> {
        self.catalog.plugins.iter().find_map(|e| {
            if e.manifest.category != norte_plugin_host::Category::Organizer
                || e.manifest.id != plugin_id
            {
                return None;
            }
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            if !e
                .manifest
                .contributions
                .organizer
                .iter()
                .any(|r| r.id == organizer_id)
            {
                return None;
            }
            let wasm = Self::verified_wasm(e)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                self.settings_of(&e.manifest.id)
                    .cloned()
                    .unwrap_or_default(),
            ))
        })
    }

    /// The consented `hook` plugins (ADR 0100), each with the events it
    /// listens to, in catalog order. Same shape as [`Self::resolve_renamer`]:
    /// approved and on, `.wasm` verified against the approved digest, and
    /// with its settings resolved.
    #[must_use]
    pub fn resolve_hooks(&self) -> Vec<(ResolvedDecorator, Vec<String>)> {
        self.catalog
            .plugins
            .iter()
            .filter(|e| e.manifest.category == norte_plugin_host::Category::Hook)
            .filter_map(|e| {
                let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
                if !Self::approval_is_current(&st, e) || !st.enabled {
                    return None;
                }
                let wasm = Self::verified_wasm(e)?;
                let ons = e
                    .manifest
                    .contributions
                    .hook
                    .iter()
                    .map(|h| h.on.clone())
                    .collect();
                Some((
                    (
                        e.manifest.id.clone(),
                        e.manifest.name.clone(),
                        wasm,
                        e.manifest.capabilities.clone(),
                        self.settings_of(&e.manifest.id)
                            .cloned()
                            .unwrap_or_default(),
                    ),
                    ons,
                ))
            })
            .collect()
    }

    /// Like [`Self::resolve_columns`], but able to demand WHICH plugin
    /// (0.35.0, #120).
    ///
    /// With `plugin_id = Some(p)` only `p` is considered: if it is not
    /// approved, enabled, or does not declare `column_id`, the answer is
    /// `None` — NEVER another plugin. Falling back to the first one that
    /// matches would be the original bug with one more parameter: two
    /// consented plugins declaring `status` made a column configured as
    /// `plugin:a/status` paint `b`'s values, and no layer noticed because
    /// each checked its own thing (the frontend, that the configured plugin
    /// declares the column; the host, that someone declares it).
    ///
    /// With `plugin_id = None` the previous behavior is kept — first one
    /// that matches — because that is what a 0.34 client expects, and what
    /// it already put up with.
    #[must_use]
    pub fn resolve_columns_of(
        &self,
        plugin_id: Option<&str>,
        column_id: &str,
    ) -> Option<ResolvedDecorator> {
        self.catalog.plugins.iter().find_map(|e| {
            if e.manifest.category != norte_plugin_host::Category::Columns {
                return None;
            }
            if plugin_id.is_some_and(|want| want != e.manifest.id) {
                return None;
            }
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            let declares = e
                .manifest
                .contributions
                .columns
                .iter()
                .any(|c| c.id == column_id);
            if !declares {
                return None;
            }
            let wasm = Self::verified_wasm(e)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                e.settings.clone(),
            ))
        })
    }

    /// Runs an APPROVED and ENABLED plugin's command (fail-closed: a
    /// non-consented plugin is NEVER run). Delegates validation to
    /// [`Self::resolve_runnable`] and runs it right after. SYNCHRONOUS
    /// (compiles and instantiates the component): the caller runs it in
    /// `spawn_blocking` (rule 2). The daemon prefers to separate resolution
    /// (under lock) from execution (outside the lock) by calling
    /// [`Self::resolve_runnable`] directly — its `handle_plugin_run_command`
    /// delivers `settings` the SAME way, just in two steps instead of one
    /// call to this method.
    ///
    /// Delivers to the guest the `[config]` values ALREADY resolved that
    /// [`Self::resolve_runnable`] returns (P2 Task 2) via `host-config` (P2
    /// Task 3) BEFORE invoking the command — a plugin with no `[config]`
    /// receives the empty map.
    ///
    /// # Errors
    /// [`PluginRunError`] if the plugin does not exist, is not approved, is
    /// disabled, has no binary, or the runtime fails.
    pub fn run_command(
        &self,
        runtime: &norte_plugin_host::PluginRuntime,
        id: &str,
        command: &str,
        arg: &str,
    ) -> Result<String, PluginRunError> {
        let (wasm, caps, settings) = self.resolve_runnable(id)?;
        let mut inst = runtime.instantiate(&wasm, caps)?;
        inst.set_settings(settings);
        Ok(inst.run_command(command, arg)?)
    }

    /// `true` if `id` corresponds to an actually discovered plugin.
    fn is_known(&self, id: &str) -> bool {
        self.catalog.plugins.iter().any(|e| e.manifest.id == id)
    }

    /// `id`'s approval anchor as it is NOW in the catalog, or `None` if the
    /// id is not discovered (issue #69).
    ///
    /// Covers the MANIFEST —capabilities, `category` and `contributions`,
    /// i.e. what it asks for and when it fires— **and the binary** (#241).
    ///
    /// It is what anchors an approval when giving it, what the daemon
    /// compares to answer "is it still the one you showed me?" (#282), and
    /// what `plugin.list` puts in `PluginInfo::manifest_digest`.
    #[must_use]
    pub fn manifest_digest(&self, id: &str) -> Option<String> {
        self.catalog
            .plugins
            .iter()
            .find(|e| e.manifest.id == id)
            .map(norte_plugin_host::PluginEntry::approval_anchor)
    }

    /// `true` if the approval is CURRENT (issue #69): the human approved AND
    /// the stored anchor matches the one NOW — the manifest (capabilities,
    /// `category`, `contributions`: what it asks for and when it fires)
    /// **and the binary** (#241). An absent `approved_digest` (an approval
    /// inherited without an anchor) NEVER matches → re-consent.
    fn approval_is_current(st: &PluginState, entry: &norte_plugin_host::PluginEntry) -> bool {
        st.approved && st.approved_digest.as_deref() == Some(entry.approval_anchor().as_str())
    }

    /// Resolves `<dir>/plugin.wasm` and verifies, by canonicalizing, that the
    /// real binary falls INSIDE the plugin's directory (issue #69, defense
    /// in depth against a `plugin.wasm` that is a symlink to `/etc/...` or to
    /// another plugin). `None` if it does not exist, is not a file, or
    /// escapes the dir. Note: whoever can write the symlink can already
    /// replace the whole binary (same trust boundary), which is why this is
    /// defense in depth, not a strong barrier. Returns the CANONICAL (already
    /// resolved) path so links are not re-followed when opening it.
    /// The guard lives in `norte-plugin-host` (see
    /// [`norte_plugin_host::verified_child`], which documents what it does
    /// NOT cover): the catalog needs it at discovery and this crate when
    /// reading or running, and a second copy of a security guard is worse
    /// than the dependency.
    ///
    /// And with the FINGERPRINT the catalog anchored (ADR 0142): the runtime
    /// compares the bytes it compiles against it. With no fingerprint —a
    /// binary that could not be read at discovery— there is no artifact,
    /// which is the same as having no binary.
    fn verified_wasm(
        e: &norte_plugin_host::PluginEntry,
    ) -> Option<norte_plugin_host::WasmArtifact> {
        let path = norte_plugin_host::verified_child(&e.dir, "plugin.wasm")?;
        let digest = e.wasm_digest.clone()?;
        Some(norte_plugin_host::WasmArtifact::approved(path, digest))
    }

    /// Reads the persisted state. Absent = empty; corrupt = `InvalidData`.
    fn read_state(path: &Path) -> io::Result<BTreeMap<String, PluginState>> {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
        };
        let doc = src
            .parse::<DocumentMut>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut map = BTreeMap::new();
        if let Some(plugins) = doc.get("plugins").and_then(Item::as_table_like) {
            for (key, item) in plugins.iter() {
                let Some(tbl) = item.as_table_like() else {
                    continue;
                };
                let flag = |name: &str| tbl.get(name).and_then(Item::as_bool).unwrap_or(false);
                let digest = tbl.get("digest").and_then(Item::as_str).map(str::to_owned);
                map.insert(
                    key.to_string(),
                    PluginState {
                        approved: flag("approved"),
                        enabled: flag("enabled"),
                        approved_digest: digest,
                    },
                );
            }
        }
        Ok(map)
    }
}

/// Numbers [`persist_state`]'s temp files within this process: two writes in
/// flight from two threads with the same name were half-renaming over one
/// another.
static WRITE_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Re-emits `config_dir/plugins-state.toml` preserving the rest of the file,
/// with one entry per plugin that has state. A key with dots is quoted.
///
/// It is a FREE function (not a method) so the daemon can persist in a
/// `spawn_blocking` from a snapshot of the state, without holding the
/// `Mutex<PluginRegistry>` across the `.await` (rule 2).
///
/// The write is ATOMIC: it writes to a temp file in the SAME directory and
/// then `rename`s onto the destination. A crash midway does not corrupt the
/// durable store for a security decision (capability consent).
///
/// # Errors
/// I/O errors re-reading, writing the temp file, or renaming; or
/// [`io::ErrorKind::InvalidData`] if the existing file is corrupt TOML.
pub(crate) fn persist_state(
    config_dir: &Path,
    state: &BTreeMap<String, PluginState>,
) -> io::Result<()> {
    let path = config_dir.join(PluginRegistry::STATE_FILE);
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .parse::<DocumentMut>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => DocumentMut::new(),
        Err(e) => return Err(e),
    };
    let root = doc.as_table_mut();
    if !root.get("plugins").is_some_and(Item::is_table_like) {
        root.insert("plugins", Item::Table(Table::new()));
    }
    // `insert` with a key that has dots stores the LITERAL key; toml_edit
    // quotes it on render (it does not interpret it as nested tables).
    let plugins = root["plugins"]
        .as_table_mut()
        .expect("plugins is a table: just guaranteed above");
    for (id, st) in state {
        let mut inline = InlineTable::new();
        inline.insert("approved", Value::from(st.approved));
        inline.insert("enabled", Value::from(st.enabled));
        // The capabilities digest anchored to the approval (issue #69)
        // persists alongside the flag; without it a re-discover could not
        // revalidate the consent and would force re-approval on every
        // startup.
        if let Some(digest) = &st.approved_digest {
            inline.insert("digest", Value::from(digest.clone()));
        }
        plugins.insert(id, Item::Value(Value::InlineTable(inline)));
    }
    // Atomic write: temp file in the same dir (same filesystem → atomic
    // rename) + rename onto the destination. The pid suffix avoids stepping
    // on the temp file of another process persisting at the same time; the
    // counter, on that of another THREAD of this one (the daemon persists
    // from `spawn_blocking`, and two writes in flight with the same name
    // were half-renaming over one another).
    let tmp = config_dir.join(format!(
        "{}.tmp.{}.{}",
        PluginRegistry::STATE_FILE,
        std::process::id(),
        WRITE_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// #282: the anchor a human READS has to change when what was shown to
    /// them changes, or `expected_digest` protects against nothing.
    ///
    /// This is the field's premise, not its use: the use is in the daemon
    /// (`handle_plugin_set_approval`) and in the embedded `Backend`, which
    /// is where there really is a window — it rediscovers the catalog on
    /// EVERY call.
    #[test]
    fn a_manifests_anchor_changes_when_what_it_declares_changes() {
        const BEFORE: &str = r#"
[plugin]
id = "org.norte.anchor"
name = "Anchor"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;
        // What changes is NOT the capabilities: it is `contributions`, i.e.
        // WHEN and HOW it fires. That is exactly what the painted list does
        // not say and the anchor does cover — that is why the client's
        // capabilities comparison is not enough.
        const AFTER: &str = r#"
[plugin]
id = "org.norte.anchor"
name = "Anchor"
publisher = "norte"
version = "0.1.0"
category = "command"
[contributions]
command = [{ id = "run", title = "Run" }]
"#;
        let cfg = TempDir::new().expect("tempdir");
        let dir = cfg.path().join("plugins").join("org.norte.anchor");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("plugin.toml"), BEFORE).expect("write");
        std::fs::write(dir.join("plugin.wasm"), b"\0asm\x01\0\0\0").expect("write wasm");

        let reg = PluginRegistry::discover(cfg.path()).expect("discover");
        let before = reg
            .manifest_digest("org.norte.anchor")
            .expect("the catalog carries the anchor with no need for a real wasm");

        std::fs::write(dir.join("plugin.toml"), AFTER).expect("rewrite");
        let reg2 = PluginRegistry::discover(cfg.path()).expect("rediscover");
        let after = reg2
            .manifest_digest("org.norte.anchor")
            .expect("still discovered");
        assert_ne!(
            before, after,
            "the anchor did not move: `expected_digest` would not protect \
             against a manifest changed under its feet"
        );
    }

    /// #29/§6.2: `decode_for_preview` delivers decoded TEXT to the previewer;
    /// valid UTF-8 is not lossy (#101).
    #[test]
    fn decode_for_preview_utf8_text_passes_through_unchanged() {
        assert_eq!(
            decode_for_preview(b"hello world".to_vec()),
            (b"hello world".to_vec(), false)
        );
    }

    /// ADR 0141 review: the text arrives at the previewer WHOLE (within the
    /// byte cap), never cut by lines: a silent cut gave a styled view that
    /// looked like the complete file and hid the end.
    #[test]
    fn decode_for_preview_does_not_cut_by_lines() {
        let long: Vec<u8> = (0..20_000)
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect();
        let (text, _) = decode_for_preview(long.clone());
        assert_eq!(text, long, "whole");
    }

    #[test]
    fn decode_for_preview_utf16le_bom_decodes_to_utf8() {
        // UTF-16LE BOM (FF FE) + "hi" → detect Text, decode to UTF-8 "hi".
        let utf16 = vec![0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        assert_eq!(decode_for_preview(utf16), (b"hi".to_vec(), false));
    }

    #[test]
    fn decode_for_preview_binary_passes_raw_bytes_through() {
        // PNG header (controls + NUL): detect Binary → bytes as is, never
        // lossy (#101).
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00".to_vec();
        assert_eq!(decode_for_preview(png.clone()), (png, false));
    }

    /// #101: bytes detected as text but with a sequence INVALID for that
    /// encoding → LOSSY decoding (`�`) flagged `lossy = true`. The needle
    /// lives in the testkit's canonical corpus (`utf8_bom_invalid`), not
    /// inline, per CLAUDE.md's rule on encoding regressions.
    #[test]
    fn decode_for_preview_invalid_text_is_lossy() {
        let fx = norte_testkit::corpus::lossy_content_fixtures()
            .into_iter()
            .find(|f| f.id == "utf8_bom_invalid")
            .expect("lossy corpus carries utf8_bom_invalid");
        let (out, lossy) = decode_for_preview(fx.bytes.clone());
        assert!(lossy, "invalid byte must flag lossy");
        assert_eq!(
            String::from_utf8_lossy(&out),
            fx.decoded,
            "the output is the canonical decode with `�`"
        );
    }

    /// G3a (ADR 0037): `to_wire_lines` reshapes the runtime's type to the
    /// wire's 1:1, WITHOUT validating `role` (that validation lives in the
    /// frontend, see its rustdoc) nor capping sizes again (already capped by
    /// `render_styled_preview`).
    #[test]
    fn to_wire_lines_reshapes_1_to_1_without_validating_role() {
        use norte_plugin_host::previewer_iface::Span;
        let lines = vec![
            vec![
                Span {
                    text: "42".to_owned(),
                    role: Some("number".to_owned()), // not a valid Role: passes through anyway
                    fg: None,
                    bg: None,
                },
                Span {
                    text: " TODO".to_owned(),
                    role: Some("keyword".to_owned()),
                    fg: Some((255, 200, 0)),
                    bg: Some((0, 0, 64)),
                },
            ],
            vec![Span {
                text: "plain".to_owned(),
                role: None,
                fg: None,
                bg: None,
            }],
        ];
        let wire = to_wire_lines(lines);
        assert_eq!(wire.len(), 2, "2 input lines → 2 output lines");
        assert_eq!(wire[0].len(), 2, "spans kept 1:1");
        assert_eq!(wire[0][0].text, "42");
        assert_eq!(
            wire[0][0].role.as_deref(),
            Some("number"),
            "role travels UNVALIDATED (not a valid Role and still passes)"
        );
        assert_eq!(wire[0][0].fg, None);
        assert_eq!(wire[0][1].fg, Some([255, 200, 0]), "tuple → [u8;3] array");
        assert_eq!(wire[1][0].text, "plain");
        assert_eq!(wire[1][0].role, None);
    }

    /// Minimal valid manifest (copied from `norte-plugin-host`'s doctest).
    const DEMO_MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
"#;

    /// Creates `config_dir/plugins/<id>/plugin.toml` with `src`.
    fn write_plugin(config_dir: &Path, id: &str, src: &str) {
        let dir = config_dir.join("plugins").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), src).unwrap();
    }

    /// A provider plugin, with a binary: `resolve_provider` requires the
    /// verified `.wasm` like any other resolver.
    const PROVIDER_MANIFEST: &str = r#"
[plugin]
id = "org.norte.memplug"
name = "Mem plug"
publisher = "norte"
version = "0.1.0"
category = "provider"
[[contributions.provider]]
scheme = "memplug"
"#;

    fn write_provider(config_dir: &Path, id: &str, src: &str) {
        write_plugin(config_dir, id, src);
        std::fs::write(
            config_dir.join("plugins").join(id).join("plugin.wasm"),
            b"\0asm",
        )
        .unwrap();
    }

    /// A `[[contributions.provider]]` was declared, approved and enabled,
    /// and NOBODY resolved it: `connect.rs` matched schemes by hand. This is
    /// the half of the registry that was missing: given a scheme, the
    /// consented plugin that declares it — or nothing.
    #[test]
    fn resolve_provider_picks_the_consented_plugin_that_declares_the_scheme() {
        let tmp = TempDir::new().unwrap();
        write_provider(tmp.path(), "org.norte.memplug", PROVIDER_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();

        // Not consented: nothing, even if the scheme matches (fail-closed).
        assert!(reg.resolve_provider("memplug").is_none());
        reg.set_approval("org.norte.memplug", true).unwrap();
        assert!(
            reg.resolve_provider("memplug").is_none(),
            "approved but off"
        );
        reg.set_enabled("org.norte.memplug", true).unwrap();

        let r = reg
            .resolve_provider("memplug")
            .expect("consented and enabled");
        assert_eq!(r.id, "org.norte.memplug");
        assert_eq!(r.name, "Mem plug");
        assert!(r.wasm.path().ends_with("plugin.wasm"));
        assert_eq!(
            r.wasm_digest,
            norte_plugin_host::wasm_digest_of(b"\0asm"),
            "the returned digest is the one of the anchored binary"
        );
        assert_eq!(r.default_port, None);
        // Another scheme is not served by it: the plugin serves what it
        // DECLARES.
        assert!(reg.resolve_provider("webdav").is_none());
        // And what approving grants is SHOWN: the scheme goes into the
        // badges.
        let info = reg.list().plugins.into_iter().next().unwrap();
        assert!(
            info.capabilities.iter().any(|c| c == "provider:memplug"),
            "{:?}",
            info.capabilities
        );

        // With no binary there is nothing to instantiate, consented or not.
        std::fs::remove_file(tmp.path().join("plugins/org.norte.memplug/plugin.wasm")).unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.resolve_provider("memplug").is_none());
    }

    /// The second gate: even if a catalog entry declares a core scheme (the
    /// manifest rejects it, so it has to be smuggled in by hand), the
    /// registry does not serve it. This is what makes the manager's guard an
    /// optimization and not the only defense.
    #[test]
    fn resolve_provider_never_serves_a_core_scheme() {
        let tmp = TempDir::new().unwrap();
        write_provider(tmp.path(), "org.norte.memplug", PROVIDER_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        reg.set_approval("org.norte.memplug", true).unwrap();
        reg.set_enabled("org.norte.memplug", true).unwrap();
        // `sftp` is claimed behind the parser's back.
        reg.catalog.plugins[0].manifest.contributions.provider[0].scheme = "sftp".to_owned();
        // The anchor changes with the contributions, so it is re-approved in
        // memory so the only thing left standing is the gate.
        reg.set_approval_in_memory("org.norte.memplug", true);
        assert!(reg.resolve_provider("sftp").is_none());
    }

    /// A plugin of another category with a smuggled-in `provider`
    /// contribution does not get in: `provider` has its own world, and only
    /// a binary that implements it should be instantiated as one (same
    /// criterion as `resolve_decorators`).
    #[test]
    fn resolve_provider_ignores_other_categories() {
        let tmp = TempDir::new().unwrap();
        write_provider(
            tmp.path(),
            "org.norte.sneaky",
            r#"
[plugin]
id = "org.norte.sneaky"
name = "Sneaky"
publisher = "norte"
version = "0.1.0"
category = "command"
[[contributions.provider]]
scheme = "memplug"
"#,
        );
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        reg.set_approval("org.norte.sneaky", true).unwrap();
        reg.set_enabled("org.norte.sneaky", true).unwrap();
        assert!(reg.resolve_provider("memplug").is_none());
    }

    #[test]
    fn plugins_discover_lists_a_plugin_with_no_state() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1);
        let p = &list.plugins[0];
        assert_eq!(p.id, "org.norte.demo");
        assert_eq!(p.category, "command");
        assert!(!p.approved);
        assert!(!p.enabled);
        assert!(p.capabilities.iter().any(|c| c == "fs-read"));
        assert!(list.errors.is_empty());
        // (P1) DEMO_MANIFEST declares neither description nor commands.
        assert_eq!(p.description, None, "no description in the manifest");
        assert!(p.commands.is_empty(), "no contributions.command");
    }

    /// (P1) a manifest with `description` + one `contributions.command`:
    /// both must reach `PluginInfo` intact via `list()`.
    #[test]
    fn plugins_discover_propagates_description_and_commands() {
        const WITH_DESC_AND_COMMANDS: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
description = "Greets from the command palette."
[contributions]
command = [
    { id = "greet", title = "Greet" },
    { id = "wave", title = "Wave" },
]
[capabilities]
fs-read = "scoped"
"#;
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", WITH_DESC_AND_COMMANDS);

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1);
        let p = &list.plugins[0];
        assert_eq!(
            p.description.as_deref(),
            Some("Greets from the command palette.")
        );
        assert_eq!(p.commands.len(), 2, "the two declared commands");
        // Manifest order preserved (not reordered).
        assert_eq!(p.commands[0].id, "greet");
        assert_eq!(p.commands[0].title, "Greet");
        assert_eq!(p.commands[1].id, "wave");
        assert_eq!(p.commands[1].title, "Wave");
    }

    /// `wasm_path` is a pure path computation (single source of truth for the
    /// `plugins/<id>/plugin.wasm` layout, review H2): it does not require the
    /// binary to exist.
    #[test]
    fn wasm_path_follows_the_plugins_id_layout() {
        let tmp = TempDir::new().unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert_eq!(
            reg.wasm_path("org.norte.demo"),
            tmp.path()
                .join("plugins")
                .join("org.norte.demo")
                .join("plugin.wasm")
        );
    }

    /// Manifest with `[config]` (P2 Task 2), for `settings_of`.
    const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfg"
name = "Cfg"
publisher = "norte"
version = "0.1.0"
category = "command"
[config.retries]
type = "int"
default = 3
min = 0
max = 10
"#;

    #[test]
    fn settings_of_with_no_config_toml_returns_the_defaults() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cfg", CONFIG_MANIFEST);

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let settings = reg
            .settings_of("org.norte.cfg")
            .unwrap_or_else(|| panic!("expected a discovered plugin"));
        assert_eq!(settings.get("retries").map(String::as_str), Some("3"));
    }

    #[test]
    fn settings_of_with_override_reflects_config_toml_value() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cfg", CONFIG_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.cfg")
                .join("config.toml"),
            "retries = 8\n",
        )
        .unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let settings = reg.settings_of("org.norte.cfg").unwrap();
        assert_eq!(settings.get("retries").map(String::as_str), Some("8"));
    }

    #[test]
    fn settings_of_unknown_id_is_none() {
        let tmp = TempDir::new().unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.settings_of("org.norte.ghost").is_none());
    }

    #[test]
    fn plugins_set_state_is_reflected_and_persists() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval("org.norte.demo", true).unwrap());
        assert!(reg.set_enabled("org.norte.demo", true).unwrap());

        let p = &reg.list().plugins[0];
        assert!(p.approved);
        assert!(p.enabled);

        // A NEW discover of the same dir remembers it (it persisted).
        let reg2 = PluginRegistry::discover(tmp.path()).unwrap();
        let p2 = &reg2.list().plugins[0];
        assert!(p2.approved, "approved must persist");
        assert!(p2.enabled, "enabled must persist");
        assert_eq!(
            p2.id, "org.norte.demo",
            "the id-with-dots must come back intact"
        );
    }

    #[test]
    fn plugins_set_of_nonexistent_id_does_not_persist_garbage() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(!reg.set_approval("org.norte.ghost", true).unwrap());
        assert!(!reg.set_enabled("org.norte.ghost", true).unwrap());

        // The state file was not created (nothing to persist).
        assert!(
            !tmp.path().join("plugins-state.toml").exists(),
            "an unknown id must not create plugins-state.toml"
        );
    }

    /// #241: changing the BINARY invalidates the approval, even if the
    /// manifest is not touched.
    ///
    /// Issue #69's anchor covered `plugin.toml` —what it asks for and when
    /// it fires— and left the bundle's other door open: whoever could write
    /// the `.wasm` without touching the `.toml` kept the capabilities a
    /// human approved for OTHER code.
    #[test]
    fn changing_the_binary_invalidates_the_approval() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let wasm = tmp
            .path()
            .join("plugins")
            .join("org.norte.demo")
            .join("plugin.wasm");
        std::fs::write(&wasm, b"\0asm-one").unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval("org.norte.demo", true).unwrap());
        assert!(
            reg.list().plugins[0].approved,
            "approved with this binary in front"
        );

        // The manifest is NOT touched; only the binary.
        std::fs::write(&wasm, b"\0asm-other").unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            !reg.list().plugins[0].approved,
            "a different binary is a different question: consent must be given again"
        );

        // And putting the previous binary back returns the approval: the
        // anchor is the CONTENT, not a change counter.
        std::fs::write(&wasm, b"\0asm-one").unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.list().plugins[0].approved);
    }

    #[test]
    fn plugins_broken_manifest_appears_in_errors_without_taking_discover_down() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        write_plugin(tmp.path(), "broken", "this is not toml [ valid =");

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1, "the valid one still loads");
        assert_eq!(
            list.errors.len(),
            1,
            "the broken one is reported, not dropped"
        );
        assert!(list.errors[0].dir.contains("broken"));
    }

    #[test]
    fn plugins_state_id_with_dots_round_trips() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        // We persist state for an id with DOTS.
        {
            let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
            assert!(reg.set_approval("org.norte.demo", true).unwrap());
        }

        // The file must carry the QUOTED key, not nested.
        let raw = std::fs::read_to_string(tmp.path().join("plugins-state.toml")).unwrap();
        assert!(
            raw.contains("\"org.norte.demo\""),
            "the key must be quoted, not as [org.norte.demo]: {raw}"
        );

        // And a fresh discover recovers the SAME id with its state.
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let st = reg.state.get("org.norte.demo").cloned().unwrap();
        assert!(st.approved && !st.enabled);
        // The anchor saved on approval (issue #69, #241) also survives the
        // round trip and matches the current one — which is manifest AND
        // binary.
        let entry = reg
            .catalog
            .plugins
            .iter()
            .find(|e| e.manifest.id == "org.norte.demo")
            .expect("discovered");
        assert_eq!(
            st.approved_digest.as_deref(),
            Some(entry.approval_anchor().as_str()),
            "the anchor must persist and match the bundle"
        );
    }

    /// Minimal `command` manifest, with no special capabilities, for the
    /// fail-closed execution tests.
    const CMD_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cmd"
name = "Cmd"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

    #[test]
    fn plugins_run_command_with_no_approval_is_not_approved() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hello")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotApproved(ref id) if id == "org.norte.cmd"),
            "a plugin with no approval is NEVER run: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_approved_but_not_enabled_is_disabled() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hello")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::Disabled(ref id) if id == "org.norte.cmd"),
            "approved but disabled does not run: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_enabled_with_no_wasm_is_no_binary() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hello")
            .unwrap_err();
        assert!(
            matches!(&err, PluginRunError::NoBinary(id) if id == "org.norte.cmd"),
            "with no plugin.wasm the runtime does not start: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_nonexistent_id_is_unknown() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.ghost", "echo", "hello")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::Unknown(ref id) if id == "org.norte.ghost"),
            "an unknown id is Unknown: {err:?}"
        );
    }

    #[test]
    fn plugins_corrupt_state_is_invalid_data() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::write(
            tmp.path().join("plugins-state.toml"),
            "this is not [ valid toml =",
        )
        .unwrap();

        let err = PluginRegistry::discover(tmp.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// Manifest of a previewer that declares `text/*`.
    const PREV_MANIFEST: &str = r#"
[plugin]
id = "org.norte.prev"
name = "Prev"
publisher = "norte"
version = "0.1.0"
category = "previewer"
[[contributions.previewer]]
mimetypes = ["text/*"]
"#;

    fn vpath(s: &str) -> norte_proto::VPath {
        norte_proto::VPath::parse(s).unwrap()
    }

    #[test]
    fn plugins_guess_mimetype_by_extension() {
        assert_eq!(guess_mimetype(&vpath("file:///a.txt")), "text/plain");
        assert_eq!(guess_mimetype(&vpath("file:///a.json")), "application/json");
        assert_eq!(guess_mimetype(&vpath("file:///README.md")), "text/markdown");
        assert_eq!(
            guess_mimetype(&vpath("file:///a.MARKDOWN")),
            "text/markdown"
        );
        assert_eq!(guess_mimetype(&vpath("file:///a.png")), "image/png");
        assert_eq!(guess_mimetype(&vpath("file:///a.JPG")), "image/jpeg");
        assert_eq!(guess_mimetype(&vpath("file:///a.jpeg")), "image/jpeg");
        assert_eq!(guess_mimetype(&vpath("file:///a.gif")), "image/gif");
        assert_eq!(guess_mimetype(&vpath("file:///a.webp")), "image/webp");
    }

    /// D4: the width a client asks for reaches the guest capped; absence
    /// stays absence (the guest chooses), not a zero nor the cap.
    #[test]
    fn clamp_preview_columns_caps_and_respects_none() {
        assert_eq!(clamp_preview_columns(None), None);
        assert_eq!(clamp_preview_columns(Some(80)), Some(80));
        assert_eq!(
            clamp_preview_columns(Some(u32::MAX)),
            Some(PREVIEW_MAX_COLUMNS)
        );
        assert_eq!(
            guess_mimetype(&vpath("file:///a")),
            "application/octet-stream"
        );
        assert_eq!(
            guess_mimetype(&vpath("file:///a.UNKNOWN")),
            "application/octet-stream"
        );
    }

    #[test]
    fn plugins_mimetype_matches_glob_and_exact() {
        assert!(mimetype_matches("text/*", "text/plain"));
        assert!(!mimetype_matches("text/*", "application/json"));
        assert!(mimetype_matches("application/json", "application/json"));
        // Does not partially match: a textual prefix without the slash is not a glob.
        assert!(!mimetype_matches("application/json", "application/json5"));
        assert!(!mimetype_matches("text/plain", "text/plai"));
    }

    #[test]
    fn plugins_resolve_previewer_fail_closed_and_by_mimetype() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);
        // EMPTY `plugin.wasm`: `is_file()` does not validate content, only presence.
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.prev")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();

        // Discovered but NOT approved/enabled → fail-closed.
        assert!(
            reg.resolve_previewer("text/plain").is_none(),
            "a non-consented previewer is never chosen"
        );

        // Approved + enabled → resolves for the mimetype that matches the glob.
        assert!(reg.set_approval_in_memory("org.norte.prev", true));
        assert!(reg.set_enabled_in_memory("org.norte.prev", true));

        let got = reg.resolve_previewer("text/plain");
        assert!(got.is_some(), "text/plain matches text/*");
        let (id, name, wasm, _caps, settings) = got.unwrap();
        assert_eq!(id, "org.norte.prev");
        assert_eq!(name, "Prev");
        assert!(wasm.path().ends_with("plugin.wasm"));
        assert!(
            settings.is_empty(),
            "PREV_MANIFEST declares no [config]: empty map"
        );

        // A mimetype that does not match the declared glob → None.
        assert!(
            reg.resolve_previewer("application/json").is_none(),
            "application/json does not match text/*"
        );
    }

    /// D3: a previewer that declares the EXACT mimetype beats one that
    /// declares the wildcard, even if the catalog orders it after. Without
    /// this, which one painted a `.md` was decided by the alphabet of the
    /// ids: `org.norte.md` beat `org.norte.syntect`, and `org.zzz.md` lost.
    #[test]
    fn plugins_resolve_previewer_prefers_exact_over_glob() {
        let tmp = TempDir::new().unwrap();
        // The wildcard goes FIRST in catalog order (lower id).
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);
        write_plugin(
            tmp.path(),
            "org.zzz.md",
            r#"
[plugin]
id = "org.zzz.md"
name = "MD"
publisher = "zzz"
version = "0.1.0"
category = "previewer"
[contributions]
previewer = [{ mimetypes = ["text/markdown"] }]
"#,
        );
        for id in ["org.norte.prev", "org.zzz.md"] {
            std::fs::write(tmp.path().join("plugins").join(id).join("plugin.wasm"), b"").unwrap();
        }
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        for id in ["org.norte.prev", "org.zzz.md"] {
            assert!(reg.set_approval_in_memory(id, true));
            assert!(reg.set_enabled_in_memory(id, true));
        }
        let (id, ..) = reg.resolve_previewer("text/markdown").unwrap();
        assert_eq!(
            id, "org.zzz.md",
            "exact beats text/* even if it comes after"
        );
        let (id, ..) = reg.resolve_previewer("text/plain").unwrap();
        assert_eq!(
            id, "org.norte.prev",
            "and the wildcard still handles the rest"
        );
    }

    #[test]
    fn plugins_resolve_previewer_with_no_wasm_is_none() {
        let tmp = TempDir::new().unwrap();
        // Without writing plugin.wasm: even if consented, there is no binary.
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.prev", true));
        assert!(reg.set_enabled_in_memory("org.norte.prev", true));

        assert!(
            reg.resolve_previewer("text/plain").is_none(),
            "with no plugin.wasm there is nothing to run"
        );
    }

    /// Overwrites `<config>/plugins/<id>/`'s `plugin.toml` with `src`.
    fn rewrite_manifest(config_dir: &Path, id: &str, src: &str) {
        std::fs::write(config_dir.join("plugins").join(id).join("plugin.toml"), src).unwrap();
    }

    /// `command` manifest with NO dangerous capabilities (for the TOCTOU test).
    const TOCTOU_BEFORE: &str = r#"
[plugin]
id = "org.norte.toctou"
name = "TOCTOU"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

    /// A guest's phrase crosses masked and capped (#332).
    #[test]
    fn the_guests_phrase_crosses_with_no_escapes_and_capped() {
        let hostile = format!("approve {}location", '\u{1b}');
        let out = guest_reason(&hostile);
        assert!(!out.contains('\u{1b}'), "{out}");
        assert!(out.starts_with("approve "), "{out}");
        let long = "x".repeat(GUEST_REASON_MAX_CHARS + 50);
        let out = guest_reason(&long);
        assert_eq!(out.chars().count(), GUEST_REASON_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
        assert_eq!(guest_reason("plain"), "plain");
    }

    /// A renamer (0.67.0): a 0.66 client sees it as a command and asks for
    /// `run_command` with its id. The answer is "does not run commands",
    /// before looking at consent or binary, and without instantiating
    /// anything.
    #[test]
    fn a_kind_that_does_not_export_command_is_not_runnable() {
        let tmp = TempDir::new().unwrap();
        write_plugin(
            tmp.path(),
            "org.norte.renamer",
            r#"
[plugin]
id = "org.norte.renamer"
name = "Renamer"
publisher = "norte"
version = "0.1.0"
category = "renamer"

[[contributions.renamer]]
id = "by-date"
title = "By date"
"#,
        );
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let err = reg.resolve_runnable("org.norte.renamer").unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotRunnable(ref id) if id == "org.norte.renamer"),
            "{err:?}"
        );
    }

    /// The SAME plugin, but with EXPANDED capabilities (fs-read + net) that
    /// the human never approved.
    const TOCTOU_AFTER: &str = r#"
[plugin]
id = "org.norte.toctou"
name = "TOCTOU"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
net = { hosts = ["evil.example"] }
"#;

    #[test]
    fn plugins_capabilities_changed_after_approval_ask_for_consent_again() {
        // Issue #69: the human approves some capabilities; then plugin.toml
        // changes on disk to broader ones and a new discover happens. The
        // approval (flag true on disk) must NOT hold for the NEW
        // capabilities: the anchored digest no longer matches → NotApproved
        // (re-consent).
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.toctou", TOCTOU_BEFORE);
        {
            let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
            assert!(reg.set_approval("org.norte.toctou", true).unwrap());
            assert!(reg.set_enabled("org.norte.toctou", true).unwrap());
        }

        // The attacker rewrites the manifest with expanded capabilities.
        rewrite_manifest(tmp.path(), "org.norte.toctou", TOCTOU_AFTER);

        // New discover: reads the state (approved=true + OLD digest) and the
        // NEW manifest.
        let reg = PluginRegistry::discover(tmp.path()).unwrap();

        // list() shows the approval as NOT current (the UI asks for consent again).
        let info = &reg.list().plugins[0];
        assert!(
            !info.approved,
            "changed capabilities ⇒ effective approval=false"
        );
        assert!(
            info.capabilities.iter().any(|c| c == "net"),
            "and shows the NEW capabilities so the human sees them"
        );

        // And resolve_runnable rejects fail-closed with NotApproved.
        let err = reg.resolve_runnable("org.norte.toctou").unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotApproved(ref id) if id == "org.norte.toctou"),
            "the anchored digest no longer matches: {err:?}"
        );

        // Re-approving re-anchors the digest to the NEW capabilities and
        // resolves again (the human consented to what is there now).
        let mut reg = reg;
        assert!(reg.set_approval("org.norte.toctou", true).unwrap());
        assert!(reg.list().plugins[0].approved);
    }

    #[test]
    fn plugins_inherited_approval_with_no_digest_asks_for_consent_again() {
        // State persisted from BEFORE the defense (issue #69): approved=true
        // with no `digest`. Fail-closed: treated as not current until
        // re-approved.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        std::fs::write(
            tmp.path().join("plugins-state.toml"),
            "[plugins]\n\"org.norte.cmd\" = { approved = true, enabled = true }\n",
        )
        .unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            !reg.list().plugins[0].approved,
            "an approval with no anchored digest is not current"
        );
        let err = reg.resolve_runnable("org.norte.cmd").unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotApproved(_)),
            "fail-closed with no digest: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn plugins_wasm_symlink_outside_the_dir_is_no_binary() {
        // Issue #69 (defense in depth): `plugin.wasm` is a symlink pointing
        // OUTSIDE the plugin's directory. It is rejected (NoBinary), a
        // foreign binary is not run.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        // An "outside" binary (content irrelevant: the symlink is rejected
        // before trying to compile it).
        let outside = tmp.path().join("foreign.wasm");
        std::fs::write(&outside, b"foreign binary").unwrap();
        let link = tmp
            .path()
            .join("plugins")
            .join("org.norte.cmd")
            .join("plugin.wasm");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));

        let err = reg.resolve_runnable("org.norte.cmd").unwrap_err();
        assert!(
            matches!(err, PluginRunError::NoBinary(ref id) if id == "org.norte.cmd"),
            "a plugin.wasm that escapes the dir is rejected: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn plugins_wasm_symlink_inside_the_dir_is_accepted() {
        // A symlink that resolves INSIDE the plugin's dir is legitimate
        // (e.g. a build that links to the real artifact next to it).
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let plugin_dir = tmp.path().join("plugins").join("org.norte.cmd");
        let real = plugin_dir.join("real.wasm");
        std::fs::write(&real, b"artifact").unwrap();
        std::os::unix::fs::symlink(&real, plugin_dir.join("plugin.wasm")).unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));

        // resolve_runnable must not fail with NoBinary (it gets to return the path).
        let resolved = reg.resolve_runnable("org.norte.cmd");
        assert!(
            resolved.is_ok(),
            "a symlink inside the dir is valid: {resolved:?}"
        );
    }

    // -------------------------------------------------------------------
    // G3b (ADR 0037): `resolve_decorators`/`resolve_columns` + the
    // positional validation helpers.

    /// Minimal `decorator` manifest.
    const DECOR_MANIFEST: &str = r#"
[plugin]
id = "org.norte.decor"
name = "Decor"
publisher = "norte"
version = "0.1.0"
category = "decorator"
[[contributions.decorator]]
"#;

    /// A second decorator, to test that `resolve_decorators` returns ALL
    /// consented ones (not the first, unlike `resolve_previewer`).
    const DECOR_MANIFEST_2: &str = r#"
[plugin]
id = "org.norte.decor2"
name = "Decor2"
publisher = "norte"
version = "0.1.0"
category = "decorator"
[[contributions.decorator]]
"#;

    /// `columns` manifest that declares a `size-human` column.
    const COLUMNS_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cols"
name = "Cols"
publisher = "norte"
version = "0.1.0"
category = "columns"
[[contributions.columns]]
id = "size-human"
header = "Size"
"#;

    #[test]
    fn resolve_decorators_fail_closed_with_no_consent() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.decor", DECOR_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.decor")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            reg.resolve_decorators().is_empty(),
            "a non-consented decorator is never resolved"
        );
    }

    #[test]
    fn resolve_decorators_returns_all_consented_ones_not_just_the_first() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.decor", DECOR_MANIFEST);
        write_plugin(tmp.path(), "org.norte.decor2", DECOR_MANIFEST_2);
        for id in ["org.norte.decor", "org.norte.decor2"] {
            std::fs::write(tmp.path().join("plugins").join(id).join("plugin.wasm"), b"").unwrap();
        }
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.decor", true));
        assert!(reg.set_enabled_in_memory("org.norte.decor", true));
        assert!(reg.set_approval_in_memory("org.norte.decor2", true));
        assert!(reg.set_enabled_in_memory("org.norte.decor2", true));

        let resolved = reg.resolve_decorators();
        assert_eq!(resolved.len(), 2, "BOTH decorators consented: {resolved:?}");
        let ids: Vec<&str> = resolved.iter().map(|((id, ..), _)| id.as_str()).collect();
        assert!(ids.contains(&"org.norte.decor"));
        assert!(ids.contains(&"org.norte.decor2"));
    }

    /// ADR 0105: the slot comes from the manifest —`icon` when it declares
    /// it, `badge` otherwise—, and from the FIRST contribution.
    #[test]
    fn resolve_decorators_reads_the_slot_from_the_manifest() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.decor", DECOR_MANIFEST);
        write_plugin(
            tmp.path(),
            "org.norte.icons",
            r#"
            [plugin]
            id = "org.norte.icons"
            name = "Icons"
            publisher = "norte"
            version = "0.1.0"
            category = "decorator"
            [[contributions.decorator]]
            slot = "icon"
            [capabilities]
        "#,
        );
        for id in ["org.norte.decor", "org.norte.icons"] {
            std::fs::write(tmp.path().join("plugins").join(id).join("plugin.wasm"), b"").unwrap();
        }
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        for id in ["org.norte.decor", "org.norte.icons"] {
            assert!(reg.set_approval_in_memory(id, true));
            assert!(reg.set_enabled_in_memory(id, true));
        }
        let slots: std::collections::HashMap<String, norte_plugin_host::DecoratorSlot> = reg
            .resolve_decorators()
            .into_iter()
            .map(|((id, ..), slot)| (id, slot))
            .collect();
        assert_eq!(
            slots["org.norte.decor"],
            norte_plugin_host::DecoratorSlot::Badge
        );
        assert_eq!(
            slots["org.norte.icons"],
            norte_plugin_host::DecoratorSlot::Icon
        );
        assert_eq!(
            slot_to_wire(slots["org.norte.icons"]),
            norte_proto::methods::DecorationSlot::Icon
        );
    }

    #[test]
    fn resolve_decorators_ignores_a_different_category_even_if_it_declares_contrib() {
        // A `command` plugin does not get in through `resolve_decorators`
        // even if, hypothetically, someone copied
        // `[[contributions.decorator]]` into its manifest: the dedicated
        // world (`norte-decorator`) requires the PRIMARY category to be
        // `decorator` (unlike previewer/command, which share a world).
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.cmd")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));
        assert!(reg.resolve_decorators().is_empty());
    }

    #[test]
    fn resolve_columns_fail_closed_and_by_id() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cols", COLUMNS_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.cols")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();

        assert!(
            reg.resolve_columns("size-human").is_none(),
            "without consent, no column resolves"
        );

        assert!(reg.set_approval_in_memory("org.norte.cols", true));
        assert!(reg.set_enabled_in_memory("org.norte.cols", true));

        let (id, name, wasm, _caps, _settings) = reg
            .resolve_columns("size-human")
            .expect("the declared column resolves");
        assert_eq!(id, "org.norte.cols");
        assert_eq!(name, "Cols");
        assert!(wasm.path().ends_with("plugin.wasm"));

        assert!(
            reg.resolve_columns("not-declared").is_none(),
            "an undeclared column id does not resolve"
        );
    }

    #[test]
    fn paths_to_basenames_extracts_the_name_not_the_full_path() {
        let paths = vec![
            norte_proto::VPath::parse("file:///a/b/module.rs").unwrap(),
            norte_proto::VPath::parse("file:///a/README.md").unwrap(),
        ];
        let names = paths_to_basenames(&paths);
        assert_eq!(names, vec![b"module.rs".to_vec(), b"README.md".to_vec()]);
    }

    #[test]
    fn decorations_to_wire_checked_correct_length_passes() {
        use norte_plugin_host::decorator_iface::Decoration;
        let out = vec![
            Decoration {
                badge: Some("M".to_string()),
                role: Some("warning".to_string()),
            },
            Decoration {
                badge: None,
                role: None,
            },
        ];
        let wire = decorations_to_wire_checked(out, 2).expect("length matches: Some");
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0].badge.as_deref(), Some("M"));
        assert_eq!(wire[1].badge, None);
    }

    #[test]
    fn decorations_to_wire_checked_different_length_is_none_fail_closed() {
        use norte_plugin_host::decorator_iface::Decoration;
        let out = vec![Decoration {
            badge: Some("M".to_string()),
            role: None,
        }];
        assert!(
            decorations_to_wire_checked(out, 2).is_none(),
            "a guest that breaks the positional contract is discarded whole"
        );
    }

    /// The token dies with the call: the session's `Drop` IS the expiration
    /// mechanism, and that is why there is no TTL to tune nor sweep to
    /// remember.
    #[test]
    fn the_token_dies_with_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let vpath = crate::policy::local_root_vpath(dir.path()).expect("tempdir's vpath");
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let token = {
            let session = mint.mint_for(&vpath, None, false).expect("mint");
            let token = session.token().to_owned();
            assert!(mint.resolve(&token).is_ok(), "alive while the call lasts");
            assert_eq!(mint.live_tokens(), 1);
            token
        };
        assert!(
            mint.resolve(&token).is_err(),
            "a token from the previous page is dead"
        );
        assert_eq!(mint.live_tokens(), 0, "and nothing is left held");
    }

    /// Two different tokens never cross, and neither is guessable.
    #[test]
    fn two_locations_do_not_share_a_token() {
        use norte_plugin_host::LocationHost as _;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("only-in-a"), b"x").unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let sa = mint
            .mint_for(
                &crate::policy::local_root_vpath(a.path()).unwrap(),
                None,
                false,
            )
            .expect("mint a");
        let sb = mint
            .mint_for(
                &crate::policy::local_root_vpath(b.path()).unwrap(),
                None,
                false,
            )
            .expect("mint b");
        assert_ne!(sa.token(), sb.token());
        assert_eq!(sa.token().len(), 64, "32 bytes in hex");
        assert!(mint.read(sa.token(), b"only-in-a").is_ok());
        assert!(
            mint.read(sb.token(), b"only-in-a").is_err(),
            "B's token does not reach A's tree"
        );
    }

    /// With no local path there is no token: the guest reads from a
    /// directory descriptor, and an `sftp://` has none.
    #[test]
    fn a_location_that_is_not_file_mints_no_token() {
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let remote = norte_proto::VPath::parse("sftp://host/dir").unwrap();
        assert!(
            mint.mint_for(&remote, None, false).is_none(),
            "with no local path there is no token"
        );
        let archive = norte_proto::VPath::parse("zip+file:///a.zip/!/inside").unwrap();
        assert!(
            mint.mint_for(&archive, None, false).is_none(),
            "not inside an archive either"
        );
    }

    /// ADR 0052: a protected root is not bypassed just because whoever asks
    /// is a plugin instead of an agent.
    #[test]
    fn the_state_directory_still_cannot_be_read_through_here() {
        let state = tempfile::tempdir().unwrap();
        let root = crate::policy::local_root_vpath(state.path()).unwrap();
        std::fs::create_dir(state.path().join("inside")).unwrap();
        let mint =
            LocationMint::with_protected(vec![root.clone()], norte_vfs_local::Bounds::default());
        assert!(
            mint.mint_for(&root, None, false).is_none(),
            "the protected root, no"
        );
        let child = crate::policy::local_root_vpath(&state.path().join("inside")).unwrap();
        assert!(
            mint.mint_for(&child, None, false).is_none(),
            "nor anything under it"
        );
    }

    /// The project-root marker: with `.git` declared, what opens is the
    /// ANCESTOR that contains it, and the prefix says what the user is
    /// looking at. Without this the column would only work with the pane
    /// right at the repository's root, because a token CANNOT climb.
    #[test]
    fn the_marker_opens_the_ancestor_and_states_the_prefix() {
        use norte_plugin_host::LocationHost as _;
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/index"), b"DIRC").unwrap();
        std::fs::create_dir_all(repo.path().join("src/deep")).unwrap();
        let dir = crate::policy::local_root_vpath(&repo.path().join("src/deep")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let session = mint.mint_for(&dir, Some(".git"), true).expect("mint");
        assert_eq!(session.as_ref().prefix, b"src/deep");
        assert_eq!(
            mint.read(session.token(), b".git/index").unwrap(),
            b"DIRC",
            "the opened root is the repository, not the visible directory"
        );
    }

    /// #241: a marker that is a SYMLINK does not count, not even dangling.
    ///
    /// `ln -s /nothing /tmp/.git` — and anyone can create a name in `/tmp`,
    /// the sticky bit only stops deleting others' — made every pane under
    /// `/tmp` hand the plugin the whole of `/tmp`. A legitimate `.git` is a
    /// directory or a worktree's `gitdir:` file; a link, never.
    #[test]
    fn a_marker_that_is_a_symlink_does_not_open_the_ancestor() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/does-not-exist", root.path().join(".git")).unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let dir = crate::policy::local_root_vpath(&root.path().join("sub")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        // The CLIMB is called directly, not just `mint_for`, so a failure
        // names the guilty ancestor. With the bare prefix, this test said
        // "it climbed" and not to where, which is half the data (#308): two
        // days of intermittency without knowing which directory had the
        // marker.
        let (found_root, prefix, _) = mint.climb_to_marker(&dir, &root.path().join("sub"), ".git");
        assert!(
            prefix.is_empty(),
            "it did not climb: the root is the visible directory, not the ancestor. \
             It climbed to {} (starting from {})",
            found_root.display(),
            root.path().join("sub").display()
        );
        let session = mint.mint_for(&dir, Some(".git"), true).expect("mint");
        assert!(session.as_ref().prefix.is_empty());
    }

    /// A marker in a directory ANYONE CAN WRITE TO opens nothing (#308).
    ///
    /// #241 closed the symlink case and left open the one that takes the
    /// least effort: `mkdir /tmp/.git`. `/tmp`'s sticky bit stops DELETING
    /// others' names, it does not stop CREATING your own, and a `.git` that
    /// is a real directory passed the check —it is written to reject links,
    /// and a directory is not a link—. From there, any pane under `/tmp`
    /// handed the plugin the WHOLE of `/tmp`: the temp files of every user
    /// on the machine.
    ///
    /// It was discovered because this test is intermittent on machines where
    /// someone has left a `/tmp/.git`. It was not a flaky test: it was the
    /// test seeing the hole every time the condition existed.
    #[test]
    fn a_marker_in_a_directory_anyone_can_write_to_opens_nothing() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        // A REAL `.git`, not a link: this is what the previous check
        // accepted.
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        // 1777, like `/tmp`: writable by everyone, with the sticky bit.
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let (found, prefix, _) = mint.climb_to_marker(
            &crate::policy::local_root_vpath(&root.path().join("sub")).unwrap(),
            &root.path().join("sub"),
            ".git",
        );
        assert!(
            prefix.is_empty(),
            "a directory anyone can write to is not a project's root: \
             it climbed to {}",
            found.display()
        );
    }

    /// And a NORMAL repository still opens: the fix cannot cost the whole
    /// use case.
    #[test]
    fn a_repository_with_normal_permissions_still_opens() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let (_, prefix, _) = mint.climb_to_marker(
            &crate::policy::local_root_vpath(&root.path().join("sub")).unwrap(),
            &root.path().join("sub"),
            ".git",
        );
        assert_eq!(prefix, b"sub", "a real repo does open its root");
    }

    /// And a worktree's `gitdir:` file DOES count: it is a real `.git`, and
    /// requiring a directory would have broken worktrees and submodules.
    #[test]
    fn a_marker_that_is_a_file_does_open_the_ancestor() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".git"), b"gitdir: /somewhere/else").unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let dir = crate::policy::local_root_vpath(&root.path().join("sub")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let session = mint.mint_for(&dir, Some(".git"), true).expect("mint");
        assert_eq!(session.as_ref().prefix, b"sub");
    }

    /// #241: the climb does NOT go past `$HOME`.
    ///
    /// `touch $HOME/.git` —a badly extracted archive, a careless installer,
    /// any process of the user's— turned every one of their directories that
    /// was not a repository into a root spanning the whole home:
    /// `MAX_CLIMB` is 64 and there was nothing else. A marker IN `$HOME` is
    /// still valid; what is not done is going past it.
    #[test]
    fn the_climb_stops_at_home() {
        let home = tempfile::tempdir().unwrap();
        // A `.git` ABOVE the home: the case that must not be reached.
        std::fs::write(home.path().join(".git"), b"gitdir: /x").unwrap();
        let child = home.path().join("projects/one");
        std::fs::create_dir_all(&child).unwrap();
        let dir = crate::policy::local_root_vpath(&child).unwrap();

        // With the home IN the ancestor that carries the marker: that one
        // opens, which is the legitimate case — the ceiling is not going
        // past the home, not ignoring what is in it.
        let mint = LocationMint::with_protected_and_home(
            Vec::new(),
            norte_vfs_local::Bounds::default(),
            Some(home.path().to_path_buf()),
        );
        let session = mint.mint_for(&dir, Some(".git"), true).expect("mint");
        assert_eq!(session.as_ref().prefix, b"projects/one");

        // And with the home at the child, the climb stops there: the `.git`
        // above no longer counts.
        let mint = LocationMint::with_protected_and_home(
            Vec::new(),
            norte_vfs_local::Bounds::default(),
            Some(child.clone()),
        );
        let session = mint.mint_for(&dir, Some(".git"), true).expect("mint");
        assert!(
            session.as_ref().prefix.is_empty(),
            "it did not climb past the home"
        );
    }

    /// Without `climb` there is no climbing: an agent confined to its scope
    /// does not gain an ancestor just because the plugin declares a marker.
    #[test]
    fn without_climb_the_root_is_the_visible_directory() {
        use norte_plugin_host::LocationHost as _;
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::create_dir(repo.path().join("src")).unwrap();
        let dir = crate::policy::local_root_vpath(&repo.path().join("src")).unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let session = mint.mint_for(&dir, Some(".git"), false).expect("mint");
        assert!(session.as_ref().prefix.is_empty());
        assert!(
            mint.read(session.token(), b".git/index").is_err(),
            "without climbing, the repository is left out"
        );
    }

    /// A marker that does not appear above does not climb anywhere: the root
    /// stays the visible directory.
    #[test]
    fn an_absent_marker_does_not_climb_just_in_case() {
        let dir_t = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir_t.path().join("sub")).unwrap();
        let dir = crate::policy::local_root_vpath(&dir_t.path().join("sub")).unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let session = mint
            .mint_for(&dir, Some(".does-not-exist"), true)
            .expect("mint");
        assert!(session.as_ref().prefix.is_empty());
    }

    /// The climb stops at a protected root: the state directory does not
    /// become anyone's root (ADR 0052).
    #[test]
    fn the_climb_stops_at_a_protected_root() {
        let state = tempfile::tempdir().unwrap();
        // The marker is in the protected PARENT; the visible directory hangs
        // from it.
        std::fs::create_dir(state.path().join(".git")).unwrap();
        std::fs::create_dir(state.path().join("inside")).unwrap();
        let root = crate::policy::local_root_vpath(state.path()).unwrap();
        let dir = crate::policy::local_root_vpath(&state.path().join("inside")).unwrap();
        let mint =
            LocationMint::with_protected(vec![root.clone()], norte_vfs_local::Bounds::default());
        assert!(
            mint.mint_for(&dir, Some(".git"), true).is_none(),
            "not even the visible directory is served, because it is already under the protected root"
        );
    }

    /// With no approved capability NOTHING is minted: not even the directory
    /// is opened. It is the same gate the host enforces, one step earlier.
    #[test]
    fn with_no_capability_nothing_is_minted_and_the_directory_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"x").unwrap();
        let vpath = crate::policy::local_root_vpath(dir.path()).unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        // `run_column_values` only calls `mint_for` when the capability is
        // granted; here the observable half is pinned: with default
        // capabilities, `granted()` is false.
        assert!(
            !norte_plugin_host::Capabilities::default()
                .location
                .granted()
        );
        drop(mint.mint_for(&vpath, None, false));
        assert_eq!(mint.live_tokens(), 0);
    }

    #[test]
    fn column_values_checked_correct_and_different_length() {
        assert_eq!(
            column_values_checked(vec![Some("1".into()), None], 2),
            Some(vec![Some("1".to_string()), None])
        );
        assert_eq!(column_values_checked(vec![Some("1".into())], 2), None);
    }

    // -------------------------------------------------------------------
    // H3e: `has_help` at discovery + `help_of` on demand.

    #[test]
    fn the_wires_cap_and_the_hosts_are_the_same_number() {
        // `PLUGIN_HELP_MAX_BYTES` is NORMATIVE: the contract invites a
        // receiver to size against it. The host trims via
        // `Limits::untrusted()`. These are two crates that do not know each
        // other, so without this anchor they could silently drift apart and
        // the wire would promise a cap nobody enforces. `norte-core` depends
        // on both: it is the only place where the equality can be asserted.
        assert_eq!(
            norte_proto::methods::PLUGIN_HELP_MAX_BYTES,
            norte_help::Limits::untrusted().max_bytes
        );
    }

    #[test]
    fn help_of_returns_the_plugins_capped_markdown() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::write(
            tmp.path().join("plugins/org.norte.demo/help.md"),
            "+++\nid = \"org.norte.demo\"\ntitle = \"Demo\"\n+++\nbody",
        )
        .unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.list().plugins[0].has_help, "list announces it");
        let help = reg.help_of("org.norte.demo").expect("there is a page");
        assert!(help.markdown.contains("body"));
        assert!(!help.truncated && !help.lossy);
    }

    #[test]
    fn help_of_an_unknown_id_is_none() {
        // Fail-closed: the id comes from the WIRE. It is resolved against
        // the catalog and never composed into a path — a `../` never
        // touches the FS.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.help_of("../../etc/passwd").is_none());
        assert!(reg.help_of("other.plugin").is_none());
    }

    #[test]
    fn help_of_caps_a_huge_help_md_and_declares_it() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let big = "a".repeat(norte_help::Limits::untrusted().max_bytes + 4096);
        std::fs::write(tmp.path().join("plugins/org.norte.demo/help.md"), &big).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let help = reg.help_of("org.norte.demo").expect("there is a page");
        assert!(help.truncated, "a file over the cap is declared");
        assert!(help.markdown.len() <= norte_help::Limits::untrusted().max_bytes);
        assert!(help.markdown.len() < big.len(), "and it really was cut");
    }

    #[test]
    fn a_giant_help_md_is_not_loaded_whole_into_memory() {
        // A SPARSE 100 GiB `help.md` is a few bytes in a tarball. Reading it
        // whole to cap it AFTERWARD aborts on a reservation failure, or
        // invites the OOM killer to take down the daemon with its journal
        // and every task in flight. And `plugin.help` is OPEN to an agent on
        // a plugin that needs neither approval nor enablement: it would be
        // the first UNCAPPED read an agent could trigger in the daemon. The
        // cap is applied when READING, not when decoding.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let f = std::fs::File::create(tmp.path().join("plugins/org.norte.demo/help.md")).unwrap();
        // Sparse: not a byte written, so the fixture fits in any CI.
        f.set_len(100 * 1024 * 1024 * 1024).unwrap();
        drop(f);

        let start = std::time::Instant::now();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let help = reg.help_of("org.norte.demo").expect("there is a page");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "the capped read does not depend on the file's size"
        );
        assert!(
            !help.markdown.is_empty(),
            "the page is served CUT, not lost: with no cap, the 100 GiB \
             reservation fails and the file degrades to \"no page\" (and \
             where the reservation does go through, the OOM killer takes it)"
        );
        assert!(
            help.markdown.len() <= norte_help::Limits::untrusted().max_bytes,
            "what crosses the wire is still capped"
        );
        assert!(
            help.truncated,
            "and the cut is declared: reading max_bytes+1 is what lets \
             `cut_and_decode_untrusted` see that there was more"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_help_md_that_points_outside_the_directory_is_not_read() {
        // `help.md` crosses the wire and an AGENT can request it: a symlink
        // leaving the plugin's directory would turn `plugin.help` into an
        // arbitrary file read OUTSIDE the policy engine (the same shape as
        // issue #69's `plugin.wasm`). It is read as a blank page.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let outside = tmp.path().join("foreign.md");
        std::fs::write(&outside, "secret-from-elsewhere").unwrap();
        let link = tmp.path().join("plugins/org.norte.demo/help.md");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            !reg.list().plugins[0].has_help,
            "the wire flag uses the SAME guard as the reader: announcing \
             `true` and serving `\"\"` is the oracle \"that path exists and \
             is a regular file\", and both halves are read by an agent"
        );
        assert!(
            reg.announces_help("org.norte.demo"),
            "and the LAX flag still says the author put the file in: \
             without it, \"they put it in and the host refuses to serve \
             it\" would be indistinguishable from \"never documented\", and \
             `norte doctor` would have nothing to report"
        );
        let help = reg.help_of("org.norte.demo").expect("the plugin exists");
        assert_eq!(
            help.markdown, "",
            "there is no page, and it is not an error"
        );
        assert!(
            !help.markdown.contains("secret-from-elsewhere"),
            "content from outside the dir NEVER crosses the wire"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_help_md_linked_inside_the_directory_is_read() {
        // The guard is "do not escape the dir", NOT "no symlinks at all": a
        // plugin that organizes its own directory with links does nothing
        // wrong.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let dir = tmp.path().join("plugins/org.norte.demo");
        std::fs::write(
            dir.join("README.md"),
            "+++\nid = \"org.norte.demo\"\ntitle = \"Demo\"\n+++\nbody",
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.join("README.md"), dir.join("help.md")).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.list().plugins[0].has_help, "is_file follows the link");
        let help = reg.help_of("org.norte.demo").expect("there is a page");
        assert!(help.markdown.contains("body"));
    }

    #[test]
    fn an_unreadable_help_md_is_a_blank_page_not_an_error() {
        // Help is cosmetic: a `help.md` that cannot be read never takes down
        // the plugin nor the call.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::create_dir_all(tmp.path().join("plugins/org.norte.demo/help.md")).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let help = reg.help_of("org.norte.demo").expect("the plugin exists");
        assert_eq!(help.markdown, "");
    }
}

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Id of the installed plugin (read from the manifest, not from the
    /// source directory's name).
    pub id: String,
    /// Readable name declared in the manifest.
    pub name: String,
    /// `true` if a plugin with that id already existed and was replaced
    /// (only with `force`). When `true`, its consent has been WITHDRAWN.
    pub replaced: bool,
}

/// Why installation could not happen.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The source has no readable `plugin.toml`.
    #[error("no readable `plugin.toml` at {0}")]
    NoManifest(PathBuf),
    /// The manifest does not validate (id, capabilities, hooks…).
    #[error("invalid `plugin.toml`: {0}")]
    Manifest(#[from] norte_plugin_host::ManifestError),
    /// The source has no `plugin.wasm`.
    #[error("no `plugin.wasm` at {0}")]
    NoWasm(PathBuf),
    /// `plugin.wasm` exceeds the runtime's artifact cap: neither would the
    /// catalog read it nor would the runtime instantiate it, so it is not
    /// copied.
    #[error("`plugin.wasm` is {len} bytes and the cap is {cap}")]
    WasmTooLarge {
        /// Bytes of the file.
        len: u64,
        /// The cap.
        cap: u64,
    },
    /// A plugin with that id is already installed and replacing it was not requested.
    #[error(
        "`{0}` is already installed; replacing it WITHDRAWS its consent — repeat with `--force` if that is what you want"
    )]
    AlreadyInstalled(String),
    /// I/O error while copying.
    #[error("installing: {0}")]
    Io(#[from] io::Error),
}

/// Installs `src`'s plugin (a directory with `plugin.toml` + `plugin.wasm`)
/// under `config_dir/plugins/<id>/`.
///
/// The id comes from the MANIFEST, never from the source directory's name:
/// it is what the discoverer is going to use, and letting a directory named
/// something else decide where it lands would be a way to stomp on someone
/// else's.
///
/// **Installing is not consenting.** The plugin is left discovered and
/// unapproved; a human approves and enables it in the manager. An installer
/// that consented on its own would turn "I'm bringing this file" into "I'm
/// giving it its capabilities", which is the whole decision.
///
/// **Replacing WITHDRAWS consent**, and this is the part that is not
/// cosmetic: the approval digest covers the MANIFEST —capabilities,
/// category, contributions— and not the `.wasm`. Without withdrawing it,
/// installing over an already approved plugin would leave a new binary
/// running under the permission a human gave to another one. That is why
/// replacing requires `force` and, when it happens, the id's state is wiped.
///
/// # Errors
/// [`InstallError`] if the manifest or the `.wasm` is missing, if the
/// manifest does not validate, if the id is already installed without
/// `force`, or on I/O.
pub fn install(config_dir: &Path, src: &Path, force: bool) -> Result<InstallReport, InstallError> {
    let manifest_path = src.join("plugin.toml");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|_| InstallError::NoManifest(manifest_path.clone()))?;
    let manifest = norte_plugin_host::Manifest::from_toml(&raw)?;

    let wasm_src = src.join("plugin.wasm");
    if !wasm_src.is_file() {
        return Err(InstallError::NoWasm(wasm_src));
    }
    // The artifact cap is applied at the door: a binary the catalog is not
    // going to read (and the runtime is not going to instantiate) is not
    // copied into the config just so every discovery lists it as broken.
    let len = std::fs::metadata(&wasm_src)?.len();
    if len > norte_plugin_host::MAX_ARTIFACT_BYTES {
        return Err(InstallError::WasmTooLarge {
            len,
            cap: norte_plugin_host::MAX_ARTIFACT_BYTES,
        });
    }

    let dest = config_dir.join("plugins").join(&manifest.id);
    let replaced = dest.exists();
    if replaced && !force {
        return Err(InstallError::AlreadyInstalled(manifest.id.clone()));
    }

    std::fs::create_dir_all(&dest)?;
    std::fs::write(dest.join("plugin.toml"), &raw)?;
    std::fs::copy(&wasm_src, dest.join("plugin.wasm"))?;
    // Help travels with the plugin if it brings one (H3e); its absence is not an error.
    let help_src = src.join("help.md");
    if help_src.is_file() {
        std::fs::copy(&help_src, dest.join("help.md"))?;
    }

    if replaced {
        // Consent withdrawn: the `.wasm` is different and the manifest's
        // digest would not have noticed.
        //
        // The entry is OVERWRITTEN to "not approved" instead of removed from
        // the map: `persist_state` merges onto the existing document, so
        // removing the key from the map would leave it intact in the file —
        // the plugin would stay approved and nothing would say so. Writing
        // the disabled entry is also what a human would want to read in
        // `plugins-state.toml`: "this was approved and no longer is", not a
        // gap.
        let mut state = PluginRegistry::read_state(&config_dir.join(PluginRegistry::STATE_FILE))?;
        state.insert(manifest.id.clone(), PluginState::default());
        persist_state(config_dir, &state)?;
    }

    Ok(InstallReport {
        id: manifest.id,
        name: manifest.name,
        replaced,
    })
}

/// The core's schemes, which no provider plugin serves (ADR 0093).
/// Re-exported for whoever does not depend on `norte-plugin-host` (the CLI).
pub use norte_plugin_host::CORE_SCHEMES;
/// The catalog's TYPED load errors, for whoever diagnoses without depending
/// on `norte-plugin-host` (`norte doctor`, ADR 0094).
pub use norte_plugin_host::{LoadError, ManifestError};

/// The schemes the provider plugins INSTALLED under `config_dir` declare,
/// consented to or not, sorted and deduplicated.
///
/// This is for whoever has to decide whether an argument is a URL before
/// anyone connects (the CLI): routing `webdav://x` as a URL grants nothing,
/// and the connection is still fail-closed in
/// [`PluginRegistry::resolve_provider`]. An unreadable catalog is an empty
/// list: the CLI can do no better than treat the argument as a file.
///
/// Reads ONLY the manifests: the full catalog hashes every `plugin.wasm` and
/// resolves every `[config]`, and this is asked in order to route an argument.
#[must_use]
pub fn installed_provider_schemes(config_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(config_dir.join("plugins")) else {
        return Vec::new();
    };
    let mut schemes: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|d| std::fs::read_to_string(d.path().join("plugin.toml")).ok())
        .filter_map(|src| norte_plugin_host::Manifest::from_toml(&src).ok())
        .filter(|m| m.category == norte_plugin_host::Category::Provider)
        .flat_map(|m| m.contributions.provider.into_iter().map(|c| c.scheme))
        .collect();
    schemes.sort();
    schemes.dedup();
    schemes
}

/// What [`uninstall`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallReport {
    /// Uninstalled id.
    pub id: String,
    /// `true` if the plugin had consent (approved in the state): the report
    /// says so because that is what just stopped existing.
    pub was_approved: bool,
}

/// Why uninstallation could not happen.
#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    /// The id is not a plugin id (reverse-DNS). Rejected BEFORE touching the
    /// disk: the id becomes a path under `plugins/`, and a `..` would be a
    /// delete outside it.
    #[error("invalid plugin id: reverse-DNS expected (e.g. `org.foo.bar`)")]
    InvalidId,
    /// No plugin installed with that id.
    #[error("`{0}` is not installed")]
    NotInstalled(String),
    /// I/O error deleting or writing the state.
    #[error("uninstalling: {0}")]
    Io(#[from] io::Error),
}

/// Uninstalls plugin `id`: deletes `config_dir/plugins/<id>/` and leaves its
/// `plugins-state.toml` entry DISABLED.
///
/// Disabled and not removed, for the same reason as [`install`] with
/// `force`: `persist_state` merges onto the existing document, so removing
/// the key from the map would leave it intact in the file — and a plugin
/// with the same id installed later would inherit a consent nobody gave it.
///
/// The state is READ before anything is deleted: if `plugins-state.toml` is
/// corrupt, it fails with the directory intact. The other way around, a
/// delete followed by a failed read would leave the approval alive in the
/// file and a second `uninstall` answering "not installed" forever. The
/// delete goes before WRITING the state for the opposite reason: if it fails
/// halfway, what is left is a broken plugin the discoverer lists in
/// `errors`, not a whole plugin with its consent silently withdrawn.
///
/// A running daemon keeps its in-memory registry until it discovers again;
/// connecting by scheme always rediscovers, running a command fails for
/// lack of a binary.
///
/// # Errors
/// [`UninstallError`] if the id is not an id, if it is not installed, or on I/O.
pub fn uninstall(config_dir: &Path, id: &str) -> Result<UninstallReport, UninstallError> {
    if !norte_plugin_host::is_valid_plugin_id(id) {
        return Err(UninstallError::InvalidId);
    }
    let dir = config_dir.join("plugins").join(id);
    if !dir.is_dir() {
        return Err(UninstallError::NotInstalled(id.to_owned()));
    }
    let state_path = config_dir.join(PluginRegistry::STATE_FILE);
    let mut state = PluginRegistry::read_state(&state_path)?;
    let was_approved = state.get(id).is_some_and(|st| st.approved);

    std::fs::remove_dir_all(&dir)?;

    state.insert(id.to_owned(), PluginState::default());
    persist_state(config_dir, &state)?;

    Ok(UninstallReport {
        id: id.to_owned(),
        was_approved,
    })
}

// ---------- location for columns plugins (ADR 0057) ----------

/// Mints the OPAQUE tokens a columns guest uses to read under the directory
/// being listed, and resolves them while the call lives.
///
/// The guest never receives the path. It receives a random string that only
/// means something inside this process and only for as long as the call that
/// minted it lasts: when the [`LocationSession`] is dropped, the token stops
/// resolving. A token from the previous page is dead, and a guest that keeps
/// it gains nothing from it.
#[derive(Debug)]
pub(crate) struct LocationMint {
    bounds: norte_vfs_local::Bounds,
    /// Roots that are NOT opened even if a plugin asks for them (ADR 0052:
    /// the daemon's state directory is not bypassed just because whoever
    /// asks is a plugin instead of an agent).
    protected: Vec<norte_proto::VPath>,
    /// The ceiling for climbing to the root marker (#241): above the home
    /// there are no projects, there is system. `None` = no `$HOME`, and then
    /// `MAX_CLIMB` rules alone.
    home: Option<std::path::PathBuf>,
    live: std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<norte_vfs_local::ConfinedRoot>>,
    >,
}

/// The user's HOME, if the environment says so. Ceiling for the climb (#241).
fn home_from_env() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
}

/// Whether `p` can be written by ANY user on the machine.
///
/// A project-root marker inside a directory like that means nothing: anyone
/// can put it there. #241 closed the link case —`ln -s /nothing /tmp/.git`—
/// and left open the one that takes the least effort, `mkdir /tmp/.git`,
/// because the check was written to reject LINKS and a real directory is not
/// one. `/tmp`'s sticky bit stops deleting others' names; it does not stop
/// creating your own. With the marker planted, every pane under `/tmp`
/// handed the plugin the whole of `/tmp`: every user's temp files.
///
/// The `o+w` bit of the DIRECTORY that carries the marker is checked, not the
/// marker's: what decides who can plant it is the container's permission. A
/// normal repository is 0755 and is not affected.
///
/// A `stat` that fails says `true` —fail-closed—: if it cannot be known who
/// writes there, that root is not handed out.
#[cfg(unix)]
fn anyone_can_write_it(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    // Written in the negative on purpose: it is "true unless it is PROVEN
    // otherwise". An `is_ok_and(… != 0)` would say `false` when the stat
    // fails, i.e. it would open the root precisely when it is not known
    // whose it is.
    !std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o002 == 0)
}

/// On systems with no POSIX bits this check does not apply: Windows has its
/// own ACL story and confinement there is carried by #217.
#[cfg(not(unix))]
fn anyone_can_write_it(_p: &std::path::Path) -> bool {
    false
}

/// `(dev, ino)` of a path, or `None` if it could not be looked at.
///
/// `None` does not relax anything on its own: whoever receives it opens
/// without verifying, which is what was done before #241 — and a path that
/// cannot be `stat`ed is not going to be openable two lines later either.
fn node_id_of(p: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

impl LocationMint {
    /// A minter with this process's protected roots.
    pub(crate) fn new(bounds: norte_vfs_local::Bounds) -> std::sync::Arc<Self> {
        Self::with_protected(crate::policy::protected_roots(), bounds)
    }

    /// Like [`Self::new`], stating which roots are protected (tests).
    pub(crate) fn with_protected(
        protected: Vec<norte_proto::VPath>,
        bounds: norte_vfs_local::Bounds,
    ) -> std::sync::Arc<Self> {
        Self::with_protected_and_home(protected, bounds, home_from_env())
    }

    /// Like [`Self::with_protected`], also stating where the home is
    /// (tests): `$HOME` is the ceiling for the climb (#241) and a test
    /// cannot touch it —`std::env::set_var` is `unsafe` in the 2024 edition
    /// and rule 5 forbids it outside `norte-vfs-local`—, so the ceiling is
    /// INJECTED. Reading it once at construction, and not on every climb, is
    /// also the correct thing: the home does not change mid-process.
    pub(crate) fn with_protected_and_home(
        protected: Vec<norte_proto::VPath>,
        bounds: norte_vfs_local::Bounds,
        home: Option<std::path::PathBuf>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            bounds,
            protected,
            home,
            live: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Cap on the levels the root-marker search climbs. A project nested 64
    /// directories below its root is not a project.
    const MAX_CLIMB: usize = 64;

    /// Mints a token for `dir`, or `None` if that location is not served: it
    /// is not a local `file://`, it falls under a protected root, or it
    /// could not be opened. `None` is NOT an error for the request — the
    /// column stays empty and the pane goes on.
    ///
    /// `marker` is the project-root marker the manifest declares (`.git`):
    /// if given, what opens is the nearest ANCESTOR that contains it, and
    /// the session's `prefix` says which part of that root the user is
    /// looking at. With no marker —or if it does not appear above— the root
    /// is `dir` and the prefix is empty.
    ///
    /// `climb` is decided by the caller: climbing only happens for the HUMAN
    /// actor. An agent or a plugin are confined to their scope, and climbing
    /// past it would be exactly what the read gate prevents.
    ///
    /// BLOCKING (opens directories): goes inside `spawn_blocking`.
    pub(crate) fn mint_for(
        self: &std::sync::Arc<Self>,
        dir: &norte_proto::VPath,
        marker: Option<&str>,
        climb: bool,
    ) -> Option<LocationSession> {
        if dir.scheme() != "file" || dir.authority().is_some() {
            return None;
        }
        if self.is_protected(dir) {
            tracing::debug!("columns: location under a protected root, no token");
            return None;
        }
        let native = norte_vfs_local::vpath_to_native(dir).ok()?;
        let (root_native, prefix, expected) = if let Some(marker) = marker.filter(|_| climb) {
            self.climb_to_marker(dir, &native, marker)
        } else {
            let id = node_id_of(&native);
            (native, Vec::new(), id)
        };
        // Protected roots travel to the confinement (#238): the root not
        // BEING under one of them —what `is_protected` checks above— says
        // nothing about whether it CONTAINS one, and containing it is the
        // normal case (`$XDG_CONFIG_HOME` contains `norte/`). Without this,
        // a pane opened in the config directory served the plugin the
        // journal, the secrets, and the connections file.
        let denied: Vec<std::path::PathBuf> = self
            .protected
            .iter()
            .filter_map(|p| norte_vfs_local::vpath_to_native(p).ok())
            .collect();
        // `open_verified` and not `open`: what opens has to be the node this
        // function looked at to decide it was the root (#241).
        let root = norte_vfs_local::ConfinedRoot::open_verified(
            &root_native,
            self.bounds,
            &denied,
            expected,
        )
        .ok()?;
        let token = mint_token();
        self.live
            .lock()
            .expect("live lock is sound")
            .insert(token.clone(), std::sync::Arc::new(root));
        Some(LocationSession {
            mint: std::sync::Arc::clone(self),
            token,
            prefix,
        })
    }

    fn is_protected(&self, path: &norte_proto::VPath) -> bool {
        self.protected
            .iter()
            .any(|root| crate::policy::is_under(root, path))
    }

    /// Is `dir` a ceiling that is not opened as a hook's location (ADR
    /// 0100)? The system root —a `VPath` with no parent— and the home: above
    /// `$HOME` there is system, and the whole home is what a hook looking at
    /// "the mutation's directory" has no reason to receive when the mutation
    /// was a `mkdir ~/project`. What is not a local `file://` is not a
    /// ceiling: `mint_for` already refuses it for another reason.
    pub(crate) fn is_ceiling(&self, dir: &norte_proto::VPath) -> bool {
        if dir.scheme() != "file" || dir.authority().is_some() {
            return false;
        }
        if dir.parent().is_none() {
            return true;
        }
        match (&self.home, norte_vfs_local::vpath_to_native(dir)) {
            (Some(home), Ok(native)) => std::path::absolute(home).is_ok_and(|h| h == native),
            _ => false,
        }
    }

    /// The nearest ancestor that contains an entry named `marker`, and the
    /// path from it to `dir` in bytes. If there is none, `dir` itself with an
    /// empty prefix — it never climbs "just in case".
    ///
    /// The climb is cut off **at `$HOME`** (#241) and at [`Self::MAX_CLIMB`]
    /// levels.
    ///
    /// The `$HOME` ceiling is what stops a `touch $HOME/.git` —a badly
    /// extracted archive, a careless installer, any process of the user's—
    /// from turning every one of their directories that is not a repository
    /// into a root spanning the whole home. A marker IN `$HOME` is still
    /// valid: the ceiling is not going past it, not ignoring what is there.
    /// Above `$HOME` there are no projects, there is system.
    ///
    /// There used to also be a cutoff at the first protected root. It was
    /// dead code claiming to do something: `is_protected(p)` means "p is
    /// under a protected root", and if an ANCESTOR is, so is `dir`, so
    /// [`Self::mint_for`] already returned `None` before getting here. What
    /// is really needed —not handing out a root that CONTAINS a protected
    /// one— is done by the confinement with its `denied` list (#238).
    fn climb_to_marker(
        &self,
        dir: &norte_proto::VPath,
        native: &std::path::Path,
        marker: &str,
    ) -> (std::path::PathBuf, Vec<u8>, Option<(u64, u64)>) {
        use std::os::unix::ffi::OsStrExt as _;
        let home = self.home.as_deref();
        let mut prefix: Vec<Vec<u8>> = Vec::new();
        let mut current_v = dir.clone();
        let mut current_n = native.to_path_buf();
        for _ in 0..=Self::MAX_CLIMB {
            // The marker CANNOT be a symlink (#241). With a bare
            // `symlink_metadata().is_ok()`, even a dangling one worked: `ln
            // -s /nothing /tmp/.git` —and anyone can write to `/tmp`, since
            // the sticky bit stopping DELETION of others' names does not
            // stop CREATING one— made any pane under `/tmp` hand the plugin
            // all of `/tmp`. A real `.git` is a directory, or a worktree's
            // `gitdir:` file; neither is a link, and accepting links only
            // buys the attack.
            if current_n
                .join(marker)
                .symlink_metadata()
                .is_ok_and(|m| !m.file_type().is_symlink())
                && !anyone_can_write_it(&current_n)
            {
                // The node that was LOOKED AT, to demand it when opening:
                // between this decision and the `open`, the path is resolved
                // again from `/`, following links, and renaming a component
                // in between would swap the root for whichever one the
                // renamer wanted (#241).
                let id = node_id_of(&current_n);
                return (current_n, prefix.join(&b'/'), id);
            }
            // The ceiling: the marker is checked IN `$HOME` (above) and
            // nothing past it. Without this, `MAX_CLIMB` was the only limit
            // and a loose `.git` in the home would take the whole home with
            // it (#241).
            if home == Some(current_n.as_path()) {
                break;
            }
            let Some(parent_v) = current_v.parent() else {
                break;
            };
            let Some(parent_n) = current_n.parent().map(std::path::Path::to_path_buf) else {
                break;
            };
            let name = current_n
                .file_name()
                .map(|n| n.as_bytes().to_vec())
                .unwrap_or_default();
            prefix.insert(0, name);
            current_v = parent_v;
            current_n = parent_n;
        }
        (native.to_path_buf(), Vec::new(), node_id_of(native))
    }

    fn resolve(
        &self,
        token: &str,
    ) -> Result<std::sync::Arc<norte_vfs_local::ConfinedRoot>, String> {
        self.live
            .lock()
            .expect("live lock is sound")
            .get(token)
            .cloned()
            .ok_or_else(|| "unknown token".to_owned())
    }

    fn retire(&self, token: &str) {
        self.live.lock().expect("live lock is sound").remove(token);
    }

    /// How many tokens are still alive. A number other than 0 between pages
    /// is a session leak, so the tests watch it.
    #[cfg(test)]
    pub(crate) fn live_tokens(&self) -> usize {
        self.live.lock().expect("live lock is sound").len()
    }
}

/// One call's token. When dropped, the token stops resolving: that is the
/// whole expiration mechanism, and that is why there is no TTL to tune.
#[derive(Debug)]
pub(crate) struct LocationSession {
    pub(crate) mint: std::sync::Arc<LocationMint>,
    token: String,
    /// What part of the root the user is looking at, in bytes and with no
    /// trailing slash. Empty = the root IS the visible directory.
    prefix: Vec<u8>,
}

impl LocationSession {
    /// The token passed to the guest. Only the tests look at it: the real
    /// path uses [`Self::as_ref`], which carries token AND prefix together.
    #[cfg(test)]
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The (token, prefix) pair as it crosses to the guest.
    pub(crate) fn as_ref(&self) -> norte_plugin_host::columns_iface::LocationRef {
        norte_plugin_host::columns_iface::LocationRef {
            token: self.token.clone(),
            prefix: self.prefix.clone(),
        }
    }

    /// The same pair, for a PANEL guest (phase 3).
    ///
    /// A separate method and not a generic because the two types are
    /// distinct even though they have the same shape: `norte:panel` is
    /// another WIT package, and WIT does not share types between packages
    /// (ADR 0094). What is shared is the session —a single token, a single
    /// retirement in its `Drop`—, which is what really matters not to
    /// duplicate.
    pub(crate) fn as_ref_panel(&self) -> norte_plugin_host::panel_iface::LocationRef {
        norte_plugin_host::panel_iface::LocationRef {
            token: self.token.clone(),
            prefix: self.prefix.clone(),
        }
    }
}

impl Drop for LocationSession {
    fn drop(&mut self) {
        self.mint.retire(&self.token);
    }
}

/// 32 random bytes in hex: neither guessable nor derivable from the path.
fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    // From the system's CSPRNG: a token derivable from the path or from a
    // counter would be guessable from ANOTHER plugin in the same process.
    getrandom::fill(&mut bytes).expect("the system's CSPRNG does not fail");
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

impl norte_plugin_host::LocationHost for LocationMint {
    fn read(&self, token: &str, rel: &[u8]) -> Result<Vec<u8>, String> {
        self.resolve(token)?.read(rel).map_err(|e| e.to_string())
    }

    fn read_prefix(&self, token: &str, rel: &[u8], max: u64) -> Result<Vec<u8>, String> {
        self.resolve(token)?
            .read_prefix(rel, max)
            .map_err(|e| e.to_string())
    }

    fn stat(
        &self,
        token: &str,
        rel: &[u8],
    ) -> Result<norte_plugin_host::location_iface::Meta, String> {
        let meta = self.resolve(token)?.stat(rel).map_err(|e| e.to_string())?;
        Ok(meta_to_wire(&meta))
    }

    fn list_dir(
        &self,
        token: &str,
        rel: &[u8],
    ) -> Result<Vec<norte_plugin_host::location_iface::Dirent>, String> {
        let entries = self.resolve(token)?.list(rel).map_err(|e| e.to_string())?;
        Ok(entries
            .into_iter()
            .map(|e| norte_plugin_host::location_iface::Dirent {
                name: e.name,
                kind: kind_to_wire(e.kind),
            })
            .collect())
    }
}

/// What changes from one repaint to the next: where the panel is looking,
/// what size it has, what the guest saved, and what just happened.
///
/// Together in one struct and not as six parameters because they are ONE
/// thing —the call— and because separated they were eight arguments, which
/// is more than anyone reads in one go.
#[derive(Debug, Clone, Copy)]
pub struct PanelCall<'a> {
    /// The directory the panel is looking at. It is the location's root AS
    /// IS, not its parent (#239).
    pub dir: &'a norte_proto::VPath,
    /// Whether it can climb looking for the project's root marker. Only the
    /// human.
    pub climb: bool,
    /// Which of the plugin's panels is painted.
    pub kind: &'a str,
    /// Size, language, and the row under the cursor.
    // NOTE(translation): field name kept as `contexto` — constructed by
    // name in core/src/daemon/server.rs (T04) and core/src/backend/plugins.rs
    // (T03), outside this task's scope. See phase-1 report.
    pub contexto: &'a norte_plugin_host::panel_iface::PanelContext,
    /// What the guest saved last time, opaque.
    pub state: &'a [u8],
    /// What triggered this repaint.
    // NOTE(translation): field name kept as `evento`, same cross-file reason.
    pub evento: &'a norte_plugin_host::panel_iface::PanelEvent,
}

/// Paints an ALREADY resolved plugin's `kind` panel, blocking.
///
/// The same function for the daemon and for the embedded backend, and that
/// is the point: they are two paths to the same guest, and writing twice
/// when a location is minted —or how long it lives— is the divergence ADR
/// 0077 goes after. Here `ntc` with no daemon and `ntc` with a daemon cannot
/// paint differently.
///
/// Returns the id alongside the frame because the tuple already carried it
/// and whoever sends it to the wire needs it.
///
/// `climb` is only set by the HUMAN: an agent is confined to its scope, and
/// climbing past it looking for the project's root is what the read gate
/// prevents.
///
/// # Errors
/// Whatever the runtime says: instantiating can fail and the guest can crash
/// or run past its epoch. A failure here is "no frame", never a screen with
/// an error — the cosmetic degrades.
pub fn render_panel_blocking(
    runtime: &norte_plugin_host::PluginRuntime,
    resolved: ResolvedPreviewer,
    call: &PanelCall<'_>,
) -> Result<(String, norte_plugin_host::PanelFrame), norte_plugin_host::RuntimeError> {
    let &PanelCall {
        dir,
        climb,
        kind,
        contexto: context,
        state,
        evento: event,
    } = call;
    let (id, _name, wasm, caps, settings) = resolved;
    // The PRODUCTION mint, which brings the policy's protected roots; with
    // no permission nothing is minted and the panel is painted with what
    // the context tells it, which is an honest degradation.
    let mint = LocationMint::new(norte_vfs_local::Bounds::default());
    let host: Option<std::sync::Arc<dyn norte_plugin_host::LocationHost>> =
        caps.location.granted().then(|| {
            std::sync::Arc::clone(&mint) as std::sync::Arc<dyn norte_plugin_host::LocationHost>
        });
    let mut inst = runtime.instantiate_panel(&wasm, caps.clone(), host)?;
    inst.set_settings(settings);
    // The session lives as long as THIS call and retires itself when
    // dropped: what is kept between repaints is the guest's opaque state,
    // never the read permission.
    let session = caps
        .location
        .granted()
        .then(|| mint.mint_for(dir, caps.location_root_marker.as_deref(), climb))
        .flatten();
    let location_ref = session.as_ref().map(LocationSession::as_ref_panel);
    let frame = inst.render_panel(kind, context, location_ref.as_ref(), state, event)?;
    Ok((id, frame))
}

/// The frame the guest returned, in the wire's shape.
///
/// Shared for the same reason as [`render_panel_blocking`]: two copies of
/// this translation would end up disagreeing on something small —color as
/// three bytes or as a string, whether an empty state travels or not— and
/// the discrepancy would only show up with a plugin in front of it.
#[must_use]
pub fn panel_frame_to_wire(
    plugin_id: String,
    frame: norte_plugin_host::PanelFrame,
) -> norte_proto::methods::PanelFrame {
    let lines = frame
        .lines
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|s| norte_proto::methods::SpanWire {
                    text: s.text,
                    role: s.role,
                    // The SAME type as a styled preview, and with the same
                    // color shape: three bytes, not a hex string. A second
                    // encoding of the same concept is what someone ends up
                    // validating differently.
                    fg: s.fg.map(|(r, g, b)| [r, g, b]),
                    bg: s.bg.map(|(r, g, b)| [r, g, b]),
                })
                .collect()
        })
        .collect();
    let hits = frame
        .hits
        .into_iter()
        .map(|h| norte_proto::methods::PanelHit {
            row: h.row,
            col: h.col,
            width: h.width,
            command: h.command,
            arg: h.arg,
        })
        .collect();
    norte_proto::methods::PanelFrame {
        plugin_id,
        lines,
        hits,
        // The bytes as is: the wire encodes and caps them on its own
        // (`panel_state_wire`). An empty state does not travel.
        state: (!frame.state.is_empty()).then_some(frame.state),
    }
}

/// The wire's event, in the shape the guest understands.
///
/// `Refresh` AND what this binary does not know, together on purpose: they
/// are the same case. [`norte_proto::methods::PanelEvent`] is
/// `#[non_exhaustive]` so it can grow without breaking anyone, so a newer
/// client can send a future variant, and the correct destination is the
/// NEUTRAL event — the panel repaints with what is there. Discarding it
/// would leave the slot frozen without saying why.
#[must_use]
pub fn panel_event_to_host(
    ev: &norte_proto::methods::PanelEvent,
) -> norte_plugin_host::panel_iface::PanelEvent {
    use norte_plugin_host::panel_iface as pif;
    match ev {
        norte_proto::methods::PanelEvent::Click { row, col } => pif::PanelEvent::Click(pif::Cell {
            row: *row,
            col: *col,
        }),
        norte_proto::methods::PanelEvent::Command { command } => {
            pif::PanelEvent::Command(command.clone())
        }
        _ => pif::PanelEvent::Refresh,
    }
}

#[cfg(test)]
mod panel_helpers_tests {
    use norte_plugin_host::panel_iface as pif;
    use norte_proto::methods::PanelEvent;

    /// Every wire event reaches the guest as its own, and what this binary
    /// does not know reaches it as the NEUTRAL one.
    ///
    /// The last part is what matters: `PanelEvent` is `#[non_exhaustive]` so
    /// it can grow, so a newer client can send a variant this daemon does
    /// not have. Discarding it would leave the slot frozen without saying
    /// why; repainting with what is there is the honest degradation. The
    /// test exists because that wildcard reads like an oversight.
    #[test]
    fn an_unknown_event_translates_to_the_neutral_one() {
        assert!(matches!(
            super::panel_event_to_host(&PanelEvent::Click { row: 2, col: 5 }),
            pif::PanelEvent::Click(pif::Cell { row: 2, col: 5 })
        ));
        assert!(matches!(
            super::panel_event_to_host(&PanelEvent::Command {
                command: "git.fetch".to_owned()
            }),
            pif::PanelEvent::Command(c) if c == "git.fetch"
        ));
        assert!(matches!(
            super::panel_event_to_host(&PanelEvent::Refresh),
            pif::PanelEvent::Refresh
        ));
    }
}

fn kind_to_wire(
    kind: norte_vfs_local::LocationKind,
) -> norte_plugin_host::location_iface::EntryKind {
    use norte_plugin_host::location_iface::EntryKind as Wire;
    match kind {
        norte_vfs_local::LocationKind::File => Wire::File,
        norte_vfs_local::LocationKind::Dir => Wire::Dir,
        norte_vfs_local::LocationKind::Symlink => Wire::Symlink,
        norte_vfs_local::LocationKind::Other => Wire::Other,
    }
}

fn meta_to_wire(meta: &norte_vfs_local::LocationMeta) -> norte_plugin_host::location_iface::Meta {
    norte_plugin_host::location_iface::Meta {
        kind: kind_to_wire(meta.kind),
        size: meta.size,
        mtime_sec: meta.mtime_sec,
        mtime_nsec: meta.mtime_nsec,
        ctime_sec: meta.ctime_sec,
        ctime_nsec: meta.ctime_nsec,
        ino: meta.ino,
        dev: meta.dev,
        mode: meta.mode,
    }
}

/// Runs `column-values` of ONE already resolved plugin, with a location if it
/// was granted one. The ONLY place where that happens: the daemon and the
/// embedded backend call here, because a capability required on one path and
/// not on the other is the bug this repository has already been written
/// three times over (#165, #201, #181).
///
/// Fail-closed at every edge — it does not instantiate, it traps, it breaks
/// the positional contract, or the location cannot be opened: the page comes
/// out with empty cells, never an error that takes down the listing.
///
/// BLOCKING (instantiates WASM and opens a directory): goes in
/// `spawn_blocking`. [`run_column_values`] for the e2e tests: the exact same
/// path as the daemon and the embedded backend, exposed because the test
/// that matters —the official plugin installed like a third party's— lives
/// outside this crate. Re-exporting the function is preferable to the test
/// building its own version of the path, which is how two paths drift apart.
/// **Only with the `testing` feature** (#241): in the published library this
/// was a minting path with NO policy —it takes `location_dir` and `climb` as
/// is, and `climb` is only the human actor's—, available to anyone depending
/// on this crate.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
#[must_use]
pub fn run_column_values_for_test(
    runtime: &norte_plugin_host::PluginRuntime,
    resolved: ResolvedDecorator,
    column_id: &str,
    location_dir: Option<&norte_proto::VPath>,
    climb: bool,
    entries: &[Vec<u8>],
    expected_len: usize,
) -> Vec<Option<String>> {
    run_column_values(
        runtime,
        resolved,
        column_id,
        location_dir,
        climb,
        entries,
        expected_len,
    )
}

/// How many characters of a guest's phrase cross the wire (#332).
pub const GUEST_REASON_MAX_CHARS: usize = 200;

/// The phrase a guest refused with, ready to be shown (#332): THIRD-PARTY
/// text, so terminal dangers (escapes, controls, bidi) are masked and it is
/// cut to [`GUEST_REASON_MAX_CHARS`]. The same criterion as `built_against`
/// in `doctor`: never raw, never uncapped.
#[must_use]
pub fn guest_reason(raw: &str) -> String {
    let masked = norte_encoding::mask_terminal_hazards(raw);
    if masked.chars().count() <= GUEST_REASON_MAX_CHARS {
        return masked;
    }
    let mut cut: String = masked.chars().take(GUEST_REASON_MAX_CHARS).collect();
    cut.push('…');
    cut
}

/// What [`run_rename_plan`] returns: the plan, or why there is none.
#[derive(Debug)]
pub enum RenamePlanOutcome {
    /// The pairs the plugin proposes, already stripped of identities and of
    /// names that were not in the request.
    Plan(Vec<norte_proto::methods::AiRenameEntry>),
    /// The guest refused with a phrase for the reader. Third-party text.
    Refused(String),
    /// The guest did not instantiate, trapped, or exceeded the caps.
    Failed,
}

/// Asks a `renamer` plugin (C3, ADR 0095) for its plan for `names` in
/// `location_dir`, with the SAME location session as a column: it is minted
/// here and dies on exit. The plan that comes out is `ai.rename_plan`'s, by
/// another producer, and what makes the operation safe comes afterward —the
/// review, `fs.rename_batch_plan`, the journal—, so here it is only cleaned:
/// out go the identity pairs and the `current`s that were not requested.
pub fn run_rename_plan(
    runtime: &norte_plugin_host::PluginRuntime,
    resolved: ResolvedDecorator,
    renamer_id: &str,
    location_dir: Option<&norte_proto::VPath>,
    climb: bool,
    names: &[String],
) -> RenamePlanOutcome {
    let (id, _name, wasm, caps, settings) = resolved;
    let session = if caps.location.granted() {
        let mint = LocationMint::new(norte_vfs_local::Bounds::default());
        location_dir.and_then(|dir| mint.mint_for(dir, caps.location_root_marker.as_deref(), climb))
    } else {
        None
    };
    let host: Option<std::sync::Arc<dyn norte_plugin_host::LocationHost>> =
        session.as_ref().map(|s| {
            std::sync::Arc::clone(&s.mint) as std::sync::Arc<dyn norte_plugin_host::LocationHost>
        });
    let Ok(mut inst) = runtime.instantiate_renamer_with_location(&wasm, caps, host) else {
        tracing::warn!(plugin = %id, "renamer: failed to instantiate");
        return RenamePlanOutcome::Failed;
    };
    inst.set_settings(settings);
    let location_ref = session.as_ref().map(LocationSession::as_ref).map(|r| {
        norte_plugin_host::renamer_iface::LocationRef {
            token: r.token,
            prefix: r.prefix,
        }
    });
    let pairs = match inst.plan(renamer_id, location_ref.as_ref(), names) {
        Ok(Ok(p)) => p,
        Ok(Err(reason)) => return RenamePlanOutcome::Refused(reason),
        Err(e) => {
            tracing::warn!(plugin = %id, error = %e, "renamer: failed to run");
            return RenamePlanOutcome::Failed;
        }
    };
    let requested: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
    RenamePlanOutcome::Plan(
        pairs
            .into_iter()
            .filter(|p| p.current != p.proposed && requested.contains(p.current.as_str()))
            .map(|p| norte_proto::methods::AiRenameEntry {
                from: p.current,
                to: p.proposed,
            })
            .collect(),
    )
}

/// What [`run_organize_plan`] returns (phase 8): the plan, or why there is
/// none.
#[derive(Debug)]
pub enum OrganizePlanOutcome {
    /// The moves the plugin proposes, already cleaned and VALIDATED.
    Plan(Vec<norte_proto::methods::OrganizeMove>),
    /// The guest refused with a phrase for the reader. Third-party text.
    Refused(String),
    /// The guest did not instantiate, trapped, or exceeded the caps.
    Failed,
    /// The guest proposed a destination that leaves the directory (a `..`, an
    /// absolute path, an empty segment…).
    ///
    /// It is a case SEPARATE from [`Self::Failed`] on purpose: a plugin that
    /// traps is broken, and one that proposes writing outside the directory
    /// is doing something else. The reader deserves to know which of the
    /// two, and the operator has the trace with the plugin's id.
    Escapes,
}

/// Asks an `organizer` plugin (phase 8) for its plan for `names` in
/// `location_dir`, with the same location session as a renamer.
///
/// **Here the destination IS validated, and that is what distinguishes this
/// path from the renamer's.** A renamer proposes a name, which `Segment`
/// already caps; an organizer proposes a PATH, and a `..` there is a write
/// outside the directory the human is looking at. It is checked with
/// [`norte_proto::methods::validar_proposed_rel`] — the SAME function the
/// core applies when executing and the same one that validates a model's
/// plan — and one single bad destination brings down the whole plan:
/// applying "what could be done" from a proposal that carried that would be
/// keeping half of something nobody reviewed.
pub fn run_organize_plan(
    runtime: &norte_plugin_host::PluginRuntime,
    resolved: ResolvedDecorator,
    organizer_id: &str,
    location_dir: Option<&norte_proto::VPath>,
    climb: bool,
    names: &[String],
) -> OrganizePlanOutcome {
    let (id, _name, wasm, caps, settings) = resolved;
    let session = if caps.location.granted() {
        let mint = LocationMint::new(norte_vfs_local::Bounds::default());
        location_dir.and_then(|dir| mint.mint_for(dir, caps.location_root_marker.as_deref(), climb))
    } else {
        None
    };
    let host: Option<std::sync::Arc<dyn norte_plugin_host::LocationHost>> =
        session.as_ref().map(|s| {
            std::sync::Arc::clone(&s.mint) as std::sync::Arc<dyn norte_plugin_host::LocationHost>
        });
    let Ok(mut inst) = runtime.instantiate_organizer_with_location(&wasm, caps, host) else {
        tracing::warn!(plugin = %id, "organizer: failed to instantiate");
        return OrganizePlanOutcome::Failed;
    };
    inst.set_settings(settings);
    let location_ref = session.as_ref().map(LocationSession::as_ref).map(|r| {
        norte_plugin_host::organizer_iface::LocationRef {
            token: r.token,
            prefix: r.prefix,
        }
    });
    let moves = match inst.plan(organizer_id, location_ref.as_ref(), names) {
        Ok(Ok(p)) => p,
        Ok(Err(reason)) => return OrganizePlanOutcome::Refused(reason),
        Err(e) => {
            tracing::warn!(plugin = %id, error = %e, "organizer: failed to run");
            return OrganizePlanOutcome::Failed;
        }
    };
    let requested: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut out = Vec::with_capacity(moves.len());
    for m in moves {
        // A `current` that was not requested is a plan for another
        // directory: it is silently discarded, as the renamer does.
        if !requested.contains(m.current.as_str()) {
            continue;
        }
        // A destination that escapes is NOT silently discarded: it brings
        // down the plan and is reported. Discarding it would leave the
        // reader looking at a proposal missing rows with no idea why.
        if norte_proto::methods::validar_proposed_rel(&m.proposed_rel).is_err() {
            tracing::warn!(
                plugin = %id,
                "organizer: proposed a destination outside the directory; the whole plan is rejected"
            );
            return OrganizePlanOutcome::Escapes;
        }
        // A move to where it already is is not a move.
        if m.proposed_rel == m.current {
            continue;
        }
        out.push(norte_proto::methods::OrganizeMove {
            current: m.current,
            proposed_rel: m.proposed_rel,
        });
    }
    OrganizePlanOutcome::Plan(out)
}

pub(crate) fn run_column_values(
    runtime: &norte_plugin_host::PluginRuntime,
    resolved: ResolvedDecorator,
    column_id: &str,
    location_dir: Option<&norte_proto::VPath>,
    climb: bool,
    entries: &[Vec<u8>],
    expected_len: usize,
) -> Vec<Option<String>> {
    let (id, _name, wasm, caps, settings) = resolved;
    // The session lives until the end of this function and not an instant
    // longer: when dropped, the token stops resolving.
    let session = if caps.location.granted() {
        let mint = LocationMint::new(norte_vfs_local::Bounds::default());
        location_dir.and_then(|dir| mint.mint_for(dir, caps.location_root_marker.as_deref(), climb))
    } else {
        None
    };
    let host: Option<std::sync::Arc<dyn norte_plugin_host::LocationHost>> =
        session.as_ref().map(|s| {
            std::sync::Arc::clone(&s.mint) as std::sync::Arc<dyn norte_plugin_host::LocationHost>
        });
    let Ok(mut inst) = runtime.instantiate_columns_with_location(&wasm, caps, host) else {
        tracing::warn!(plugin = %id, "columns: failed to instantiate, empty cells");
        return vec![None; expected_len];
    };
    inst.set_settings(settings);
    let location_ref = session.as_ref().map(LocationSession::as_ref);
    let Ok(raw) = inst.column_values(column_id, location_ref.as_ref(), entries) else {
        tracing::warn!(plugin = %id, "columns: failed to run, empty cells");
        return vec![None; expected_len];
    };
    column_values_checked(raw, expected_len).unwrap_or_else(|| {
        tracing::warn!(
            plugin = %id,
            "columns: length does not match the positional contract, empty cells"
        );
        vec![None; expected_len]
    })
}

/// How many live instances are kept at once. Eight because a page is two
/// panels and a few columns: above that, what is kept is memory for
/// directories nobody is looking at anymore.
const POOL_MAX: usize = 8;

/// How long an instance survives unused. One minute is "the reader is still
/// paging around here"; past that, whoever comes back would rather not be
/// paying for the guest's memory for a directory they left behind.
const POOL_TTL: std::time::Duration = std::time::Duration::from_mins(1);

/// A LIVE columns instance, with what is needed to know whether it is still
/// serving.
struct EnPool {
    /// `(plugin id, wasm, location on the wire)`. The location goes into the
    /// key because it is what the guest caches inside: a parsed
    /// `.git/index` is worthless for another project.
    // The artifact carries the approved fingerprint (ADR 0142): a pool
    // instance does not serve a newly approved binary.
    key: (String, norte_plugin_host::WasmArtifact, String),
    /// The permissions it was instantiated with. If the catalog resolves
    /// different ones —a withdrawn consent, a reinstalled manifest— the
    /// instance is DISCARDED: reusing it would mean running with permissions
    /// nobody grants anymore.
    caps: norte_plugin_host::Capabilities,
    /// Who resolves this instance's tokens. Survives the call; what does not
    /// survive is the SESSION, minted and dropped on every one.
    mint: std::sync::Arc<LocationMint>,
    inst: norte_plugin_host::ColumnsInstance,
    last_used: std::time::Instant,
}

/// Columns instances reused across pages (#224).
///
/// The measured cost of a twenty-row page over a two-thousand-entry git index
/// was **167 ms**, with the WASM component instantiated and `.git/index`
/// parsed from scratch on every call. None of that is work that changes
/// between page 1 and page 2 of the same directory.
///
/// **What the pool buys is not just wall clock.** Freshness is deliberately
/// the guest's problem (the host cannot know what its answer depends on),
/// and a guest that does not survive the call cannot cache ANYTHING: without
/// a pool, that cache is not merely unused, it is forbidden.
///
/// Lives here, next to `run_column_values` —private, so no link—, because
/// both paths need it: the daemon hangs it off its shared state and the
/// embedded backend off its own. Recreating it for one and not the other is
/// exactly the #165/#201/#181 asymmetry.
///
/// **What the pool does NOT keep is a live token.** The location session is
/// minted at the start of each call and dropped at its end —its `Drop`
/// retires it from the minter—, so between page and page the stored instance
/// has a `LocationHost` that resolves nothing.
#[derive(Default)]
pub struct ColumnPool {
    /// Most recent last. Eight at most, so a `Vec` with linear search is
    /// faster —and much easier to read— than a map with usage order on the
    /// side.
    live: std::sync::Mutex<Vec<EnPool>>,
    /// How many calls found their instance already alive.
    ///
    /// This is what makes the pool TESTABLE without a stopwatch: that the
    /// second page takes less time is the symptom, and a symptom measured in
    /// milliseconds turns red the day the machine is under load. That the
    /// instance was reused is the fact, and it is deterministic.
    reutilizadas: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for ColumnPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // By hand and not derived because a `ColumnsInstance` has no useful
        // `Debug` (its wasmtime `Store` does not have one), so what is
        // printed is HOW MANY there are, not which ones.
        let n = self.live.lock().map_or(0, |v| v.len());
        f.debug_struct("ColumnPool")
            .field("live", &n)
            .field("reutilizadas", &self.reutilizadas)
            .finish()
    }
}

impl ColumnPool {
    /// The column's values, reusing this `(plugin, location)`'s instance if
    /// it is still alive and with the same permissions.
    ///
    /// How many calls found their instance alive. See [`Self::reutilizadas`].
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    #[must_use]
    pub fn reutilizadas(&self) -> u64 {
        self.reutilizadas.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// [`Self::column_values`] for the e2e tests, for the same reason and
    /// with the same caveat as [`run_column_values_for_test`]: the test that
    /// matters lives outside this crate, and building its own version of the
    /// path there is how two paths drift apart.
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the SAME list as `run_column_values`, on purpose"
    )]
    pub fn column_values_for_test(
        &self,
        runtime: &norte_plugin_host::PluginRuntime,
        resolved: ResolvedDecorator,
        column_id: &str,
        location_dir: Option<&norte_proto::VPath>,
        climb: bool,
        entries: &[Vec<u8>],
        expected_len: usize,
    ) -> Vec<Option<String>> {
        self.column_values(
            runtime,
            resolved,
            column_id,
            location_dir,
            climb,
            entries,
            expected_len,
        )
    }

    /// The same contract as `run_column_values`, down to the degradation:
    /// what cannot be done comes out as empty cells, never as an error that
    /// takes down the listing. And equally BLOCKING: it goes in
    /// `spawn_blocking`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the SAME list as `run_column_values`, on purpose"
    )]
    pub(crate) fn column_values(
        &self,
        runtime: &norte_plugin_host::PluginRuntime,
        resolved: ResolvedDecorator,
        column_id: &str,
        location_dir: Option<&norte_proto::VPath>,
        climb: bool,
        entries: &[Vec<u8>],
        expected_len: usize,
    ) -> Vec<Option<String>> {
        let (id, name, wasm, caps, settings) = resolved;
        let key = (
            id.clone(),
            wasm.clone(),
            location_dir
                .map(norte_proto::VPath::to_wire)
                .unwrap_or_default(),
        );
        let Ok(mut live) = self.live.lock() else {
            // A poisoned mutex is no reason to leave everyone without
            // columns: it falls back to the no-pool path, the usual one.
            tracing::warn!("columns: poisoned pool, instantiating without reuse");
            return run_column_values(
                runtime,
                (id, name, wasm, caps, settings),
                column_id,
                location_dir,
                climb,
                entries,
                expected_len,
            );
        };
        let now = std::time::Instant::now();
        live.retain(|e| now.duration_since(e.last_used) < POOL_TTL);
        let found = live
            .iter()
            .position(|e| e.key == key && e.caps == caps)
            .map(|i| live.remove(i));
        // The instance leaves the pool while it is used: the mutex is
        // released before entering the guest, which is the long call, and
        // two pages of the same directory at once instantiate separately
        // instead of serializing.
        drop(live);

        let mut entry = if let Some(e) = found {
            self.reutilizadas
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            e
        } else {
            let mint = LocationMint::new(norte_vfs_local::Bounds::default());
            let host: Option<std::sync::Arc<dyn norte_plugin_host::LocationHost>> =
                if caps.location.granted() {
                    Some(std::sync::Arc::clone(&mint)
                        as std::sync::Arc<dyn norte_plugin_host::LocationHost>)
                } else {
                    None
                };
            let Ok(inst) = runtime.instantiate_columns_with_location(&wasm, caps.clone(), host)
            else {
                tracing::warn!(plugin = %id, "columns: failed to instantiate, empty cells");
                return vec![None; expected_len];
            };
            EnPool {
                key,
                caps,
                mint,
                inst,
                last_used: now,
            }
        };

        // The session is minted HERE and dies at the end of this function,
        // whether a new or a reused instance uses it: what is kept between
        // pages is the guest and its memory, never the read permission.
        let session = if entry.caps.location.granted() {
            location_dir.and_then(|dir| {
                entry
                    .mint
                    .mint_for(dir, entry.caps.location_root_marker.as_deref(), climb)
            })
        } else {
            None
        };
        entry.inst.set_settings(settings);
        let location_ref = session.as_ref().map(LocationSession::as_ref);
        let output = entry
            .inst
            .column_values(column_id, location_ref.as_ref(), entries);
        drop(session);

        let raw = match output {
            Ok(raw) => raw,
            Err(e) => {
                // A failed instance does NOT go back to the pool: a guest
                // that trapped may have left its linear memory half done,
                // and reusing it would mean serving that half on the next
                // page.
                tracing::warn!(plugin = %id, error = %e, "columns: failed to run, empty cells");
                return vec![None; expected_len];
            }
        };
        entry.last_used = std::time::Instant::now();
        if let Ok(mut live) = self.live.lock() {
            live.push(entry);
            // The OLDEST fall off the top, which is what makes this an
            // LRU: every use puts its own back at the end.
            if live.len() > POOL_MAX {
                let excess = live.len() - POOL_MAX;
                live.drain(..excess);
            }
        }
        column_values_checked(raw, expected_len).unwrap_or_else(|| {
            tracing::warn!(
                plugin = %id,
                "columns: length does not match the positional contract, empty cells"
            );
            vec![None; expected_len]
        })
    }
}
