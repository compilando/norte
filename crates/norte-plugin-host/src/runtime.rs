//! wasmtime runtime of the plugin host (M4-P2, ADR 0022 D1/D4): engine,
//! EMPTY WASI sandbox, Component Model instantiation and the host-side
//! ENFORCEMENT of capabilities (`fs-read`).
//!
//! Enforcement lives in the HOST, not the guest (ADR 0022 D4): a hostile
//! plugin cannot evade the gating because the check is made in the host
//! implementation of `host-log::read-scoped`, before handing over bytes.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use thiserror::Error;
use wasmtime::component::{Component, Linker, ResourceAny, ResourceTable};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::NortePlugin;
use crate::bindings::exports::norte::plugin::previewer::{PreviewInput, Span};
use crate::bindings::norte::host::{host_config, host_log};
use crate::capability::Capabilities;

/// Cap on log lines a plugin can accumulate (anti-DoS: the guest cannot
/// grow the host's memory without limit via `host-log::log`).
const MAX_LOGS: usize = 1024;

/// Cap on characters per log line: a guest cannot grow the host's memory
/// with a single giant line (anti-DoS, complements [`MAX_LOGS`]).
/// Truncated on a char boundary (never splits a code point).
const MAX_LOG_CHARS: usize = 4096;

/// Linear memory limit per guest store (64 MiB, generous): a plugin cannot
/// exhaust the host's RAM by growing its linear memory without end.
const MAX_STORE_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Cap on the guest's RETURN value (`run_command`/`render_preview`), in
/// bytes (issue #68): a guest cannot grow the host's memory by returning a
/// giant `String`. 4 MiB is generous for preview text or a status bar
/// message, and consistent with the core's 1 MiB read cap when previewing.
/// Above it, it is REJECTED (fail-loud), never truncated halfway — a cut
/// value is not the one the plugin meant to return.
const MAX_RETURN_BYTES: usize = 4 * 1024 * 1024;

/// Cap on the LINES of a `render-styled` (ADR 0037 decision table 1):
/// anti-DoS on the number of lines a guest can return from a styled
/// preview. Applied POST-return from the guest (the ADR's hard rule:
/// reject the whole thing, never truncate halfway).
const MAX_STYLED_LINES: usize = 10_000;

/// Cap on SPANS per line of a `render-styled` (ADR 0037 decision table 1).
/// It used to be 64; an image previewer paints ONE span per cell (`▀` with
/// its `fg`/`bg`, 0.9.0) and 64 cells is a thumbnail, so D4's amendment
/// raises it to the width of a large terminal. The total-bytes cap still
/// bounds the set.
const MAX_STYLED_SPANS_PER_LINE: usize = 256;

/// Cap on UTF-8 bytes of ONE span's `text` (ADR 0037 decision table 1:
/// "4 KiB"). Measured in BYTES, not characters — a WIT `string` does not
/// itself impose a length limit and "character" is ambiguous (code point
/// vs. grapheme); bytes is the only unambiguous thing and what actually
/// occupies memory.
const MAX_STYLED_SPAN_TEXT_BYTES: usize = 4 * 1024;

/// TOTAL cap on `text` bytes summed across ALL spans of a `render-styled`
/// (ADR 0037 decision table 1): reuses the same cap as `MAX_RETURN_BYTES`
/// (the runtime's return cap, issue #68) — a styled preview must not be
/// able to inflate the host's memory more than any other guest return
/// value. Since 0.9.0 it is measured with [`span_wire_cost`] — text PLUS
/// its fields — not just the text.
const MAX_STYLED_TOTAL_TEXT_BYTES: usize = MAX_RETURN_BYTES;

/// Ceiling on the bytes of a returned thumbnail (ADR 0107): the same as
/// any return value, and the one the wire applies when reading it.
pub const THUMB_MAX_BYTES: usize = MAX_RETURN_BYTES;

/// The largest edge asked of a thumbnail guest, at most: above it, it is
/// not a thumbnail, it is the image.
pub const THUMB_MAX_EDGE: u32 = 2048;

/// Cap on the `.wasm` ARTIFACT's size on disk BEFORE compiling it (issue
/// #68): compiling a component with cranelift costs CPU and memory
/// proportional to its size; that work is not spent on an arbitrarily
/// large artifact. 64 MiB is very generous for a legitimate component (the
/// example guests weigh a few hundred KiB). Above it, it is rejected
/// without ever reaching `Component::from_file`.
pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// Period of the "ticker" thread that increments the engine's epoch.
/// Together with the per-call deadline, it sets the CLOCK cap on a guest
/// operation (≈ deadline × period). Clock-based and not CPU-based: the
/// ticker advances even while the guest is descheduled (#211).
const EPOCH_TICK: Duration = Duration::from_millis(50);

/// Default epoch deadline: ≈ [`EPOCH_TICK`] × 200 ≈ 10 s **PER CALL**
/// (#211). A guest that overruns that in ONE operation TRAPS (hard rule 3:
/// no long operations without a cutoff). Deliberately generous so as not
/// to kill legitimate plugins; tests use
/// [`PluginRuntime::with_epoch_deadline`] with a much smaller value so
/// they don't take long.
///
/// **Per call, not per instance**, ever since [`PluginInstance::rearm`]
/// rearms it on every entry into the guest: armed only once when the
/// store was created, the ten seconds were the budget for the instance's
/// ENTIRE LIFE, so an FTP connection would die ten seconds after being
/// opened.
///
/// And it is ten seconds of CLOCK time, not the guest's CPU: the epochs
/// are advanced by a ticker thread, so a loaded host eats them just the
/// same. With the per-call cutoff, that stops being a suite flake — it
/// was `#211` — and becomes what it says: an operation cannot take more
/// than ten seconds.
///
/// When it expires, the error is [`RuntimeError::Deadline`] and **never**
/// a trap (#211): the difference is what the reader ends up reading, and
/// "this plugin is broken" is the one answer guaranteed to be false when
/// what actually happened is that the machine was under load. Outside
/// this crate it translates to `ProviderUnavailable { retryable: true }`,
/// which is what really happened.
const DEFAULT_EPOCH_DEADLINE: u64 = 200;

/// Plugin runtime failure.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The artifact is not a valid WASM component (or could not be read).
    #[error("invalid WASM component: {0}")]
    Component(String),
    /// The linker or instantiation failed.
    #[error("instantiation failed: {0}")]
    Instantiate(String),
    /// The guest trapped while running an export.
    #[error("guest trap: {0}")]
    Trap(String),
    /// The call's BUDGET ran out: the guest was still running when its
    /// epoch deadline expired (#211).
    ///
    /// Separate from [`Self::Trap`] because it is not the same thing and
    /// the message matters: a trap says "this plugin is broken", and this
    /// says "it did not get enough time". The deadline is measured in
    /// CLOCK time — a ticker advances the epochs — so a loaded machine can
    /// exhaust it with a plugin that is merely slow, and calling that a
    /// broken plugin is the one answer guaranteed to be false.
    #[error("the guest ran out of its call budget")]
    Deadline,
    /// `plugin.wasm` is not the one that was approved: its fingerprint is
    /// not the catalog's (ADR 0142). It is neither compiled nor run.
    #[error("the plugin binary changed since it was approved")]
    DigestMismatch,
    /// The guest returned a readable `Err` from its logic.
    #[error("plugin error: {0}")]
    Guest(String),
    /// The `.wasm` artifact on disk exceeds the `MAX_ARTIFACT_BYTES` cap:
    /// rejected BEFORE compiling it (issue #68).
    #[error("artifact too large: {len} bytes (max {cap})")]
    ArtifactTooLarge {
        /// Actual size of the `.wasm` on disk.
        len: u64,
        /// Allowed cap (`MAX_ARTIFACT_BYTES`).
        cap: u64,
    },
    /// The guest's return value exceeds the `MAX_RETURN_BYTES` cap (issue
    /// #68): rejected fail-loud instead of growing the host's memory.
    #[error("plugin return value too large: {len} bytes (max {cap})")]
    ReturnTooLarge {
        /// Length of the value the guest returned.
        len: usize,
        /// Allowed cap (`MAX_RETURN_BYTES`).
        cap: usize,
    },
    /// The guest's `render-styled` exceeds one of the caps in ADR 0037
    /// decision table 1 (lines / spans per line / bytes per span / total
    /// bytes): rejected AS A WHOLE, fail-closed — never truncated halfway
    /// (the caller falls back to the plain `render` preview).
    #[error("styled preview exceeds a cap: {0}")]
    StyledPreviewTooLarge(String),
    /// The thumbnail the guest returned did not pass the host's
    /// verification (ADR 0107 decision 3): encoding, magic bytes,
    /// dimensions or edge.
    #[error("thumbnail rejected: {0}")]
    ThumbnailRejected(String),
}

/// Applies the size cap to the guest's return value (issue #68).
/// Fail-loud: above `MAX_RETURN_BYTES` it returns
/// [`RuntimeError::ReturnTooLarge`] instead of handing over (or
/// truncating) the string.
fn cap_return_value(value: String) -> Result<String, RuntimeError> {
    if value.len() > MAX_RETURN_BYTES {
        return Err(RuntimeError::ReturnTooLarge {
            len: value.len(),
            cap: MAX_RETURN_BYTES,
        });
    }
    Ok(value)
}

/// Checks that the artifact on disk does not exceed [`MAX_ARTIFACT_BYTES`]
/// (issue #68). Kept separate so the decision can be tested without
/// writing a huge file.
fn check_artifact_size(len: u64) -> Result<(), RuntimeError> {
    if len > MAX_ARTIFACT_BYTES {
        return Err(RuntimeError::ArtifactTooLarge {
            len,
            cap: MAX_ARTIFACT_BYTES,
        });
    }
    Ok(())
}

/// Applies the FOUR `render-styled` caps (ADR 0037 decision table 1) to
/// the result the guest returned, POST-return: line count, spans per
/// line, UTF-8 `text` bytes per span, and total `text` bytes summed
/// across ALL spans. Fail-closed: the first violation found rejects the
/// WHOLE set — never truncated halfway, the caller (norte-core) falls
/// back to the plain preview.
fn cap_styled_text(lines: Vec<Vec<Span>>) -> Result<Vec<Vec<Span>>, RuntimeError> {
    if lines.len() > MAX_STYLED_LINES {
        return Err(RuntimeError::StyledPreviewTooLarge(format!(
            "{} lines (max {MAX_STYLED_LINES})",
            lines.len()
        )));
    }
    let mut total_text_bytes: usize = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.len() > MAX_STYLED_SPANS_PER_LINE {
            return Err(RuntimeError::StyledPreviewTooLarge(format!(
                "line {i}: {} spans (max {MAX_STYLED_SPANS_PER_LINE})",
                line.len()
            )));
        }
        for span in line {
            if span.text.len() > MAX_STYLED_SPAN_TEXT_BYTES {
                return Err(RuntimeError::StyledPreviewTooLarge(format!(
                    "span of {} bytes (max {MAX_STYLED_SPAN_TEXT_BYTES})",
                    span.text.len()
                )));
            }
            total_text_bytes += span_wire_cost(span);
        }
    }
    if total_text_bytes > MAX_STYLED_TOTAL_TEXT_BYTES {
        return Err(RuntimeError::StyledPreviewTooLarge(format!(
            "{total_text_bytes} total bytes estimated on the wire (max {MAX_STYLED_TOTAL_TEXT_BYTES})"
        )));
    }
    Ok(lines)
}

/// What a span costs on the wire, approximated from above: the text plus
/// the fields it carries. Until 0.9.0 the total cap counted only the
/// text, and a ONE-character span with `fg` and `bg` — each cell of an
/// image — weighs forty bytes of JSON for three of text: the byte cap has
/// to measure what actually crosses, or it bounds nothing.
fn span_wire_cost(span: &Span) -> usize {
    const BASE: usize = 12; // `{"text":""},`
    const COLOUR: usize = 16; // `"fg":[255,255,255],`
    const ROLE: usize = 10; // `"role":"",`
    BASE + span.text.len()
        + span.role.as_ref().map_or(0, |r| ROLE + r.len())
        + span.fg.map_or(0, |_| COLOUR)
        + span.bg.map_or(0, |_| COLOUR)
}

/// Aggregate cap over a BATCH of `decorate`/`column-values` (the same
/// `MAX_RETURN_BYTES` as any other runtime return value, issue #68):
/// `len` is the sum of USEFUL bytes in the batch (badges+roles, or column
/// values), not the entry count — a large batch of tiny cells is
/// legitimate, a batch of a few giant cells is not.
fn cap_total_bytes(len: usize) -> Result<(), RuntimeError> {
    if len > MAX_RETURN_BYTES {
        return Err(RuntimeError::ReturnTooLarge {
            len,
            cap: MAX_RETURN_BYTES,
        });
    }
    Ok(())
}

/// The state that lives in wasmtime's `Store<T>`: the (empty) WASI
/// context, the resource table, the declared capabilities, the log
/// buffer, the scoped resources the HOST prepared for the guest, and the
/// `[config]` values (P2 Task 3) the guest can read via `host-config`.
pub struct HostState {
    ctx: WasiCtx,
    table: ResourceTable,
    caps: Capabilities,
    logs: Vec<String>,
    scoped_resources: HashMap<String, Vec<u8>>,
    /// Whoever knows how to resolve a location token (ADR 0057). `None` =
    /// the caller injected none, and then the interface answers with an
    /// error even if the capability is declared: fail-closed on both
    /// axes.
    location: Option<Arc<dyn LocationHost>>,
    /// VALIDATED `[config]` values (P2 decision 3/4): the manifest
    /// schema's defaults with `config.toml` already overlaid —
    /// `norte-plugin-host::resolve_settings` runs BEFORE instantiating
    /// (in the catalog, Task 2). Empty by default ([`Self`] is built
    /// before the caller knows the specific plugin);
    /// [`PluginInstance::set_settings`]/[`ProviderInstance::set_settings`]
    /// fill it in BEFORE invoking any guest export (same pattern as
    /// [`PluginInstance::preload_scoped`]).
    settings: BTreeMap<String, String>,
    /// Store resource limits (linear memory). Referenced by
    /// `Store::limiter` via a `WasiView`-adjacent closure in
    /// `instantiate`.
    limits: StoreLimits,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

impl host_log::Host for HostState {
    fn log(&mut self, message: String) {
        if self.logs.len() < MAX_LOGS {
            // Per-line cap on a char boundary (anti-DoS against a giant
            // line).
            let capped = if message.chars().count() > MAX_LOG_CHARS {
                message.chars().take(MAX_LOG_CHARS).collect()
            } else {
                message
            };
            self.logs.push(capped);
        }
    }

    fn read_scoped(&mut self, token: String) -> Result<Vec<u8>, String> {
        // Host-side ENFORCEMENT (ADR 0022 D4): without the declared
        // capability, not even the token is looked at.
        if !self.caps.fs_read.granted() {
            return Err("fs-read not declared".into());
        }
        match self.scoped_resources.get(&token) {
            Some(bytes) => Ok(bytes.clone()),
            None => Err("unknown token".into()),
        }
    }
}

/// What the consuming HOST (today `norte-core`) knows how to do with a
/// location token.
///
/// The WIT interface never touches the filesystem from this crate, and
/// that is not a matter of taste: `norte-plugin-host` is the sandbox crate
/// and cannot depend on `norte-vfs-local` — that direction would put the
/// filesystem INSIDE the sandbox. Here there is only a trait; whoever
/// implements it is whoever already has the right to read.
pub trait LocationHost: Send + Sync + std::fmt::Debug {
    /// Bytes of a file under the token, or a readable error.
    ///
    /// # Errors
    /// Whatever the implementer sees fit: the string travels to the guest
    /// as-is.
    fn read(&self, token: &str, rel: &[u8]) -> Result<Vec<u8>, String>;

    /// Like [`Self::read`], at most the first `max` bytes: what a header
    /// needs. The implementer reads and pays only for what it returns.
    ///
    /// # Errors
    /// Same as [`Self::read`].
    fn read_prefix(&self, token: &str, rel: &[u8], max: u64) -> Result<Vec<u8>, String>;

    /// Metadata of an entry under the token (without following symlinks).
    ///
    /// # Errors
    /// Same as [`Self::read`].
    fn stat(&self, token: &str, rel: &[u8]) -> Result<location::Meta, String>;

    /// Entries of a directory under the token.
    ///
    /// # Errors
    /// Same as [`Self::read`].
    fn list_dir(&self, token: &str, rel: &[u8]) -> Result<Vec<location::Dirent>, String>;
}

/// `location` (ADR 0057). Each branch gates against the capability BEFORE
/// looking at the token, just like [`HostState::read_scoped`] gates
/// against `fs-read`: without permission, not a single byte is resolved,
/// and the guest does not even learn whether the token was valid.
impl location::Host for HostState {
    fn read(&mut self, token: String, rel: Vec<u8>) -> Result<Vec<u8>, String> {
        let host = self.location_host()?;
        host.read(&token, &rel)
    }

    fn read_prefix(&mut self, token: String, rel: Vec<u8>, max: u64) -> Result<Vec<u8>, String> {
        let host = self.location_host()?;
        host.read_prefix(&token, &rel, max)
    }

    fn stat(&mut self, token: String, rel: Vec<u8>) -> Result<location::Meta, String> {
        let host = self.location_host()?;
        host.stat(&token, &rel)
    }

    fn list_dir(&mut self, token: String, rel: Vec<u8>) -> Result<Vec<location::Dirent>, String> {
        let host = self.location_host()?;
        host.list_dir(&token, &rel)
    }
}

impl HostState {
    /// The location resolver, if the capability is declared AND the
    /// caller injected one. Fail-closed on both sides, and with the same
    /// message: a guest cannot tell "I wasn't approved" from "there is no
    /// location here", and has no reason to.
    fn location_host(&self) -> Result<Arc<dyn LocationHost>, String> {
        if !self.caps.location.granted() {
            return Err("location not declared".into());
        }
        self.location
            .clone()
            .ok_or_else(|| "location not available".into())
    }
}

/// `host-config` (P2 Task 3): PURE reads of the already-resolved
/// `settings` map — no branch touches FS, network or the clock, unlike
/// `read-scoped` (which does gate against a capability). There is nothing
/// to gate here: `settings` is ALWAYS the result of a fail-closed
/// validation done BEFORE reaching this struct (Task 2,
/// `resolve_settings`), so any key present is already safe to hand over
/// as-is — the sandbox invariant (hard rule 9, no direct guest access to
/// the outside world) stays intact.
impl host_config::Host for HostState {
    fn get(&mut self, key: String) -> Option<String> {
        self.settings.get(&key).cloned()
    }

    fn all(&mut self) -> Vec<(String, String)> {
        self.settings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// The host's wasmtime engine, reusable across instantiations.
///
/// Translates a guest call's error.
///
/// An epoch expiry is NOT a guest trap (#211): wasmtime delivers both
/// through the same path, and counting them the same turned "your machine
/// was under load" into "your plugin is broken" — the one reading that is
/// guaranteed to be false.
fn map_call_error(e: &wasmtime::Error) -> RuntimeError {
    if e.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::Interrupt) {
        return RuntimeError::Deadline;
    }
    RuntimeError::Trap(e.to_string())
}

/// Starts a "ticker" thread that increments the engine's epoch every
/// [`EPOCH_TICK`] (50 ms); combined with the per-store deadline
/// ([`Store::set_epoch_deadline`]) it puts a CLOCK cap on every guest call
/// (hard rule 3). The thread stops cleanly in [`Drop`].
pub struct PluginRuntime {
    engine: Engine,
    /// Epoch ticks a store can consume before trapping.
    epoch_deadline: u64,
    /// Stop signal for the ticker thread.
    ticker_stop: Arc<AtomicBool>,
    /// Ticker thread handle; `take()`n in `Drop` to join it.
    ticker: Option<JoinHandle<()>>,
    /// The already-COMPILED components, by the sha256 of their bytes (ADR
    /// 0141).
    ///
    /// Compiling a plugin with cranelift costs seconds (the syntax
    /// highlighting one, 2-3 s), and it used to happen on EVERY call: each
    /// F3 paid for it. A `Component` is immutable and cheap to clone, and
    /// every call still gets its own NEW `Store` and instance — what gets
    /// reused is the machine code, not the state.
    ///
    /// By CONTENT and not by path or date: it is compiled from the same
    /// bytes that were digested, so a changed file is a different entry
    /// and old code never runs for a new one, nor the other way around.
    compiled: std::sync::Mutex<Compiled>,
}

/// The sha256 of some bytes: the cache key and, in hex, the catalog's
/// fingerprint ([`crate::wasm_digest_of`]).
fn digest_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// A `plugin.wasm` together with the FINGERPRINT a human approved (#241,
/// ADR 0142): the only thing the runtime accepts to instantiate from
/// disk.
///
/// The runtime reads the file, digests it and rejects it with
/// [`RuntimeError::DigestMismatch`] if it is not the approved one, over
/// the SAME bytes it compiles. Before, it was only compared at discovery
/// time, and the `.wasm` could change afterward with nobody looking again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmArtifact {
    path: std::path::PathBuf,
    digest: String,
}

impl WasmArtifact {
    /// The path and the approved fingerprint (sha256 in lowercase hex,
    /// the one from [`crate::PluginEntry::wasm_digest`]).
    ///
    /// The type CARRIES the fingerprint; it does not certify it. The
    /// fingerprint has to come from the catalog, after checking the
    /// approval is still valid — one computed now over the file protects
    /// nothing.
    #[must_use]
    pub fn approved(path: std::path::PathBuf, digest: String) -> Self {
        Self { path, digest }
    }

    /// The fingerprint of what is NOW at `path`.
    ///
    /// This is trusting the file: only for whoever is the authority over
    /// that file — the tests, which just compiled it — never for a
    /// third party's plugin, which goes through [`Self::approved`] and
    /// the catalog's fingerprint.
    ///
    /// # Errors
    /// If it cannot be read.
    pub fn trusting_current(path: impl Into<std::path::PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let bytes = std::fs::read(&path)?;
        Ok(Self {
            digest: crate::wasm_digest_of(&bytes),
            path,
        })
    }

    /// The path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The approved fingerprint.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// To READ or copy the file (tests, diagnostics): the path. Instantiating
/// asks for the whole artifact, fingerprint included.
impl AsRef<Path> for WasmArtifact {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// What the compiled-components cache holds (ADR 0141).
#[derive(Default)]
struct Compiled {
    /// The code, by the sha256 of the bytes it came from.
    by_digest: HashMap<[u8; 32], Component>,
    /// Which digest each PATH currently has: recompiling a changed plugin
    /// drops its previous version, which nobody is going to ask for
    /// anymore. Without this, each development iteration would leave a
    /// dead entry until the cache was cleared.
    by_path: HashMap<std::path::PathBuf, [u8; 32]>,
}

/// How many compiled components a runtime keeps. Few: each one takes up
/// as much as its machine code, and the plugins in use at the same time
/// are a handful. When it overflows, it is cleared entirely, which is the
/// simplest thing that doesn't grow.
const COMPILED_MAX: usize = 16;

impl PluginRuntime {
    /// Builds the engine with the Component Model enabled and the
    /// production epoch deadline (`DEFAULT_EPOCH_DEADLINE`, ≈ 10 s of
    /// CPU).
    ///
    /// # Errors
    /// Fails if the wasmtime engine's configuration is invalid on this
    /// platform.
    pub fn new() -> Result<Self, RuntimeError> {
        Self::with_epoch_deadline(DEFAULT_EPOCH_DEADLINE)
    }

    /// Like [`PluginRuntime::new`] but with an explicit epoch deadline (in
    /// `EPOCH_TICK` ticks, 50 ms). Meant for tests that need a short
    /// timeout (e.g. verifying that a looping guest traps without hanging
    /// the host) without waiting the ~10 s of the production default.
    ///
    /// # Errors
    /// Fails if the wasmtime engine's configuration is invalid on this
    /// platform.
    pub fn with_epoch_deadline(epoch_deadline: u64) -> Result<Self, RuntimeError> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        // Epoch-based interruption: the engine checks the deadline at the
        // guest's loop/function boundaries and traps once it is exceeded
        // (hard rule 3).
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        // Ticker thread: increments the epoch every EPOCH_TICK until Drop
        // stops it. `Engine` is Clone (an Arc inside), so the thread
        // shares the same engine.
        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker = {
            let engine = engine.clone();
            let stop = Arc::clone(&ticker_stop);
            std::thread::Builder::new()
                .name("norte-plugin-epoch".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(EPOCH_TICK);
                        engine.increment_epoch();
                    }
                })
                .map_err(|e| RuntimeError::Instantiate(e.to_string()))?
        };

        Ok(Self {
            engine,
            epoch_deadline,
            ticker_stop,
            ticker: Some(ticker),
            compiled: std::sync::Mutex::new(Compiled::default()),
        })
    }

    /// How many compiled components this runtime holds. For the cache
    /// tests (ADR 0141); not API.
    #[doc(hidden)]
    #[must_use]
    pub fn compiled_components(&self) -> usize {
        self.compiled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_digest
            .len()
    }

    /// The component for these bytes, compiled only once per runtime
    /// ([`Self::compiled`]). `path` is where they came from, if they came
    /// from disk: used to drop the previous version of the same plugin.
    ///
    /// Two simultaneous calls with the same not-yet-compiled plugin both
    /// compile it; whichever finishes second overwrites the first with
    /// the same thing. It is CPU spent once per process and plugin, not a
    /// bug.
    fn compiled_component(
        &self,
        bytes: &[u8],
        path: Option<&Path>,
    ) -> Result<Component, RuntimeError> {
        self.compiled_component_with(digest_bytes(bytes), bytes, path)
    }

    /// Like [`Self::compiled_component`], with the digest already
    /// computed: whoever just checked the fingerprint does not compute it
    /// twice.
    fn compiled_component_with(
        &self,
        key: [u8; 32],
        bytes: &[u8],
        path: Option<&Path>,
    ) -> Result<Component, RuntimeError> {
        // A poisoned mutex is a panic from another call while it was
        // inserting an entry; the maps stay coherent (each insertion is
        // atomic from the outside), so they keep being used.
        if let Some(c) = self
            .compiled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_digest
            .get(&key)
        {
            return Ok(c.clone());
        }
        // Compile OUTSIDE the lock: it takes seconds, and another call
        // with another plugin has no reason to wait for them.
        let component = Component::from_binary(&self.engine, bytes)
            .map_err(|e| RuntimeError::Component(e.to_string()))?;
        let mut cache = self
            .compiled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(p) = path
            && let Some(previous) = cache.by_path.insert(p.to_path_buf(), key)
            && previous != key
        {
            cache.by_digest.remove(&previous);
        }
        if cache.by_digest.len() >= COMPILED_MAX {
            cache.by_digest.clear();
            cache.by_path.retain(|_, c| *c == key);
        }
        cache.by_digest.insert(key, component.clone());
        Ok(component)
    }

    /// Instantiates a plugin from a WASM component on disk, with the
    /// declared `caps` and an EMPTY WASI sandbox.
    ///
    /// # Errors
    /// - [`RuntimeError::ArtifactTooLarge`] if the `.wasm` on disk exceeds
    ///   `MAX_ARTIFACT_BYTES` (rejected before compiling).
    /// - [`RuntimeError::Component`] if the artifact is not a valid
    ///   component.
    /// - [`RuntimeError::Instantiate`] if the linker or instantiation
    ///   fail.
    pub fn instantiate(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
    ) -> Result<PluginInstance, RuntimeError> {
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        let bindings = NortePlugin::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(PluginInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates a PROVIDER guest (world `norte-provider`, #30 stage 2)
    /// with the SAME sandbox and limits as [`Self::instantiate`]. Returns
    /// a [`ProviderInstance`] to call its exports (`capabilities`/`stat`/
    /// `list-dir`/`read`).
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_provider(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
    ) -> Result<ProviderInstance, RuntimeError> {
        use crate::bindings::provider_world::NorteProvider;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        let bindings = NorteProvider::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ProviderInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates a DECORATOR guest (world `norte-decorator`, ADR 0037
    /// decision 2) with the SAME sandbox and limits as
    /// [`Self::instantiate`]. Returns a [`DecoratorInstance`] to call
    /// `decorate`.
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_decorator(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
    ) -> Result<DecoratorInstance, RuntimeError> {
        use crate::bindings::decorator_world::NorteDecorator;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        let bindings = NorteDecorator::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(DecoratorInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates a PANEL guest (world `norte-panel`, phase 3 of the
    /// 2026-09-15 program) with the SAME sandbox and limits as
    /// [`Self::instantiate`].
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_panel(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
        location: Option<Arc<dyn LocationHost>>,
    ) -> Result<PanelInstance, RuntimeError> {
        use crate::bindings::panel_world::NortePanel;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        // Location is plugged in BEFORE instantiating, as with columns and
        // renamers. Without this line the world imports `norte:location`
        // and every guest call answers "not available": a linked, dead
        // capability, and a git panel that cannot read `.git/HEAD`.
        store.data_mut().location = location;
        let bindings = NortePanel::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(PanelInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates a THUMBNAIL guest (world `norte-thumbnail`, ADR 0107)
    /// with the SAME sandbox and limits as [`Self::instantiate`]. Returns
    /// a [`ThumbnailInstance`] to call `render`.
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_thumbnail(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
    ) -> Result<ThumbnailInstance, RuntimeError> {
        use crate::bindings::thumbnail_world::NorteThumbnail;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        let bindings = NorteThumbnail::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ThumbnailInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates a COLUMNS guest (world `norte-columns`, ADR 0037
    /// decision 2) with the SAME sandbox and limits as
    /// [`Self::instantiate`]. Returns a [`ColumnsInstance`] to call
    /// `column-values`.
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_columns(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
    ) -> Result<ColumnsInstance, RuntimeError> {
        self.instantiate_columns_with_location(wasm, caps, None)
    }

    /// Like [`Self::instantiate_columns`], injecting who resolves
    /// location tokens (ADR 0057). `None` = nobody: the `location`
    /// interface stays linked and keeps answering with an error, which is
    /// what it should do.
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_columns_with_location(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
        location: Option<Arc<dyn LocationHost>>,
    ) -> Result<ColumnsInstance, RuntimeError> {
        use crate::bindings::columns_world::NorteColumns;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        store.data_mut().location = location;
        let bindings = NorteColumns::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ColumnsInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates a RENAMER guest (world `norte-renamer`, ADR 0095)
    /// with the SAME sandbox and limits as
    /// [`Self::instantiate_columns_with_location`], and the same location
    /// resolver. Returns a [`RenamerInstance`].
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_renamer_with_location(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
        location: Option<Arc<dyn LocationHost>>,
    ) -> Result<RenamerInstance, RuntimeError> {
        use crate::bindings::renamer_world::NorteRenamer;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        store.data_mut().location = location;
        let bindings = NorteRenamer::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(RenamerInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates an ORGANIZER guest (world `norte-organizer`, phase 8)
    /// with the SAME sandbox, limits and location resolver as
    /// [`Self::instantiate_renamer_with_location`].
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_organizer_with_location(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
        location: Option<Arc<dyn LocationHost>>,
    ) -> Result<OrganizerInstance, RuntimeError> {
        use crate::bindings::organizer_world::NorteOrganizer;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        store.data_mut().location = location;
        let bindings = NorteOrganizer::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(OrganizerInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Instantiates a HOOK guest (world `norte-hook`, ADR 0100) with the
    /// SAME sandbox and limits as
    /// [`Self::instantiate_renamer_with_location`], and the same location
    /// resolver. Returns a [`HookInstance`].
    ///
    /// # Errors
    /// Same as [`Self::instantiate`].
    pub fn instantiate_hook_with_location(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
        location: Option<Arc<dyn LocationHost>>,
    ) -> Result<HookInstance, RuntimeError> {
        use crate::bindings::hook_world::NorteHook;
        let (mut store, component, linker) = self.prepare(wasm, caps)?;
        store.data_mut().location = location;
        let bindings = NorteHook::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(HookInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Like [`Self::instantiate_provider`] but from the BYTES of a
    /// component in memory (ADR 0033: the FTP guest ships EMBEDDED in
    /// norte's binary, since the `wasm32-wasip2` target may be missing on
    /// the build host). Applies the SAME sandbox and limits as the
    /// disk-based path, including the artifact size cap.
    ///
    /// # Errors
    /// [`RuntimeError::ArtifactTooLarge`] if the bytes exceed the cap;
    /// [`RuntimeError::Component`] if they are not a valid component;
    /// [`RuntimeError::Instantiate`] if the linker or instantiation fail.
    pub fn instantiate_provider_bytes(
        &self,
        bytes: &[u8],
        caps: Capabilities,
    ) -> Result<ProviderInstance, RuntimeError> {
        use crate::bindings::provider_world::NorteProvider;
        // The embedded artifact is first-party, but the cap costs nothing
        // and protects a future caller who passes third-party bytes (rust
        // review m3).
        check_artifact_size(bytes.len() as u64)?;
        let component = self.compiled_component(bytes, None)?;
        let (mut store, linker) = self.prepare_common(caps)?;
        let bindings = NorteProvider::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ProviderInstance {
            store,
            bindings,
            epoch_deadline: self.epoch_deadline,
        })
    }

    /// Prepares the `Store` (empty WASI sandbox + limits + deadline) and
    /// the `Linker` (WASI + `host-log`) common to any world, and fetches
    /// the component from the file's bytes, compiled or from the cache
    /// (ADR 0141). The caller instantiates the specific world.
    fn prepare(
        &self,
        wasm: &WasmArtifact,
        caps: Capabilities,
    ) -> Result<(Store<HostState>, Component, Linker<HostState>), RuntimeError> {
        // Artifact cap BEFORE reading (issue #68): a giant `.wasm` must
        // not spend even the read. `metadata` does not read the content;
        // the read below checks it again over what was actually read.
        let len = std::fs::metadata(wasm.path())
            .map_err(|e| RuntimeError::Component(e.to_string()))?
            .len();
        check_artifact_size(len)?;

        // The BYTES and not `from_file`: they are digested for the cache
        // and compiled from those same bytes (ADR 0141). Reading a
        // couple-megabyte `.wasm` costs milliseconds; compiling it,
        // seconds.
        // Capped at the limit plus one: the file could have grown between
        // `metadata` and the read, and reading it whole would be the
        // memory the cap exists to avoid spending. With one extra, the
        // cap catches it.
        let bytes = {
            use std::io::Read;
            let f = std::fs::File::open(wasm.path())
                .map_err(|e| RuntimeError::Component(e.to_string()))?;
            let mut v = Vec::new();
            f.take(MAX_ARTIFACT_BYTES.saturating_add(1))
                .read_to_end(&mut v)
                .map_err(|e| RuntimeError::Component(e.to_string()))?;
            v
        };
        check_artifact_size(bytes.len() as u64)?;
        // The bytes about to be compiled are the ones the human APPROVED,
        // or nothing gets compiled (ADR 0142). Checked HERE, over what was
        // read and before the cache, and not at discovery time: between
        // discovery and load, whoever could write `plugin.wasm` would run
        // their code with the capabilities granted to someone else.
        let key = digest_bytes(&bytes);
        if crate::capability::hex_lower(&key) != wasm.digest() {
            return Err(RuntimeError::DigestMismatch);
        }
        let component = self.compiled_component_with(key, &bytes, Some(wasm.path()))?;
        let (store, linker) = self.prepare_common(caps)?;
        Ok((store, component, linker))
    }

    /// The `Store` (empty WASI sandbox, limits and deadline) and the
    /// `Linker` (WASI plus `host-log`) common to any world, WITHOUT
    /// loading the component: the caller brings its own `Component` (from
    /// disk via [`Self::prepare`], or from embedded bytes via
    /// [`Self::instantiate_provider_bytes`], ADR 0033).
    fn prepare_common(
        &self,
        caps: Capabilities,
    ) -> Result<(Store<HostState>, Linker<HostState>), RuntimeError> {
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        // FULL WASI linker on purpose (issue #68, point 3 — evaluated and
        // DISCARDED trimming it): guests are compiled to `wasm32-wasip2`
        // with Rust's std, which imports the standard surface (wasi:cli,
        // wasi:io, wasi:clocks, wasi:random, wasi:filesystem…) for its
        // runtime (panic, allocation, formatting). Trimming the linker
        // would fail instantiation of legitimate guests with "unsatisfied
        // import", without gaining security: REAL isolation is not the
        // absence of imports in the linker but the EMPTY `WasiCtx`
        // below — with no preopens, stdio, network or env, those
        // interfaces exist but grant NO capability. The stateful gate
        // (scoped `fs-read`) is still gated by the HOST.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        host_log::add_to_linker::<HostState, wasmtime::component::HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        // `host-config` (P2 Task 3): ALWAYS linked, for ANY plugin, just
        // like `host-log` — a 0.4.0 guest that does not import it simply
        // never resolves it while instantiating (the `Linker` can offer
        // MORE functions than a specific world requires; only an
        // UNRESOLVED import breaks instantiation, never an extra one).
        host_config::add_to_linker::<HostState, wasmtime::component::HasSelf<_>>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        // `location` (ADR 0057): also ALWAYS in the linker, for the same
        // reason as the two above — an extra import never breaks
        // anything, an unresolved one does. What decides whether it
        // serves anything is the capability, inside.
        location::add_to_linker::<HostState, wasmtime::component::HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        // WASI sandbox: no inherited stdio, no preopens, no env. NETWORK
        // is granted ONLY if the `net` capability is declared, and even
        // then RESTRICTED to the allow-list hosts (#30 stage 3a). Without
        // `net`, the default `socket_addr_check` REJECTS every address
        // (fail-closed) — the guest exists with `wasi:sockets` linked but
        // with no connection granted (same principle as `fs-read`: the
        // full linker, the host grants the capability). This line is the
        // heart of network isolation (review it, security).
        let mut ctx_builder = WasiCtxBuilder::new();
        if let Some(net) = &caps.net {
            // Allow-list resolved by HOST. ONLY OUTBOUND TCP connections
            // (`TcpConnect`): bind/listen and ALL UDP are rejected — a
            // network provider connects, it does not listen or send
            // datagrams (least privilege, security review). An
            // `ip:port` entry fixes the port; a plain `ip` one
            // authorizes ANY port on that host — this is deliberate
            // (passive FTP negotiates DYNAMIC data ports, not boundable
            // ahead of time) and the human sees it when approving the
            // manifest. No DNS in the guest
            // (`allow_ip_name_lookup(false)`): it connects by IP and the
            // allow-list is by IP; resolving hostnames + a
            // link-local/metadata (169.254/fe80) deny-list belongs to
            // stage 3b's wiring.
            let allowed: std::collections::HashSet<String> = net.hosts.iter().cloned().collect();
            ctx_builder.socket_addr_check(move |addr, use_| {
                let permitted = matches!(use_, wasmtime_wasi::sockets::SocketAddrUse::TcpConnect)
                    && (allowed.contains(&addr.ip().to_string())
                        || allowed.contains(&addr.to_string()));
                Box::pin(async move { permitted })
            });
            ctx_builder.allow_ip_name_lookup(false);
            ctx_builder.allow_udp(false);
        }
        let ctx = ctx_builder.build();
        // Per-store linear memory limit (closes M4-P2b): a guest cannot
        // exhaust the host's RAM. `StoreLimits` implements
        // `ResourceLimiter`.
        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_STORE_MEMORY_BYTES)
            .build();
        let state = HostState {
            ctx,
            table: ResourceTable::new(),
            caps,
            logs: Vec::new(),
            scoped_resources: HashMap::new(),
            location: None,
            // Empty until the caller knows the specific plugin and calls
            // `set_settings` (same pattern as
            // `scoped_resources`/`preload_scoped`, P2 Task 3) —
            // equivalent to a manifest without `[config]`, the correct
            // behavior for any caller not yet extended to hand over
            // settings.
            settings: BTreeMap::new(),
            limits,
        };

        let mut store = Store::new(&self.engine, state);
        // Memory-limit enforcement: the limiter points at `limits`.
        store.limiter(|s: &mut HostState| &mut s.limits);
        // CPU timeout: the guest traps if it consumes more than
        // `epoch_deadline` epoch ticks (the ticker thread advances them).
        // The trap is mapped to `RuntimeError::Trap` in
        // run_command/render_preview.
        store.set_epoch_deadline(self.epoch_deadline);

        Ok((store, linker))
    }
}

impl Drop for PluginRuntime {
    /// Stops the ticker thread cleanly: signals the stop and joins it so
    /// as not to leave orphan threads (nor a "thread leak" in tests) when
    /// the runtime dies.
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.ticker.take() {
            let _ = handle.join();
        }
    }
}

/// A live plugin instance: its `Store` (host state) and the world's
/// bindings to call its exports.
pub struct PluginInstance {
    store: Store<HostState>,
    /// The epoch ticks given to EACH guest call.
    ///
    /// Per call, not per instance: see [`PluginInstance::rearm`].
    epoch_deadline: u64,
    bindings: NortePlugin,
}

impl std::fmt::Debug for PluginInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginInstance")
            .field("logs", &self.store.data().logs.len())
            .finish_non_exhaustive()
    }
}

impl PluginInstance {
    /// Rearms the epoch budget BEFORE every guest call (#211).
    ///
    /// `Store::set_epoch_deadline` sets an ABSOLUTE instant (the current
    /// epoch plus N), not a budget that renews itself. Armed once when
    /// the store was created — as it used to be — what the plugin gets is
    /// not a per-operation budget but a per-LIFE one: with the default of
    /// 10 seconds, an FTP connection would stop working ten seconds after
    /// being opened, and every subsequent call would trap. In tests that
    /// showed up as a `contract_hostile_names_roundtrip` that went red
    /// only under load, which is how it was discovered; in production it
    /// is a session dying while you use it.
    ///
    /// And epochs advance by CLOCK time, not the guest's CPU (the ticker
    /// thread increments them every `EPOCH_TICK`), so the per-call budget
    /// is clock-based too: a loaded host can still cut off a guest that
    /// is merely slow. That is what remains alive from #211 — with the
    /// per-call cutoff, the margin is that of ONE operation and not of an
    /// entire session, which is the difference between a generous cap and
    /// a useless one.
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// The log lines the plugin has accumulated via `host-log::log`.
    #[must_use]
    pub fn logs(&self) -> &[String] {
        &self.store.data().logs
    }

    /// Prepares a scoped resource the guest will be able to read with
    /// `read-scoped` using `token` (only if it declared `fs-read`).
    pub fn preload_scoped(&mut self, token: &str, bytes: Vec<u8>) {
        self.store
            .data_mut()
            .scoped_resources
            .insert(token.to_owned(), bytes);
    }

    /// Installs the VALIDATED `[config]` values (P2 Task 3) the guest
    /// will see via `host-config::get`/`all`. Must be called BEFORE
    /// invoking any export that might read them (same pattern as
    /// [`Self::preload_scoped`]). A guest compiled against an earlier
    /// package that does not import `host-config` simply never calls
    /// these functions — installing the map does not change its behavior
    /// nor require the caller to know whether the guest uses them.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Invokes the guest's `previewer::render` export.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps.
    /// - [`RuntimeError::Guest`] if the guest returns a logic `Err`.
    /// - [`RuntimeError::ReturnTooLarge`] if the returned text exceeds
    ///   `MAX_RETURN_BYTES`.
    pub fn render_preview(
        &mut self,
        mimetype: &str,
        content: &[u8],
    ) -> Result<String, RuntimeError> {
        self.rearm();
        let input = PreviewInput {
            mimetype: mimetype.to_owned(),
            content: content.to_vec(),
            // The PLAIN render has no viewer to measure: no width hint.
            columns: None,
        };
        let out = self
            .bindings
            .norte_plugin_previewer()
            .call_render(&mut self.store, &input)
            .map_err(|e| map_call_error(&e))?
            .map_err(RuntimeError::Guest)?;
        cap_return_value(out)
    }

    /// Invokes the guest's `previewer::render-styled` export (ADR 0037
    /// decision 2): the styled twin of [`Self::render_preview`]. Decision
    /// table 1's caps are applied POST-return, BEFORE returning to the
    /// caller (`norte-core`, which also validates `role` against
    /// `norte_theme::Role` — this crate does not know that set, decision
    /// 1).
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps.
    /// - [`RuntimeError::Guest`] if the guest returns a logic `Err`.
    /// - [`RuntimeError::StyledPreviewTooLarge`] if the result exceeds
    ///   any of the lines/spans/bytes-per-span/total-bytes caps.
    pub fn render_styled_preview(
        &mut self,
        mimetype: &str,
        content: &[u8],
        columns: Option<u32>,
    ) -> Result<Vec<Vec<Span>>, RuntimeError> {
        self.rearm();
        let input = PreviewInput {
            mimetype: mimetype.to_owned(),
            content: content.to_vec(),
            columns,
        };
        let out = self
            .bindings
            .norte_plugin_previewer()
            .call_render_styled(&mut self.store, &input)
            .map_err(|e| map_call_error(&e))?
            .map_err(RuntimeError::Guest)?;
        cap_styled_text(out)
    }

    /// Invokes the guest's `command::run` export.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps.
    /// - [`RuntimeError::Guest`] if the guest returns a logic `Err`.
    /// - [`RuntimeError::ReturnTooLarge`] if the returned text exceeds
    ///   `MAX_RETURN_BYTES`.
    pub fn run_command(&mut self, id: &str, arg: &str) -> Result<String, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_plugin_command()
            .call_run(&mut self.store, id, arg)
            .map_err(|e| map_call_error(&e))?
            .map_err(RuntimeError::Guest)?;
        cap_return_value(out)
    }
}

/// Types of the `provider` export (generated records/enums: `Entry`,
/// `Page`, `Caps`, `VfsError`, `EntryKind`) — re-exported so the host
/// adapter can use them without digging into the generated bindings
/// module (#30 stage 2).
pub use crate::bindings::provider_world::exports::norte::provider::provider as provider_iface;

/// A live instance of a PROVIDER guest (#30 stage 2, world
/// `norte-provider`): its `Store` (host state + sandbox) and the bindings
/// to call the `provider` interface's exports. Each method is ONE
/// synchronous call to the guest; the host adapter (`Provider`)
/// reassembles the streams by calling in a loop (paginated list, ranged
/// read).
pub struct ProviderInstance {
    store: Store<HostState>,
    bindings: crate::bindings::provider_world::NorteProvider,
    /// Epoch ticks for EACH guest call. See
    /// [`PluginInstance::rearm`], which explains why it is per call.
    epoch_deadline: u64,
}

impl std::fmt::Debug for ProviderInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderInstance").finish_non_exhaustive()
    }
}

impl ProviderInstance {
    /// Rearms the epoch budget before every call (#211). See
    /// [`PluginInstance::rearm`]: a per-plugin provider is exactly the
    /// case where a per-instance-life budget shows, because its store
    /// lasts as long as the connection does.
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// Installs the VALIDATED `[config]` values (P2 Task 3) the PROVIDER
    /// guest will see via `host-config::get`/`all`. Same contract as
    /// [`PluginInstance::set_settings`]: call BEFORE invoking any export.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// The capabilities the guest declares (stage 2: only `read-only`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn capabilities(&mut self) -> Result<provider_iface::Caps, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_capabilities(&mut self.store)
            .map_err(|e| map_call_error(&e))
    }

    /// `stat` of an entry by its segments. The inner `Ok` is the guest's
    /// LOGICAL result (`Entry` or `VfsError`); the outer `Err` is a trap.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn stat(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<provider_iface::Entry, provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_stat(&mut self.store, segments)
            .map_err(|e| map_call_error(&e))
    }

    /// One PAGE of a directory listing (equiv. one leg of the
    /// `EntryStream`). `cursor` = `None` starts; the page's `next-cursor`
    /// feeds the next call.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn list_dir(
        &mut self,
        segments: &[Vec<u8>],
        cursor: Option<&[u8]>,
    ) -> Result<Result<provider_iface::Page, provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_list_dir(&mut self.store, segments, cursor)
            .map_err(|e| map_call_error(&e))
    }

    /// A bounded RANGE of a file (equiv. a chunk of the `ByteStream`): at
    /// most `len` bytes from `offset`. A returned value larger than
    /// `MAX_RETURN_BYTES` is REJECTED fail-loud
    /// ([`RuntimeError::ReturnTooLarge`]) — not an allocation guard (the
    /// value has already materialized in the host's memory coming down
    /// from the guest; the real transient bound is the store's 64 MiB
    /// limit), but an honest rejection. Stage-2b debt: `list_dir`/`stat`
    /// do NOT yet bound entry count / name length — the `Provider` host
    /// adapter will, when reassembling.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps;
    /// [`RuntimeError::ReturnTooLarge`] if the guest returns more than
    /// `MAX_RETURN_BYTES`.
    pub fn read(
        &mut self,
        segments: &[Vec<u8>],
        offset: u64,
        len: u64,
    ) -> Result<Result<Vec<u8>, provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_provider_provider()
            .call_read(&mut self.store, segments, offset, len)
            .map_err(|e| map_call_error(&e))?;
        if let Ok(bytes) = &out
            && bytes.len() > MAX_RETURN_BYTES
        {
            return Err(RuntimeError::ReturnTooLarge {
                len: bytes.len(),
                cap: MAX_RETURN_BYTES,
            });
        }
        Ok(out)
    }

    /// Configures the guest-provider's connection (#30 stage 3c):
    /// endpoint ALREADY resolved by the host, credentials and base. The
    /// inner `Ok` is the guest's logical result; the outer `Err` is a
    /// trap. A connectionless provider (mem) implements it as a no-op.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn configure(
        &mut self,
        cfg: &provider_iface::ProviderConfig,
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_configure(&mut self.store, cfg)
            .map_err(|e| map_call_error(&e))
    }

    // ---- write (#30 stage 2b-write) ----

    /// Opens a transactional `writer` over `segments` (equiv.
    /// `Provider::write`). Returns the guest resource's handle; the
    /// caller MUST release it with [`Self::writer_drop`] after
    /// `commit`/`abort`.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn open_writer(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<ResourceAny, provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_open_writer(&mut self.store, segments)
            .map_err(|e| map_call_error(&e))
    }

    /// Appends a chunk to the `writer`'s staging.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn writer_write(
        &mut self,
        writer: ResourceAny,
        chunk: &[u8],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .writer()
            .call_write(&mut self.store, writer, chunk)
            .map_err(|e| map_call_error(&e))
    }

    /// Publishes the `writer`'s staging to the final path.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn writer_commit(
        &mut self,
        writer: ResourceAny,
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .writer()
            .call_commit(&mut self.store, writer)
            .map_err(|e| map_call_error(&e))
    }

    /// Discards the `writer`'s staging without publishing.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn writer_abort(
        &mut self,
        writer: ResourceAny,
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .writer()
            .call_abort(&mut self.store, writer)
            .map_err(|e| map_call_error(&e))
    }

    /// Releases the `writer` handle (drops the guest resource). ALWAYS
    /// called after `commit`/`abort`.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest's drop traps.
    pub fn writer_drop(&mut self, writer: ResourceAny) -> Result<(), RuntimeError> {
        self.rearm();
        writer
            .resource_drop(&mut self.store)
            .map_err(|e| map_call_error(&e))
    }

    /// Creates a directory (equiv. `Provider::mkdir`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn make_dir(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_make_dir(&mut self.store, segments)
            .map_err(|e| map_call_error(&e))
    }

    /// Deletes an entry (equiv. `Provider::remove`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn remove(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_remove(&mut self.store, segments)
            .map_err(|e| map_call_error(&e))
    }

    /// Renames/moves (equiv. `Provider::rename`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] if the guest traps.
    pub fn rename(
        &mut self,
        src: &[Vec<u8>],
        dst: &[Vec<u8>],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.rearm();
        self.bindings
            .norte_provider_provider()
            .call_rename(&mut self.store, src, dst)
            .map_err(|e| map_call_error(&e))
    }
}

/// Types of the `decorator` export (record `Decoration`) — re-exported
/// like [`provider_iface`], so the host adapter can use them without
/// digging into the generated bindings module (ADR 0037 decision 2).
pub use crate::bindings::decorator_world::exports::norte::plugin::decorator as decorator_iface;
pub use crate::bindings::thumbnail_world::exports::norte::thumbnail::thumbnail as thumbnail_iface;

/// The `location` interface's types (ADR 0057): `Meta`, `Dirent` and
/// `EntryKind` as they cross the ABI. Re-exported so whoever implements
/// [`LocationHost`] does not have to name the generated module.
pub use crate::bindings::columns_world::norte::location::location as location_iface;

/// The types the `columns` interface puts on the wire toward the guest —
/// today [`columns_iface::LocationRef`], ADR 0057's (token, prefix) pair.
pub use crate::bindings::columns_world::exports::norte::plugin::columns as columns_iface;
use crate::bindings::columns_world::norte::location::location;

/// The types the `renamer` interface (package `norte:renamer`, ADR 0095)
/// puts on the wire: [`renamer_iface::LocationRef`] and
/// [`renamer_iface::Proposal`].
pub use crate::bindings::renamer_world::exports::norte::renamer::renamer as renamer_iface;

/// Cap on the pairs a renamer can return in one call: the same number the
/// AI plan allows (`MAX_AI_PLAN_ENTRIES`), because it is the same plan
/// from a different producer; above it, the whole thing is rejected,
/// fail-closed.
pub const MAX_RENAME_PROPOSALS: usize = 10_000;

/// An instantiated `renamer` guest (world `norte-renamer`).
pub struct RenamerInstance {
    store: Store<HostState>,
    bindings: crate::bindings::renamer_world::NorteRenamer,
    /// Epoch ticks for EACH call. See [`PluginInstance::rearm`].
    epoch_deadline: u64,
}

impl std::fmt::Debug for RenamerInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenamerInstance").finish_non_exhaustive()
    }
}

impl RenamerInstance {
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// The VALIDATED `[config]` values the guest will see. Call BEFORE
    /// `plan`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Asks the guest for pairs for `names`. `Ok(Err(sentence))` is the
    /// guest declining with a sentence for the reader; the caps are
    /// applied POST-return and reject the whole thing.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps.
    /// - [`RuntimeError::ReturnTooLarge`] if it returns more pairs than
    ///   [`MAX_RENAME_PROPOSALS`] or more bytes than the return cap.
    pub fn plan(
        &mut self,
        id: &str,
        location: Option<&renamer_iface::LocationRef>,
        names: &[String],
    ) -> Result<Result<Vec<renamer_iface::Proposal>, String>, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_renamer_renamer()
            .call_plan(&mut self.store, id, location, names)
            .map_err(|e| map_call_error(&e))?;
        let Ok(pairs) = out else {
            return Ok(out);
        };
        if pairs.len() > MAX_RENAME_PROPOSALS {
            return Err(RuntimeError::ReturnTooLarge {
                len: pairs.len(),
                cap: MAX_RENAME_PROPOSALS,
            });
        }
        let total: usize = pairs
            .iter()
            .map(|p| p.current.len() + p.proposed.len())
            .sum();
        cap_total_bytes(total)?;
        Ok(Ok(pairs))
    }
}

/// The types the `organizer` interface (package `norte:organizer`, phase
/// 8) puts on the wire: [`organizer_iface::LocationRef`] and
/// [`organizer_iface::Proposal`].
pub use crate::bindings::organizer_world::exports::norte::organizer::organizer as organizer_iface;

/// An instantiated ORGANIZER guest (world `norte-organizer`, phase 8).
pub struct OrganizerInstance {
    store: Store<HostState>,
    bindings: crate::bindings::organizer_world::NorteOrganizer,
    /// Epoch ticks for EACH call. See [`PluginInstance::rearm`].
    epoch_deadline: u64,
}

impl std::fmt::Debug for OrganizerInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrganizerInstance").finish_non_exhaustive()
    }
}

impl OrganizerInstance {
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// The VALIDATED `[config]` values the guest will see. Call BEFORE
    /// `plan`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Asks the guest where to move `names`. `Ok(Err(sentence))` is the
    /// guest declining with a sentence for the reader; the caps are
    /// applied POST-return and reject the whole thing, as in the renamer.
    ///
    /// What this method does NOT do is validate the destinations: that
    /// belongs to the core, with the same function that validates a
    /// model's plan (`validate_proposed_rel`). Here only the size is
    /// bounded — whoever decides if a path escapes the directory is
    /// whoever is going to create the folders.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps.
    /// - [`RuntimeError::ReturnTooLarge`] if it returns more moves than
    ///   [`MAX_RENAME_PROPOSALS`] or more bytes than the return cap.
    pub fn plan(
        &mut self,
        id: &str,
        location: Option<&organizer_iface::LocationRef>,
        names: &[String],
    ) -> Result<Result<Vec<organizer_iface::Proposal>, String>, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_organizer_organizer()
            .call_plan(&mut self.store, id, location, names)
            .map_err(|e| map_call_error(&e))?;
        let Ok(moves) = out else {
            return Ok(out);
        };
        if moves.len() > MAX_RENAME_PROPOSALS {
            return Err(RuntimeError::ReturnTooLarge {
                len: moves.len(),
                cap: MAX_RENAME_PROPOSALS,
            });
        }
        let total: usize = moves
            .iter()
            .map(|p| p.current.len() + p.proposed_rel.len())
            .sum();
        cap_total_bytes(total)?;
        Ok(Ok(moves))
    }
}

/// The types the `hook` interface (package `norte:hook`, ADR 0100) puts
/// on the wire: [`hook_iface::Event`], [`hook_iface::Effect`],
/// [`hook_iface::Op`], [`hook_iface::ActorKind`] and
/// [`hook_iface::LocationRef`].
pub use crate::bindings::hook_world::exports::norte::hook::hook as hook_iface;

/// Cap on effects a hook can return per call. A hook receives at most a
/// few hundred events per call and an effect is a sentence for the human:
/// above this it is not a notice, it is a spam channel, and it is
/// rejected AS A WHOLE, fail-closed.
pub const MAX_HOOK_EFFECTS: usize = 64;

/// Cap on the content bytes of ONE sidecar (ADR 0101). A small log or
/// index fits; above this what is being written is a data file, and a
/// hook is not a provider.
pub const MAX_SIDECAR_BYTES: usize = 64 * 1024;

/// Cap on sidecars per call. A call brings events from at most a few
/// directories; four work files per batch is generous.
pub const MAX_SIDECAR_EFFECTS: usize = 4;

/// An instantiated `hook` guest (world `norte-hook`).
pub struct HookInstance {
    store: Store<HostState>,
    bindings: crate::bindings::hook_world::NorteHook,
    /// Epoch ticks for EACH call. See [`PluginInstance::rearm`].
    epoch_deadline: u64,
}

impl std::fmt::Debug for HookInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookInstance").finish_non_exhaustive()
    }
}

impl HookInstance {
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// The VALIDATED `[config]` values the guest will see. Call BEFORE
    /// `on_events`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// The location resolver for the NEXT call: the instance lives across
    /// batches and each batch mints its own tokens, so the host is set
    /// before `on_events` and removed afterward.
    pub fn set_location(&mut self, location: Option<Arc<dyn LocationHost>>) {
        self.store.data_mut().location = location;
    }

    /// Hands the guest the events since the last call and how many were
    /// dropped for a full queue since then. `Ok(Err(sentence))` is the
    /// guest declining with a sentence for the log; the caps are applied
    /// POST-return and reject the whole thing.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps (including the epoch
    ///   deadline).
    /// - [`RuntimeError::ReturnTooLarge`] if it returns more effects than
    ///   [`MAX_HOOK_EFFECTS`] or more bytes than the return cap.
    pub fn on_events(
        &mut self,
        events: &[hook_iface::Event],
        dropped: u64,
    ) -> Result<Result<Vec<hook_iface::Effect>, String>, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_hook_hook()
            .call_on_events(&mut self.store, events, dropped)
            .map_err(|e| map_call_error(&e))?;
        let Ok(effects) = out else {
            return Ok(out);
        };
        if effects.len() > MAX_HOOK_EFFECTS {
            return Err(RuntimeError::ReturnTooLarge {
                len: effects.len(),
                cap: MAX_HOOK_EFFECTS,
            });
        }
        let mut sidecars = 0usize;
        let mut total = 0usize;
        for e in &effects {
            match e {
                hook_iface::Effect::Notify(s) => total += s.len(),
                hook_iface::Effect::WriteSidecar(sc) => {
                    sidecars += 1;
                    if sc.content.len() > MAX_SIDECAR_BYTES {
                        return Err(RuntimeError::ReturnTooLarge {
                            len: sc.content.len(),
                            cap: MAX_SIDECAR_BYTES,
                        });
                    }
                    total += sc.name.len() + sc.content.len();
                }
            }
        }
        if sidecars > MAX_SIDECAR_EFFECTS {
            return Err(RuntimeError::ReturnTooLarge {
                len: sidecars,
                cap: MAX_SIDECAR_EFFECTS,
            });
        }
        cap_total_bytes(total)?;
        Ok(Ok(effects))
    }
}

/// Types of the `previewer` export (record `Span`, alias `PreviewInput`)
/// — re-exported like [`provider_iface`]/[`decorator_iface`]: the caller
/// (`norte-core`, G3a) needs to build the `Vec<Vec<Span>>` that
/// [`PluginInstance::render_styled_preview`] returns into the wire type
/// `SpanWire` (`norte-proto`) without digging into `crate::bindings`.
pub use crate::bindings::exports::norte::plugin::previewer as previewer_iface;

/// A live instance of a DECORATOR guest (ADR 0037 decision 2, world
/// `norte-decorator`): its `Store` (host state + sandbox) and the
/// bindings to call `decorate`.
pub struct DecoratorInstance {
    store: Store<HostState>,
    bindings: crate::bindings::decorator_world::NorteDecorator,
    /// Epoch ticks for EACH call. See [`PluginInstance::rearm`].
    epoch_deadline: u64,
}

impl std::fmt::Debug for DecoratorInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecoratorInstance").finish_non_exhaustive()
    }
}

impl DecoratorInstance {
    /// Rearms the epoch budget before every call (#211). See
    /// [`PluginInstance::rearm`].
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// Installs the VALIDATED `[config]` values the DECORATOR guest will
    /// see via `host-config::get`/`all`. Same contract as
    /// [`PluginInstance::set_settings`]: call BEFORE invoking `decorate`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Decorates a BATCH of entries (batched per visible page, ADR 0037
    /// decision 2): `entries` are the raw names with their class (0.10.0,
    /// ADR 0105) in the order the host lists them; the result is
    /// POSITIONAL 1:1 — never reordered, never sparse. Applies the same
    /// aggregate `MAX_RETURN_BYTES` cap as any other runtime return
    /// value (issue #68), summing `badge`+`role` bytes across ALL
    /// decorations in the batch.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps.
    /// - [`RuntimeError::ReturnTooLarge`] if the returned batch exceeds
    ///   the aggregate cap.
    pub fn decorate(
        &mut self,
        entries: &[decorator_iface::Entry],
    ) -> Result<Vec<decorator_iface::Decoration>, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_plugin_decorator()
            .call_decorate(&mut self.store, entries)
            .map_err(|e| map_call_error(&e))?;
        let total: usize = out
            .iter()
            .map(|d| d.badge.as_deref().map_or(0, str::len) + d.role.as_deref().map_or(0, str::len))
            .sum();
        cap_total_bytes(total)?;
        Ok(out)
    }
}

/// A thumbnail that passed the host's verification (ADR 0107 decision 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// `image/png`, `image/jpeg` or `image/webp`: the one from the MAGIC
    /// bytes, which matched what the guest declared.
    pub mimetype: &'static str,
    /// The encoded bytes, under [`THUMB_MAX_BYTES`].
    pub bytes: Vec<u8>,
    /// What the raster's header says, which matched what was declared.
    pub width: u32,
    /// Ditto.
    pub height: u32,
}

/// The frame a PANEL guest described, already BOUNDED by the host
/// (phase 3).
///
/// What crosses from here on fits within the proto's limits: a guest that
/// sends a thousand lines is describing something nobody is going to
/// read, and trimming is fail-soft — a panel is cosmetic, and cosmetics
/// degrade instead of bringing down the screen.
///
/// No `PartialEq`: the types `bindgen!` generates for the guest do not
/// derive it, and nobody needs to compare two frames for equality — what
/// gets compared is what already crossed the bridge, which does have its
/// own types.
#[derive(Debug, Clone)]
pub struct PanelFrame {
    /// The lines, top to bottom.
    pub lines: Vec<Vec<panel_iface::Span>>,
    /// The clickable zones that survived trimming.
    pub hits: Vec<panel_iface::Hit>,
    /// The opaque state the guest wants for next time.
    pub state: Vec<u8>,
}

/// The panel guest's types (world `norte-panel`), to name them without
/// repeating the bindings' whole path.
pub use crate::bindings::panel_world::exports::norte::panel::panel as panel_iface;

/// A live instance of a PANEL guest (phase 3, world `norte-panel`): its
/// `Store` and the bindings to call `render`.
pub struct PanelInstance {
    store: Store<HostState>,
    bindings: crate::bindings::panel_world::NortePanel,
    /// Epoch ticks for EACH call. See [`PluginInstance::rearm`].
    epoch_deadline: u64,
}

impl std::fmt::Debug for PanelInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanelInstance").finish_non_exhaustive()
    }
}

impl PanelInstance {
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// Installs the VALIDATED `[config]` values the guest will see via
    /// `host-config::get`/`all`. Call BEFORE `render`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Asks for a panel's frame and BOUNDS it before returning it.
    ///
    /// Trimming is the host's job, not the guest's: the limits live in
    /// the proto because they are part of the contract, and two surfaces
    /// trimming differently would show different panels for the same
    /// plugin. A clickable zone pointing at a line trimming removed goes
    /// with it — an invisible button that runs something is worse than a
    /// missing button.
    ///
    /// # Errors
    /// - [`RuntimeError::Guest`] if the guest said no (`Err`).
    /// - [`RuntimeError::Trap`] / [`RuntimeError::Deadline`] if it
    ///   trapped or ran out of time.
    /// - [`RuntimeError::ReturnTooLarge`] if the state exceeds its
    ///   ceiling.
    pub fn render_panel(
        &mut self,
        kind: &str,
        context: &panel_iface::PanelContext,
        location: Option<&panel_iface::LocationRef>,
        state: &[u8],
        event: &panel_iface::PanelEvent,
    ) -> Result<PanelFrame, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_panel_panel()
            .call_render(&mut self.store, kind, context, location, state, event)
            .map_err(|e| map_call_error(&e))?
            // The guest's reason is its own text and goes to the log:
            // bounded like any `host-log` line.
            .map_err(|mut m| {
                if m.len() > MAX_LOG_CHARS {
                    let cut = (0..=MAX_LOG_CHARS)
                        .rev()
                        .find(|i| m.is_char_boundary(*i))
                        .unwrap_or(0);
                    m.truncate(cut);
                }
                RuntimeError::Guest(m)
            })?;
        if out.state.len() > norte_proto::methods::PANEL_MAX_STATE_BYTES {
            return Err(RuntimeError::ReturnTooLarge {
                len: out.state.len(),
                cap: norte_proto::methods::PANEL_MAX_STATE_BYTES,
            });
        }
        let mut lines = out.lines;
        lines.truncate(norte_proto::methods::PANEL_MAX_LINES);
        for line in &mut lines {
            line.truncate(norte_proto::methods::PANEL_MAX_SPANS_PER_LINE);
            // And the TEXT of each span: without this trim, a frame with
            // all its counts within cap — 256 lines of 256 spans — still
            // has no maximum size, because each span carries a free
            // string. It is cut at a character boundary, not a byte one,
            // or the trim would split a UTF-8 sequence in half.
            for span in line.iter_mut() {
                if span.text.len() > norte_proto::methods::PANEL_MAX_SPAN_TEXT {
                    let cut = (0..=norte_proto::methods::PANEL_MAX_SPAN_TEXT)
                        .rev()
                        .find(|i| span.text.is_char_boundary(*i))
                        .unwrap_or(0);
                    span.text.truncate(cut);
                }
            }
        }
        let height = lines.len();
        let hits: Vec<panel_iface::Hit> = out
            .hits
            .into_iter()
            .filter(|h| usize::from(h.row) < height && h.width > 0)
            .take(norte_proto::methods::PANEL_MAX_HITS)
            .collect();
        Ok(PanelFrame {
            lines,
            hits,
            state: out.state,
        })
    }
}

/// A live instance of a THUMBNAIL guest (ADR 0107, world
/// `norte-thumbnail`): its `Store` (host state + sandbox) and the
/// bindings to call `render`.
pub struct ThumbnailInstance {
    store: Store<HostState>,
    bindings: crate::bindings::thumbnail_world::NorteThumbnail,
    /// Epoch ticks for EACH call. See [`PluginInstance::rearm`].
    epoch_deadline: u64,
}

impl std::fmt::Debug for ThumbnailInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbnailInstance").finish_non_exhaustive()
    }
}

impl ThumbnailInstance {
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// Installs the VALIDATED `[config]` values the guest will see via
    /// `host-config::get`/`all`. Call BEFORE `render`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Asks for the thumbnail of `content` (already BOUNDED by the
    /// caller) that fits within `max_edge` — clamped to
    /// [`THUMB_MAX_EDGE`] — and VERIFIES it before returning it (ADR 0107
    /// decision 3): size under [`THUMB_MAX_BYTES`], encoding among the
    /// three the window paints and recognized by its magic bytes,
    /// declared mimetype matching the magic's, declared dimensions
    /// matching the header's, and neither above the requested edge. A
    /// guest describes an image; it does not stuff bytes into a `blob:`.
    ///
    /// # Errors
    /// - [`RuntimeError::Guest`] if the guest said no (`Err`).
    /// - [`RuntimeError::Trap`] / [`RuntimeError::Deadline`] if it
    ///   trapped or ran out of time.
    /// - [`RuntimeError::ReturnTooLarge`] above the byte ceiling.
    /// - [`RuntimeError::ThumbnailRejected`] if it fails verification.
    pub fn render_thumbnail(
        &mut self,
        mimetype: &str,
        content: &[u8],
        max_edge: u32,
    ) -> Result<Thumbnail, RuntimeError> {
        self.rearm();
        let max_edge = max_edge.clamp(1, THUMB_MAX_EDGE);
        let input = thumbnail_iface::ThumbInput {
            mimetype: mimetype.to_owned(),
            content: content.to_vec(),
            max_edge,
        };
        let out = self
            .bindings
            .norte_thumbnail_thumbnail()
            .call_render(&mut self.store, &input)
            .map_err(|e| map_call_error(&e))?
            // The guest's reason is its own text and goes to the log:
            // bounded like any `host-log` line, or a guest would make it
            // megabytes long.
            .map_err(|mut m| {
                if m.len() > MAX_LOG_CHARS {
                    let cut = (0..=MAX_LOG_CHARS)
                        .rev()
                        .find(|i| m.is_char_boundary(*i))
                        .unwrap_or(0);
                    m.truncate(cut);
                }
                RuntimeError::Guest(m)
            })?;
        if out.bytes.len() > THUMB_MAX_BYTES {
            return Err(RuntimeError::ReturnTooLarge {
                len: out.bytes.len(),
                cap: THUMB_MAX_BYTES,
            });
        }
        let Some((kind, w, h)) = crate::thumb::sniff(&out.bytes) else {
            return Err(RuntimeError::ThumbnailRejected(
                "the bytes are neither PNG, JPEG nor WebP".to_owned(),
            ));
        };
        if kind.mimetype() != out.mimetype {
            return Err(RuntimeError::ThumbnailRejected(format!(
                "declares {} and the magic bytes say {}",
                out.mimetype,
                kind.mimetype()
            )));
        }
        if (w, h) != (out.width, out.height) {
            return Err(RuntimeError::ThumbnailRejected(format!(
                "declares {}x{} and the header says {w}x{h}",
                out.width, out.height
            )));
        }
        if w == 0 || h == 0 || w > max_edge || h > max_edge {
            return Err(RuntimeError::ThumbnailRejected(format!(
                "{w}x{h} does not fit within the requested edge ({max_edge})"
            )));
        }
        // The second gate (ADR 0107 decision 3): decoded HERE, in safe
        // Rust and with limits, and what crosses is a raster made by the
        // host. The guest's bytes never reach the webview's native
        // decoders.
        let (mimetype, bytes) = crate::thumb::reencode(&out.bytes, w, h, THUMB_MAX_BYTES)
            .map_err(RuntimeError::ThumbnailRejected)?;
        Ok(Thumbnail {
            mimetype,
            bytes,
            width: w,
            height: h,
        })
    }
}

/// A live instance of a COLUMNS guest (ADR 0037 decision 2, world
/// `norte-columns`): its `Store` (host state + sandbox) and the bindings
/// to call `column-values`.
pub struct ColumnsInstance {
    store: Store<HostState>,
    bindings: crate::bindings::columns_world::NorteColumns,
    /// Epoch ticks for EACH call. See [`PluginInstance::rearm`].
    epoch_deadline: u64,
}

impl std::fmt::Debug for ColumnsInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnsInstance").finish_non_exhaustive()
    }
}

impl ColumnsInstance {
    /// Rearms the epoch budget before every call (#211). See
    /// [`PluginInstance::rearm`].
    fn rearm(&mut self) {
        self.store.set_epoch_deadline(self.epoch_deadline);
    }

    /// Installs the VALIDATED `[config]` values the COLUMNS guest will
    /// see via `host-config::get`/`all`. Same contract as
    /// [`PluginInstance::set_settings`]: call BEFORE invoking
    /// `column-values`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Values of column `id` for a BATCH of entries: the same 1:1
    /// positional contract as [`DecoratorInstance::decorate`]. Each cell
    /// is `Option<String>` — `None` = "does not apply to this entry",
    /// distinguishable from an actual empty value (ADR 0037 decision 1).
    /// Applies the same aggregate `MAX_RETURN_BYTES` cap, summing the
    /// bytes of the `Some` cells.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] if the guest traps.
    /// - [`RuntimeError::ReturnTooLarge`] if the returned batch exceeds
    ///   the aggregate cap.
    ///
    /// `location` is the OPAQUE token of the location being listed, or
    /// `None` if the guest was not approved for the capability (or the
    /// host could not open the directory). A guest that receives `None`
    /// still has to keep answering.
    pub fn column_values(
        &mut self,
        id: &str,
        location: Option<&columns_iface::LocationRef>,
        entries: &[Vec<u8>],
    ) -> Result<Vec<Option<String>>, RuntimeError> {
        self.rearm();
        let out = self
            .bindings
            .norte_plugin_columns()
            .call_column_values(&mut self.store, id, location, entries)
            .map_err(|e| map_call_error(&e))?;
        let total: usize = out.iter().map(|v| v.as_deref().map_or(0, str::len)).sum();
        cap_total_bytes(total)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_return_value_passes_under_the_cap() {
        let ok = "x".repeat(MAX_RETURN_BYTES);
        assert_eq!(cap_return_value(ok.clone()).unwrap().len(), ok.len());
    }

    #[test]
    fn cap_return_value_rejects_above_the_cap() {
        let big = "x".repeat(MAX_RETURN_BYTES + 1);
        let err = cap_return_value(big).unwrap_err();
        assert!(
            matches!(err, RuntimeError::ReturnTooLarge { len, cap }
                if len == MAX_RETURN_BYTES + 1 && cap == MAX_RETURN_BYTES),
            "was {err:?}"
        );
    }

    #[test]
    fn check_artifact_size_accepts_at_the_cap_and_rejects_above() {
        assert!(check_artifact_size(MAX_ARTIFACT_BYTES).is_ok());
        let err = check_artifact_size(MAX_ARTIFACT_BYTES + 1).unwrap_err();
        assert!(
            matches!(err, RuntimeError::ArtifactTooLarge { len, cap }
                if len == MAX_ARTIFACT_BYTES + 1 && cap == MAX_ARTIFACT_BYTES),
            "was {err:?}"
        );
    }

    /// A minimal `HostState` (no engine: the WASI fields are built
    /// loose) to test `host_config::Host` DIRECTLY, without compiling any
    /// WASM component (P2 Task 3) — covers `get`/`all` cheaply on any
    /// toolchain, including ones without the `wasm32-wasip2` target.
    fn bare_host_state(settings: BTreeMap<String, String>) -> HostState {
        HostState {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            caps: Capabilities::default(),
            logs: Vec::new(),
            scoped_resources: HashMap::new(),
            location: None,
            settings,
            limits: StoreLimitsBuilder::new().build(),
        }
    }

    #[test]
    fn host_config_get_returns_the_value_or_none() {
        let mut state = bare_host_state(BTreeMap::from([(
            "greeting".to_string(),
            "hola".to_string(),
        )]));
        assert_eq!(
            host_config::Host::get(&mut state, "greeting".to_string()),
            Some("hola".to_string())
        );
        assert_eq!(
            host_config::Host::get(&mut state, "not-declared".to_string()),
            None,
            "a key absent from the resolved map is None, not an error"
        );
    }

    #[test]
    fn host_config_all_returns_every_pair() {
        let mut state = bare_host_state(BTreeMap::from([
            ("greeting".to_string(), "hola".to_string()),
            ("retries".to_string(), "3".to_string()),
        ]));
        let mut all = host_config::Host::all(&mut state);
        all.sort();
        assert_eq!(
            all,
            vec![
                ("greeting".to_string(), "hola".to_string()),
                ("retries".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn host_config_without_settings_is_an_empty_map() {
        // `prepare_common`'s default before `set_settings` (same
        // criterion as a plugin without `[config]`, Task 1/2): neither
        // `get` nor `all` should return anything, and neither should ever
        // panic. `HostState`/`host_config::Host` is the ONLY
        // implementation shared by BOTH worlds (`with:` in bindings.rs) —
        // this test covers both `PluginInstance` (command/previewer) and
        // `ProviderInstance` (P2 Task 4a: the concrete case of a provider
        // that never calls `set_settings`, e.g. FTP today, see
        // `norte_core::plugin_provider`/`ftp_plugin`).
        let mut state = bare_host_state(BTreeMap::new());
        assert_eq!(
            host_config::Host::get(&mut state, "anything".to_string()),
            None
        );
        assert!(host_config::Host::all(&mut state).is_empty());
    }
}
