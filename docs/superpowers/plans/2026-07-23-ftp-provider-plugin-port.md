# FTP Provider Plugin Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the full FTP provider into a WASM guest plugin (all ops over `suppaftp` sync + CR/LF anti-injection defenses), add a WIT `configure` entry point, wire the host (hostname→IP resolve, grant `net`, configure), pass `provider_contract!` against `libunftp`, and retire `norte-vfs-ftp`.

**Architecture:** The guest owns a synchronous `suppaftp::FtpStream` over `wasi:sockets` (single-threaded, thread-local). The host resolves DNS (guest has no DNS), grants the `net` capability scoped to the resolved IP, instantiates the guest via `PluginProvider`, then calls the new WIT `configure(provider-config)` to establish the FTP session. Every `Provider` op projects to a bounded synchronous WIT call, exactly as the existing `provider` interface already does for reads/writes. FTPS is deferred (`aws-lc-rs` will not compile to `wasm32-wasip2`); plaintext FTP only, documented as debt.

**Tech Stack:** `wasmtime` component model, `wit-bindgen` (guest), `suppaftp` v6 sync (`default-features = false`, no TLS) in the guest, `libunftp`/`unftp-sbe-fs` in-process test server, `PluginProvider` adapter in `norte-core`.

---

## Scope & key decisions

1. **WIT `configure` lives in the `provider` interface** (not a new world). The `provider` interface is pre-release (ADR 0032), so adding a function is a permitted breaking change under a 0.x minor bump (`0.3.0` → `0.4.0`). All provider guests must implement it; the in-memory guests implement a trivial `Ok`-returning stub. One world = one `ProviderInstance` type in the host = minimal binding churn.
2. **The guest connects itself.** The host cannot hand a live `suppaftp` stream into wasm, so credentials cross the WIT boundary into guest sandbox memory. This is inherent and documented (the guest needs them to `login`). Over plaintext FTP the password is already on the wire in clear — FTPS deferred.
3. **Per-chunk RETR.** The WIT `read(segs, offset, len)` is call-bounded and cannot hold a live data connection across calls (there is no read-stream resource in the interface). Each `read` performs one REST+RETR cycle, reading up to `len` then draining to EOF and finalizing. This is O(n²) transfer for large files read in chunks — acceptable for the contract (tiny files) and filed as debt.
4. **Production shipping (ADR 0033).** `norte-vfs-ftp` is removed, so `norte-core::connect` must route `ftp://` through the plugin. The guest `.wasm` cannot be a build-time dependency of `norte-core` (the `wasm32-wasip2` target may be absent on a build host). Decision: commit a prebuilt `ftp-provider.wasm` artifact and load it with `wasmtime::component::Component::from_binary` via `include_bytes!` — no temp file, no build-host target requirement. A `just build-ftp-wasm` recipe rebuilds and re-commits the artifact.
5. **`provider_contract!` hard-requires the target.** The macro cannot SKIP, so the contract test crate requires `wasm32-wasip2` at test time (present in the `just ci` environment). Documented in the test module.

## File structure

- `crates/norte-plugin-host/wit/norte-plugin.wit` — add `provider-config` record + `configure` func to `provider`; bump to `0.4.0`. **Mirror into all 8 `examples-wasm/*/wit/norte-plugin.wit` copies** (identical files, kept in sync manually).
- `crates/norte-plugin-host/src/runtime.rs` — `ProviderInstance::configure`.
- `crates/norte-plugin-host/src/lib.rs` — re-export the `provider-config` type if needed.
- `crates/norte-plugin-host/examples-wasm/provider-mem/src/lib.rs`, `provider-mem-rw/src/lib.rs` — no-op `configure` stub.
- `crates/norte-plugin-host/examples-wasm/ftp-provider/` — **new guest crate** (`Cargo.toml`, `src/lib.rs`, `wit/norte-plugin.wit`).
- `crates/norte-core/src/plugin_provider.rs` — `PluginProvider::configure` pass-through (or construction that configures).
- `crates/norte-core/src/ftp_plugin.rs` — **new**: host wiring helper (resolve host→IP, build net caps, instantiate + configure) + the embedded artifact loader.
- `crates/norte-core/resources/ftp-provider.wasm` — **new** committed prebuilt artifact.
- `crates/norte-core/tests/ftp_provider_contract.rs` — **new**: `provider_contract!` over the guest + in-process `libunftp`.
- `crates/norte-core/src/connect.rs` — switch the `"ftp"` arm to the plugin path.
- `docs/adr/0033-ftp-provider-as-plugin.md` — **new** ADR (shipping + threat model).
- Deletions: `crates/norte-vfs-ftp/` whole crate; workspace `Cargo.toml` member + dep; `norte-connect` FTP connector usage in core (keep `FtpConnector` only if still needed for TLS — see Task 8); `ARCHITECTURE.md`, `docs/spec/norte-spec.md` references.
- `justfile` — `build-ftp-wasm` recipe; drop the `norte-vfs-ftp --features it-ftp` nightly line.

---

## Task 1: WIT `configure` + host binding

**Files:**
- Modify: `crates/norte-plugin-host/wit/norte-plugin.wit`
- Modify (mirror): all `crates/norte-plugin-host/examples-wasm/*/wit/norte-plugin.wit`
- Modify: `crates/norte-plugin-host/src/runtime.rs`
- Modify: `crates/norte-plugin-host/examples-wasm/provider-mem/src/lib.rs`
- Modify: `crates/norte-plugin-host/examples-wasm/provider-mem-rw/src/lib.rs`
- Test: `crates/norte-plugin-host/tests/provider_e2e.rs` (extend: call `configure` no-op)

- [ ] **Step 1: Edit the WIT.** In the `provider` interface, after `read`, add:

```wit
    /// Configuración de un provider que ESTABLECE SU PROPIA conexión (#30
    /// stage 3c, FTP). El HOST resuelve el hostname a IP, concede la capability
    /// `net` acotada a esa IP y llama a `configure` con el endpoint YA resuelto
    /// (`ip:puerto`) — el guest jamás resuelve DNS. Las credenciales cruzan a la
    /// memoria del guest (sandbox aislado): imprescindibles para el `login`;
    /// sobre FTP en claro (FTPS = deuda, aws-lc-rs no compila a wasm) ya viajan
    /// sin cifrar por el cable. `base` = raíz remota absoluta bajo la que vive
    /// todo. Un provider que no necesita conexión (mem) la implementa como no-op.
    record provider-config {
        /// Endpoint YA resuelto por el host: `ip:puerto`.
        endpoint: string,
        /// Usuario de login (`anonymous` por convención guest).
        user: string,
        /// Password de login (viaja a memoria del guest para autenticar).
        password: string,
        /// Raíz remota absoluta (sin `..`, sin barra final significativa).
        base: string,
    }
    configure: func(cfg: provider-config) -> result<_, vfs-error>;
```

Bump the package header comment and version to `package norte:plugin@0.4.0;`. Update the top-of-file version log with a `0.3.0 → 0.4.0` line explaining `configure` is a breaking add to `provider` (guests need a new export), safe because `provider` is pre-release with no published guests.

- [ ] **Step 2: Mirror the WIT to all guest copies.**

Run: `for d in crates/norte-plugin-host/examples-wasm/*/wit; do cp crates/norte-plugin-host/wit/norte-plugin.wit "$d/norte-plugin.wit"; done`

- [ ] **Step 3: Add no-op `configure` to the two mem guests.** In both `provider-mem/src/lib.rs` and `provider-mem-rw/src/lib.rs`, inside `impl Guest for Mem`, add (using the generated `ProviderConfig` type):

```rust
    fn configure(_cfg: exports::norte::plugin::provider::ProviderConfig) -> Result<(), VfsError> {
        // El provider en memoria no establece conexión: no-op.
        Ok(())
    }
```

(Add `ProviderConfig` to the `use exports::norte::plugin::provider::{...}` list.)

- [ ] **Step 4: Add `ProviderInstance::configure` to the host.** In `runtime.rs`, after `read`, add:

```rust
    /// Configura la conexión del guest-provider (#30 stage 3c): endpoint YA
    /// resuelto, credenciales y base. El `Ok` interno es el resultado lógico
    /// del guest; el `Err` externo es un trap.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn configure(
        &mut self,
        cfg: provider_iface::ProviderConfig,
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_configure(&mut self.store, &cfg)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }
```

Confirm `provider_iface` (re-exported at `runtime.rs:462`) now surfaces `ProviderConfig`. It does — it re-exports the whole `provider` interface module.

- [ ] **Step 5: Build the workspace + guests.**

Run: `cargo build -p norte-plugin-host && cargo build --release --target wasm32-wasip2 --manifest-path crates/norte-plugin-host/examples-wasm/provider-mem-rw/Cargo.toml`
Expected: both compile (the mem guest with its new stub).

- [ ] **Step 6: Extend `provider_e2e.rs`** to call `configure` on the mem guest and assert `Ok`, proving the new binding round-trips. Add near the top of `provider_wit_e2e_wasm_real`, after instantiation:

```rust
    let cfg = norte_plugin_host::provider_iface::ProviderConfig {
        endpoint: String::new(),
        user: String::new(),
        password: String::new(),
        base: String::new(),
    };
    inst.configure(cfg).expect("configure sin trap").expect("mem configure no-op");
```

- [ ] **Step 7: Run the e2e.**

Run: `cargo nextest run -p norte-plugin-host --test provider_e2e`
Expected: PASS (or SKIP if the runner lacks the target — present here).

- [ ] **Step 8: Commit.**

```bash
git add -A
git commit -m "feat(plugin-host): #30 stage 3c — WIT configure() en provider (0.4.0)"
```

---

## Task 2: The FTP provider guest crate (scaffold + configure + capabilities/stat)

**Files:**
- Create: `crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml`
- Create: `crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs`
- Create: `crates/norte-plugin-host/examples-wasm/ftp-provider/wit/norte-plugin.wit` (copy)

- [ ] **Step 1: Cargo.toml** (mirror `ftp-probe`, but the provider world, `bytes` not needed):

```toml
# Guest WASM (#30 stage 3c): el provider FTP COMPLETO en un guest. suppaftp SYNC
# sobre wasi:sockets, todas las ops del trait Provider proyectadas a la interfaz
# WIT `provider`, con las defensas anti-inyección CR/LF. Sin TLS (aws-lc-rs no
# compila a wasm; FTPS = deuda). NO es miembro del workspace.
[workspace]

[package]
name = "ftp-provider"
edition = "2021"
version = "0.0.0"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.46"
suppaftp = { version = "6", default-features = false, features = ["sync"] }

[profile.release]
panic = "abort"
opt-level = "s"
```

(If `suppaftp` v6 has no explicit `sync` feature, use `default-features = false` alone — verify with `cargo tree`; the sync `FtpStream` is the default surface.)

- [ ] **Step 2: Copy the WIT.**

Run: `mkdir -p crates/norte-plugin-host/examples-wasm/ftp-provider/wit && cp crates/norte-plugin-host/wit/norte-plugin.wit crates/norte-plugin-host/examples-wasm/ftp-provider/wit/`

- [ ] **Step 3: `src/lib.rs` — scaffold, thread-local connection, `configure`, `capabilities`, `stat`.** This is the port of `norte-vfs-ftp/src/provider.rs` to sync + WIT. Full content below (Tasks 2–4 build it up; write the whole file once here and extend). Start with:

```rust
//! Guest WASM (#30 stage 3c): el provider FTP COMPLETO. Proyección SÍNCRONA del
//! trait `norte_vfs::Provider` sobre `suppaftp::FtpStream` (sync) por
//! `wasi:sockets`. Port de `norte-vfs-ftp` con las MISMAS defensas
//! anti-inyección CR/LF y el mismo tratamiento MLSD/LIST. Sin TLS (FTPS=deuda).
//!
//! Single-threaded wasm: la conexión de control vive en un `thread_local`
//! `RefCell<Option<FtpStream>>`, no en `Arc<Mutex>`. Nombres crudos en bytes
//! (regla 1); FTP exige UTF-8 → un nombre no representable es `invalid-path`.

use std::cell::RefCell;

use suppaftp::list::{File, ListParser};
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, Status};

wit_bindgen::generate!({
    world: "norte-provider",
    path: "wit",
});

use exports::norte::plugin::provider::{
    Caps, Entry, EntryKind, Guest, GuestWriter, Page, ProviderConfig, VfsError, Writer,
};

/// Cota defensiva de entradas materializadas por listado (issue #40).
const MAX_LIST_ENTRIES: usize = 1 << 20;
/// Prefijo del staging de escritura (ADR 0012).
const PARTIAL_PREFIX: &str = ".norte-partial.";

struct Session {
    ftp: FtpStream,
    base: String,
    has_mlsd: bool,
    seq: u64,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Ejecuta `f` con la sesión establecida, o `provider-unavailable` si `configure`
/// no se llamó / falló.
fn with_session<T>(f: impl FnOnce(&mut Session) -> Result<T, VfsError>) -> Result<T, VfsError> {
    SESSION.with_borrow_mut(|s| match s.as_mut() {
        Some(sess) => f(sess),
        None => Err(VfsError::ProviderUnavailable),
    })
}

struct FtpProvider;

impl Guest for FtpProvider {
    fn configure(cfg: ProviderConfig) -> Result<(), VfsError> {
        // base: absoluta, sin CR/LF/NUL (defensa en profundidad: inyectaría un
        // comando FTP en cada op saltándose el filtro por-segmento).
        let mut base = cfg.base;
        while base.len() > 1 && base.ends_with('/') {
            base.pop();
        }
        if !base.starts_with('/') || base.contains(['\r', '\n', '\0']) {
            return Err(VfsError::InvalidPath);
        }
        let mut ftp = FtpStream::connect(&cfg.endpoint).map_err(|_| VfsError::ProviderUnavailable)?;
        ftp.login(&cfg.user, &cfg.password).map_err(|e| map_err(&e))?;
        let has_mlsd = setup_conn(&mut ftp).map_err(|e| map_err(&e))?;
        SESSION.set(Some(Session { ftp, base, has_mlsd, seq: 0 }));
        Ok(())
    }

    fn capabilities() -> Caps {
        // El adapter host mapea read_only=false → sin READ_ONLY. El resto de
        // flags (APPEND/CASE_*) no viajan por la interfaz WIT stage-2 (solo
        // read_only); el adapter los fija por su cuenta si hiciera falta. Para
        // el contrato basta read_only=false.
        Caps { read_only: false }
    }

    fn stat(segments: Vec<Vec<u8>>) -> Result<Entry, VfsError> {
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            // La raíz del provider es el dir base.
            if segments.is_empty() {
                return Ok(Entry { name: Vec::new(), kind: EntryKind::Dir, size: None });
            }
            match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                Some(f) => Ok(entry_from_file(last_name(&segments), &f)),
                None => Err(VfsError::NotFound),
            }
        })
    }

    // ... list_dir, read, writer/open_writer, make_dir, remove, rename: Tasks 3–4
    # placeholder
}
```

**Do not leave the placeholder** — Tasks 3 and 4 fill in the remaining methods and the free functions (`remote`, `setup_conn`, `stat_remote`, `map_err`, `entry_from_file`, `parse_list_line`, `last_name`). Write them all before compiling. The free functions are direct sync ports of the async originals in `norte-vfs-ftp/src/provider.rs` — copy their bodies, dropping `.await` and `async`, and returning `VfsError` instead of `Error`:

```rust
/// Path remoto absoluto bajo `base` desde segmentos crudos. MISMAS defensas que
/// el provider original: UTF-8 exigido, sin `/`/`.`/`..`, sin CR/LF, ≤255 bytes.
fn remote(base: &str, segments: &[Vec<u8>]) -> Result<String, VfsError> {
    let mut out = String::from(base);
    for seg in segments {
        let name = std::str::from_utf8(seg).map_err(|_| VfsError::InvalidPath)?;
        if name.contains('/') || name == "." || name == ".." {
            return Err(VfsError::InvalidPath);
        }
        if name.contains(['\r', '\n']) {
            return Err(VfsError::InvalidPath);
        }
        if seg.len() > 255 {
            return Err(VfsError::InvalidPath);
        }
        if out.len() > 1 || !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(name);
    }
    Ok(out)
}

fn last_name(segments: &[Vec<u8>]) -> Vec<u8> {
    segments.last().cloned().unwrap_or_default()
}

fn setup_conn(ftp: &mut FtpStream) -> Result<bool, FtpError> {
    ftp.transfer_type(FileType::Binary)?;
    let feats = ftp.feat().ok();
    let has_mlsd = feats.as_ref().is_some_and(|f| {
        f.keys().any(|k| k.eq_ignore_ascii_case("MLST") || k.eq_ignore_ascii_case("MLSD"))
    });
    if feats.as_ref().is_some_and(|f| f.keys().any(|k| k.eq_ignore_ascii_case("UTF8"))) {
        let _ = ftp.opts("UTF8", Some("ON"));
    }
    Ok(has_mlsd)
}

fn map_err(e: &FtpError) -> VfsError {
    match e {
        FtpError::UnexpectedResponse(r) => match r.status {
            Status::FileUnavailable => VfsError::NotFound,
            Status::NotLoggedIn => VfsError::PermissionDenied,
            Status::BadFilename => VfsError::InvalidPath,
            Status::RequestFileActionIgnored => VfsError::Io,
            _ => VfsError::Io,
        },
        FtpError::ConnectionError(_) | FtpError::SecureError(_) => VfsError::ProviderUnavailable,
        FtpError::InvalidAddress(_) => VfsError::InvalidPath,
        FtpError::BadResponse | FtpError::DataConnectionAlreadyOpen => VfsError::Io,
    }
}

fn parse_list_line(line: &str) -> Option<File> {
    ListParser::parse_posix(line).ok().or_else(|| ListParser::parse_dos(line).ok())
}

fn entry_from_file(name: Vec<u8>, f: &File) -> Entry {
    let kind = if f.is_symlink() { EntryKind::Symlink }
        else if f.is_directory() { EntryKind::Dir }
        else if f.is_file() { EntryKind::File }
        else { EntryKind::Other };
    let size = (kind == EntryKind::File).then(|| f.size() as u64);
    Entry { name, kind, size }
}

/// `stat` de `remote`: MLST si hay MLSD, si no LIST del padre + búsqueda. Port
/// sync directo de `stat_remote` original (misma contención de raíz/base y el
/// salto de nombres lossy con U+FFFD).
fn stat_remote(ftp: &mut FtpStream, remote: &str, has_mlsd: bool, base: &str)
    -> Result<Option<File>, VfsError>
{
    if has_mlsd {
        return match ftp.mlst(Some(remote)) {
            Ok(line) => {
                let f = ListParser::parse_mlst(&line).map_err(|_| VfsError::Io)?;
                Ok(Some(f))
            }
            Err(e) => match map_err(&e) { VfsError::NotFound => Ok(None), other => Err(other) },
        };
    }
    if remote == base { return Ok(None); }
    let (parent, child) = match remote.rfind('/') {
        Some(0) => ("/", &remote[1..]),
        Some(i) => (&remote[..i], &remote[i + 1..]),
        None => return Ok(None),
    };
    if child.is_empty() { return Ok(None); }
    let lines = match ftp.list(Some(parent)) {
        Ok(l) => l,
        Err(e) => return match map_err(&e) { VfsError::NotFound => Ok(None), other => Err(other) },
    };
    for line in lines {
        let Some(f) = parse_list_line(&line) else { continue };
        let n = f.name();
        if n.contains('\u{FFFD}') { continue; }
        if n == child { return Ok(Some(f)); }
    }
    Ok(None)
}

fn exists(ftp: &mut FtpStream, remote: &str, has_mlsd: bool, base: &str) -> Result<bool, VfsError> {
    Ok(stat_remote(ftp, remote, has_mlsd, base)?.is_some())
}

fn create_empty(ftp: &mut FtpStream, remote: &str) -> Result<(), VfsError> {
    let data = ftp.put_with_stream(remote).map_err(|e| map_err(&e))?;
    ftp.finalize_put_stream(data).map_err(|e| map_err(&e))
}
```

- [ ] **Step 4: Build the guest.**

Run: `cargo build --release --target wasm32-wasip2 --manifest-path crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml`
Expected: compiles once `list_dir`/`read`/writer/`make_dir`/`remove`/`rename` from Tasks 3–4 are present. (Compile at the end of Task 4, not here — this file is written whole.)

---

## Task 3: Guest `list_dir` + `read` (per-chunk RETR)

**Files:** Modify `crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs`

- [ ] **Step 1: `list_dir`.** Port the async `list` body sync. WIT `list_dir` is paginated; the guest returns ONE page (`next_cursor: None`) — the whole listing (suppaftp buffers it entirely anyway). Ignore the incoming cursor.

```rust
    fn list_dir(segments: Vec<Vec<u8>>, _cursor: Option<Vec<u8>>) -> Result<Page, VfsError> {
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            let lines = if s.has_mlsd {
                s.ftp.mlsd(Some(&remote))
            } else {
                s.ftp.list(Some(&remote))
            }
            .map_err(|e| map_err(&e))?;
            let mut entries = Vec::new();
            for line in lines {
                if entries.len() >= MAX_LIST_ENTRIES {
                    return Err(VfsError::Io);
                }
                let parsed = if s.has_mlsd {
                    ListParser::parse_mlsd(&line).ok()
                } else {
                    parse_list_line(&line)
                };
                let f = match parsed {
                    Some(f) => f,
                    None if s.has_mlsd => return Err(VfsError::Io),
                    None => continue,
                };
                let name = if s.has_mlsd {
                    let Some((_, n)) = line.split_once(' ') else { return Err(VfsError::Io) };
                    n
                } else {
                    f.name()
                };
                if name == "." || name == ".." { continue; }
                if name.contains('\u{FFFD}') || name.contains('/') {
                    return Err(VfsError::InvalidPath);
                }
                entries.push(entry_from_file(name.as_bytes().to_vec(), &f));
            }
            Ok(Page { entries, next_cursor: None })
        })
    }
```

Note: the adapter (`PluginProvider::list`) rebuilds `VPath` segments from `entry.name` and skips names that are not valid `Segment`s — so `list_dir` returns raw bytes and the adapter enforces `Segment` validity.

- [ ] **Step 2: `read` — one REST+RETR cycle per call, drained to EOF.**

```rust
    fn read(segments: Vec<Vec<u8>>, offset: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        use std::io::Read;
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            // Rechaza dir/ausente con la MISMA taxonomía que el original.
            match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                None => return Err(VfsError::NotFound),
                Some(f) if f.is_directory() => return Err(VfsError::Conflict),
                Some(_) => {}
            }
            if offset > 0 {
                let off = usize::try_from(offset).map_err(|_| VfsError::Io)?;
                s.ftp.resume_transfer(off).map_err(|e| map_err(&e))?;
            }
            let mut reader = s.ftp.retr_as_stream(&remote).map_err(|e| map_err(&e))?;
            // Lee hasta `len` bytes.
            let want = usize::try_from(len).unwrap_or(usize::MAX);
            let mut out = Vec::new();
            let mut buf = [0u8; 8192];
            while out.len() < want {
                let n = reader.read(&mut buf).map_err(|_| VfsError::Io)?;
                if n == 0 { break; }
                let take = n.min(want - out.len());
                out.extend_from_slice(&buf[..take]);
                if out.len() >= want {
                    // Drena el resto (RETR va offset→EOF; parar sin drenar
                    // desincronizaría el control).
                    loop {
                        match reader.read(&mut buf) { Ok(0) | Err(_) => break, Ok(_) => {} }
                    }
                    break;
                }
            }
            s.ftp.finalize_retr_stream(reader).map_err(|e| map_err(&e))?;
            Ok(out)
        })
    }
```

Note: `len` from the adapter is always `Some(64KiB)` per chunk (or the remaining). `retr_as_stream` returns a reader whose type may need an explicit `Box<dyn Read>` — check suppaftp v6 sync signature and adapt (it returns `DataStream<...>` implementing `Read`). Add a module doc line flagging the O(n²) per-chunk RETR as debt.

- [ ] **Step 3: (guest not independently compilable until Task 4 — no build step here.)**

---

## Task 4: Guest writer + mkdir/remove/rename, then build

**Files:** Modify `crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs`

- [ ] **Step 1: The transactional writer resource.** The WIT `writer` accumulates via `write`, publishes on `commit`, discards on `abort`. Port the FTP staging approach: `open_writer` checks the final path is free and creates an empty staging file; `write` APPEs a chunk; `commit` re-checks + renames staging→final; `abort` deletes staging. Because the guest is single-threaded and the WIT `writer` is a resource with its own methods that borrow `&self`, hold the staging path in the resource and reach the connection via the thread-local on each call.

```rust
    type Writer = FtpWriter;

    fn open_writer(segments: Vec<Vec<u8>>) -> Result<Writer, VfsError> {
        with_session(|s| {
            let final_remote = remote(&s.base, &segments)?;
            let parent = {
                // padre = remote de segments[..len-1]
                let plen = segments.len().saturating_sub(1);
                remote(&s.base, &segments[..plen])?
            };
            if exists(&mut s.ftp, &final_remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            let seq = s.seq;
            s.seq += 1;
            let staging = format!("{parent}/{PARTIAL_PREFIX}eph.{seq}");
            create_empty(&mut s.ftp, &staging)?;
            Ok(Writer::new(FtpWriter {
                staging: RefCell::new(Some(staging)),
                final_remote,
            }))
        })
    }

    fn make_dir(segments: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            if exists(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp.mkdir(&remote).map_err(|e| map_err(&e))
        })
    }

    fn remove(segments: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            let f = stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)?.ok_or(VfsError::NotFound)?;
            if f.is_directory() { s.ftp.rmdir(&remote).map_err(|e| map_err(&e)) }
            else { s.ftp.rm(&remote).map_err(|e| map_err(&e)) }
        })
    }

    fn rename(src: Vec<Vec<u8>>, dst: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            let from_r = remote(&s.base, &src)?;
            let to_r = remote(&s.base, &dst)?;
            if exists(&mut s.ftp, &to_r, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp.rename(&from_r, &to_r).map_err(|e| map_err(&e))
        })
    }
}

struct FtpWriter {
    staging: RefCell<Option<String>>,
    final_remote: String,
}

impl GuestWriter for FtpWriter {
    fn write(&self, chunk: Vec<u8>) -> Result<(), VfsError> {
        use std::io::Write;
        if chunk.is_empty() { return Ok(()); }
        let staging = self.staging.borrow().clone().ok_or(VfsError::Io)?;
        with_session(|s| {
            let mut data = s.ftp.append_with_stream(&staging).map_err(|e| map_err(&e))?;
            let res = data.write_all(&chunk);
            let fin = s.ftp.finalize_put_stream(data);
            res.map_err(|_| VfsError::Io)?;
            fin.map_err(|e| map_err(&e))
        })
    }

    fn commit(&self) -> Result<(), VfsError> {
        let staging = self.staging.borrow_mut().take().ok_or(VfsError::Io)?;
        with_session(|s| {
            if exists(&mut s.ftp, &self.final_remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp.rename(&staging, &self.final_remote).map_err(|e| map_err(&e))
        })
    }

    fn abort(&self) -> Result<(), VfsError> {
        if let Some(staging) = self.staging.borrow_mut().take() {
            let _ = with_session(|s| { let _ = s.ftp.rm(&staging); Ok(()) });
        }
        Ok(())
    }
}

export!(FtpProvider);
```

Note the WIT `writer` methods take `&self` (see `provider-mem-rw`'s `GuestWriter`), so mutable staging state lives behind `RefCell`. The `write` empty-chunk short-circuit matches the async sink. There is no `keep`/`open_resumable`/`partial_digest` in the WIT interface — the adapter's `open_resumable` falls back to the trait default (which the contract's resume tests auto-skip). Good: those contract cases self-skip.

- [ ] **Step 2: Build the guest.**

Run: `cargo build --release --target wasm32-wasip2 --manifest-path crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml`
Expected: PASS. Fix any suppaftp v6 sync API mismatches (method names: `retr_as_stream`, `resume_transfer`, `finalize_retr_stream`, `put_with_stream`, `append_with_stream`, `finalize_put_stream`, `mlst`, `mlsd`, `feat`, `opts`, `transfer_type`, `mkdir`, `rmdir`, `rm`, `rename` — verify each against `suppaftp` v6 sync docs; the async originals used the same names).

- [ ] **Step 3: Commit.**

```bash
git add -A
git commit -m "feat(plugin-host): #30 stage 3c — guest ftp-provider (todas las ops, sync)"
```

---

## Task 5: Host wiring — resolve DNS, grant net, configure

**Files:**
- Create: `crates/norte-core/src/ftp_plugin.rs`
- Modify: `crates/norte-core/src/lib.rs` (module + re-export)
- Modify: `crates/norte-core/src/plugin_provider.rs` (`PluginProvider::configure`)

- [ ] **Step 1: `PluginProvider::configure` pass-through.** In `plugin_provider.rs`, add a method that forwards to the guest through the `spawn_blocking` `call` helper:

```rust
    /// Configura la conexión del guest-provider (#30 stage 3c). Se llama UNA vez
    /// tras construir, antes de usarlo como `Provider`.
    ///
    /// # Errors
    /// El error lógico del guest (mapeado) o un fallo del runtime.
    pub async fn configure(
        &self,
        endpoint: String,
        user: String,
        password: String,
        base: String,
    ) -> Result<(), Error> {
        use norte_plugin_host::provider_iface::ProviderConfig;
        self.call(move |g| {
            g.configure(ProviderConfig { endpoint, user, password, base })
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }
```

- [ ] **Step 2: `ftp_plugin.rs` — the wiring helper.** Resolve the host to an IP (the guest has no DNS), build `net` caps scoped to that IP (all ports for passive FTP), instantiate the guest as a `PluginProvider`, then `configure`.

```rust
//! Wiring del provider FTP-por-plugin (#30 stage 3c): resuelve el hostname a IP
//! (el guest no tiene DNS), concede la capability `net` acotada a esa IP (todos
//! los puertos — el FTP pasivo negocia puertos de datos dinámicos), instancia el
//! guest `ftp-provider` y llama a `configure`. Reemplaza a `norte-vfs-ftp`.

use std::net::ToSocketAddrs;

use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_vfs::proto::Error;

use crate::plugin_provider::PluginProvider;

/// El `.wasm` del guest FTP, EMBEBIDO (ADR 0033): no puede ser dep de build de
/// core (el target wasm32-wasip2 puede faltar en el host de compilación). Se
/// recompila con `just build-ftp-wasm`.
const FTP_PROVIDER_WASM: &[u8] = include_bytes!("../resources/ftp-provider.wasm");

/// Resuelve `host` a una IP (v4 preferida; el guest conecta por IP). `port` solo
/// para formar el `SocketAddr` de resolución.
fn resolve_ip(host: &str, port: u16) -> Result<std::net::IpAddr, Error> {
    // spawn_blocking en el caller (regla 2): esta fn es sync pura de resolución.
    (host, port)
        .to_socket_addrs()
        .map_err(|_| Error::ProviderUnavailable { retryable: true })?
        .next()
        .map(|sa| sa.ip())
        .ok_or(Error::ProviderUnavailable { retryable: true })
}

/// Construye un `PluginProvider` FTP conectado: resuelve DNS, concede `net` a la
/// IP, instancia el guest y configura la sesión.
///
/// # Errors
/// Resolución fallida, instanciación del guest, o el `configure` del guest.
pub async fn connect_ftp_plugin(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    base: &str,
) -> Result<PluginProvider, Error> {
    let host_owned = host.to_string();
    let ip = tokio::task::spawn_blocking(move || resolve_ip(&host_owned, port))
        .await
        .map_err(|_| Error::Internal { panic: true })??;
    let endpoint = format!("{ip}:{port}");
    // net: bare-ip = todos los puertos (control + datos pasivos).
    let caps = HostCaps::with_net(vec![ip.to_string()]);

    let user = user.to_string();
    let password = password.to_string();
    let base = base.to_string();
    let ep = endpoint.clone();
    // Instanciación + configure: síncronos/bloqueantes → spawn_blocking. El
    // PluginProvider se construye desde los BYTES embebidos (from_binary).
    let provider = tokio::task::spawn_blocking(move || {
        let runtime = PluginRuntime::new().map_err(|e| crate::plugin_provider::map_runtime_error(&e))?;
        PluginProvider::from_bytes(runtime, FTP_PROVIDER_WASM, caps, "ftp")
            .map_err(|e| crate::plugin_provider::map_runtime_error(&e))
    })
    .await
    .map_err(|_| Error::Internal { panic: true })??;
    provider.configure(ep, user, password, base).await?;
    Ok(provider)
}
```

This requires two additions:
  - `PluginProvider::from_bytes(runtime, &[u8], HostCaps, scheme)` — like `new` but from an in-memory component. Add to `plugin_provider.rs`; it needs a `ProviderInstance` from bytes.
  - `ProviderInstance` from bytes → add `PluginRuntime::instantiate_provider_bytes(&self, &[u8], HostCaps)` in `runtime.rs` using `Component::from_binary(&self.engine, bytes)` inside a `prepare_bytes` twin of `prepare` (the only difference is the component source; refactor `prepare` to take an enum or a pre-built `Component`).
  - Make `map_runtime_error` `pub(crate)` in `plugin_provider.rs`.

- [ ] **Step 3: `runtime.rs` — component from bytes.** Refactor `prepare` to accept a `Component` (built by the caller) instead of a path, and add `instantiate_provider_bytes`:

```rust
    /// Como [`Self::instantiate_provider`] pero desde los BYTES de un componente
    /// en memoria (ADR 0033: el guest FTP va embebido en el binario). Aplica el
    /// MISMO sandbox y límites.
    pub fn instantiate_provider_bytes(
        &self,
        bytes: &[u8],
        caps: Capabilities,
    ) -> Result<ProviderInstance, RuntimeError> {
        use crate::bindings::provider_world::NorteProvider;
        let component = Component::from_binary(&self.engine, bytes)
            .map_err(|e| RuntimeError::Component(e.to_string()))?;
        let (mut store, linker) = self.prepare_common(caps)?;
        let bindings = NorteProvider::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ProviderInstance { store, bindings })
    }
```

Extract the store+linker construction from `prepare` into `prepare_common(caps) -> (Store, Linker)` (drop the artifact-size + `Component::from_file` part, which stays in the path-based `prepare`). The artifact-size cap does not apply to embedded bytes (trusted first-party).

- [ ] **Step 4: Add `PluginProvider::from_bytes`** in `plugin_provider.rs`, mirroring `new` but calling `instantiate_provider_bytes`.

- [ ] **Step 5: Build core.** (Will fail to link until the wasm artifact exists — Task 6 produces it. Sequence Task 6 before building.)

---

## Task 6: Build + commit the prebuilt wasm artifact; `just` recipe; ADR 0033

**Files:**
- Create: `crates/norte-core/resources/ftp-provider.wasm`
- Modify: `justfile`
- Create: `docs/adr/0033-ftp-provider-as-plugin.md`

- [ ] **Step 1: ADR.** Use the `/adr` skill (or write manually) `docs/adr/0033-ftp-provider-as-plugin.md`: context (retire `norte-vfs-ftp`, guest owns suppaftp over wasi:sockets), decision (WIT `configure`, host resolves DNS + grants net-by-IP, **embedded prebuilt `.wasm` via `include_bytes!` + `Component::from_binary`** because the target may be absent on build hosts), consequences (committed binary blob rebuilt by `just build-ftp-wasm`; FTPS deferred — `aws-lc-rs` won't target wasm; per-chunk RETR debt). Cross-reference ADR 0032 (provider WIT) and ADR 0014 (original FTP provider).

- [ ] **Step 2: `just build-ftp-wasm` recipe.**

```make
# Recompila el guest ftp-provider a wasm32-wasip2 y actualiza el artefacto
# embebido en norte-core (ADR 0033). Correr tras tocar el guest.
build-ftp-wasm:
    cargo build --release --target wasm32-wasip2 \
        --manifest-path crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml
    cp crates/norte-plugin-host/examples-wasm/ftp-provider/target/wasm32-wasip2/release/ftp_provider.wasm \
        crates/norte-core/resources/ftp-provider.wasm
```

- [ ] **Step 3: Produce the artifact.**

Run: `mkdir -p crates/norte-core/resources && just build-ftp-wasm && ls -l crates/norte-core/resources/ftp-provider.wasm`
Expected: a `.wasm` a few hundred KiB.

- [ ] **Step 4: Build core.**

Run: `cargo build -p norte-core`
Expected: PASS (the `include_bytes!` now resolves).

- [ ] **Step 5: Commit.**

```bash
git add -A
git commit -m "feat(core): #30 stage 3c — wiring ftp-por-plugin + artefacto embebido (ADR 0033)"
```

---

## Task 7: Contract test — `provider_contract!` over the guest + libunftp

**Files:**
- Create: `crates/norte-core/tests/ftp_provider_contract.rs`
- Modify: `crates/norte-core/Cargo.toml` (dev-deps already present: `libunftp`, `unftp-sbe-fs`, `tempfile`, `norte-testkit`, `sha2`; confirm)

- [ ] **Step 1: The test harness.** Linux-only (like the original ftp contract). A per-test `fresh()` factory: spawn `libunftp` on a tempdir at an ephemeral port, then `connect_ftp_plugin("127.0.0.1", port, "anonymous", "anonymous", "/")`. The macro's `factory` is evaluated once per test. Build the guest is unnecessary here — the artifact is embedded via the wiring — but the wiring loads the committed `.wasm`, so the test exercises the SHIPPED artifact. Since `provider_contract!` cannot SKIP, this test hard-requires nothing at runtime except the embedded wasm (always present) + Linux.

```rust
//! `provider_contract!` sobre el guest FTP-por-plugin (#30 stage 3c) contra un
//! servidor `libunftp` IN-PROCESS — la MISMA suite que pasaban `MemProvider`,
//! `SftpProvider` y el difunto `norte-vfs-ftp`, ahora sobre el provider en wasm.
//! Usa el artefacto `.wasm` EMBEBIDO (ADR 0033), así que valida lo que se
//! envía. Solo-Linux (libunftp mapea sobre el FS del host, que debe ser POSIX).
#![cfg(target_os = "linux")]

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use norte_core::ftp_plugin::connect_ftp_plugin;
use norte_core::plugin_provider::PluginProvider;
use norte_proto::{Authority, Scheme, VPath};

fn spawn_libunftp(home: PathBuf) -> u16 {
    // Idéntico al helper de norte-vfs-ftp/tests/common: bind-then-drop para el
    // puerto, un hilo con su runtime tokio corriendo el server, espera a listen.
    // (Copiar el cuerpo de crates/norte-plugin-host/tests/ftp_plugin_e2e.rs
    //  `spawn_ftp_server`.)
    todo!("copiar spawn_ftp_server de ftp_plugin_e2e.rs")
}

async fn fresh() -> PluginProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = spawn_libunftp(dir.path().to_path_buf());
    std::mem::forget(dir); // vive tanto como el test
    connect_ftp_plugin("127.0.0.1", port, "anonymous", "anonymous", "/")
        .await
        .expect("provider ftp-por-plugin conectado")
}

fn ftp_root() -> VPath {
    // El PluginProvider asume root scheme-only (sin authority): ver segments().
    VPath::root(Scheme::new("ftp").expect("scheme ftp"), None)
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names().into_iter().map(|n| n.bytes).collect()
}

norte_vfs::provider_contract! {
    mod ftp_plugin_inproc,
    factory: fresh().await,
    root: ftp_root(),
    hostile_names: hostile_names(),
}
```

**Root caveat:** `PluginProvider::segments` `debug_assert!`s that `p.authority().is_none()` (root is scheme-only). The original `norte-vfs-ftp` contract used `FtpProvider::root(Authority::new("test:21"))` — an authority-bearing root. The plugin adapter expects scheme-only. Use `VPath::root(Scheme, None)`. Verify the contract's `child`/`join` work on a scheme-only root (they do — `MemProvider::root()` is scheme-only and passes the same macro).

- [ ] **Step 2: Fill in `spawn_libunftp`** by copying the body of `spawn_ftp_server` from `crates/norte-plugin-host/tests/ftp_plugin_e2e.rs`.

- [ ] **Step 3: Run the contract.**

Run: `cargo nextest run -p norte-core --test ftp_provider_contract`
Expected: all ~24 contract tests PASS. Resume/digest/symlink/trash/node_id cases self-skip (the WIT interface exposes none of those; the adapter returns `Unsupported`/defaults, and those cases auto-skip on missing capability or `Ok(None)`).

- [ ] **Step 4: Debug failures.** Likely spots: (a) `read` per-chunk RETR draining; (b) MLSD name extraction for libunftp (libunftp advertises MLSD → the `split_once(' ')` raw-name path runs); (c) `stat` of root vs base; (d) hostile names that FTP rejects cleanly (the contract skips `InvalidPath`/`Conflict` on commit). Fix in the guest, re-run `just build-ftp-wasm`, re-test.

- [ ] **Step 5: Commit.**

```bash
git add -A
git commit -m "test(core): #30 stage 3c — provider_contract! del ftp-por-plugin vs libunftp"
```

---

## Task 8: Switch `connect.rs` to the plugin path; retire `norte-vfs-ftp`

**Files:**
- Modify: `crates/norte-core/src/connect.rs`
- Modify: workspace `Cargo.toml` (drop member + dep)
- Modify: `crates/norte-core/Cargo.toml` (drop `norte-vfs-ftp` dep if direct)
- Modify: `crates/norte-connect/*` — decide FTP connector fate (see below)
- Delete: `crates/norte-vfs-ftp/`
- Modify: `ARCHITECTURE.md`, `docs/spec/norte-spec.md`, `justfile`

- [ ] **Step 1: Rewrite the `"ftp"` arm in `establish()`.** Replace the `FtpProvider::with_reader` construction with `connect_ftp_plugin`. Credentials come from the same `secret`/`spec.auth` logic already in `establish`; the TLS negotiation (`FtpConnector`) is GONE (FTPS deferred → plaintext only). Extract `host`/`port`/`user` from `ep`; the password from `secret` (or `anonymous` for `AuthMethod::Agent`). Preserve the `#44` degradation warning? No — there is no TLS now, so no `tls_degraded`. Emit a one-time warning that FTP-via-plugin is plaintext-only (FTPS deferred). Keep `logical_trash` unsupported (the WIT interface has no trash — the adapter already returns `Unsupported`).

```rust
            "ftp" => {
                let user = ep.user.clone().unwrap_or_else(|| "anonymous".to_string());
                let password = match (&spec.auth, &secret) {
                    (AuthMethod::Password, Some(s)) => s.expose().to_string(),
                    (AuthMethod::Agent, _) => "anonymous".to_string(),
                    (AuthMethod::Password, None) => return Err(Error::PermissionDenied),
                    (AuthMethod::Key | AuthMethod::AccessKey, _) => return Err(Error::Unsupported),
                };
                let port = ep.port.unwrap_or(21);
                warnings.push(ConnectionWarning {
                    scheme: ep.scheme.clone(),
                    host: ep.host.clone(),
                    reason: ConnectionWarningReason::FtpPlaintext, // NUEVO variant
                });
                let provider = crate::ftp_plugin::connect_ftp_plugin(
                    &ep.host, port, &user, &password, "/",
                ).await?;
                Ok(Connected { provider: Arc::new(provider), warnings })
            }
```

Add `ConnectionWarningReason::FtpPlaintext` (+ its `reason()` str `"ftp-plaintext"`) in `connect.rs`. Remove the `use norte_vfs_ftp::FtpProvider;` import and the `ftp: FtpConnector` field usage in the `"ftp"` arm.

- [ ] **Step 2: Decide `FtpConnector`'s fate.** It handled TLS + login for the old provider. Now the guest logs in and there is no TLS. Options: (a) keep `FtpConnector` unused-but-available for a future FTPS host-side path; (b) remove its use from `ConnectionManager` (drop the `ftp: FtpConnector` field + `FtpConnector::new()`), leaving the crate's `FtpConnector` type in place for later. Choose (b): drop the field, keep the type in `norte-connect` (documented as "reserved for host-side FTPS, deferred"). This avoids dead-code warnings in core. If `norte-connect` then has unused FTP code, gate it or leave with a `#[allow]`-free doc note (it is a `pub` library item — no dead-code warning).

- [ ] **Step 3: Delete the crate + references.**

```bash
git rm -r crates/norte-vfs-ftp
```
Edit workspace `Cargo.toml`: remove `"crates/norte-vfs-ftp",` from `members` and the `norte-vfs-ftp = { ... }` dependency line. Edit `crates/norte-core/Cargo.toml`: remove the `norte-vfs-ftp` dependency. Edit `justfile`: remove the `cargo nextest run -p norte-vfs-ftp --features it-ftp` nightly line.

- [ ] **Step 4: Update docs.** In `ARCHITECTURE.md` and `docs/spec/norte-spec.md`, change the FTP provider description from a native crate to a first-party WASM plugin (cite ADR 0033). Grep: `grep -rn "norte-vfs-ftp\|vfs-ftp" ARCHITECTURE.md docs/`.

- [ ] **Step 5: Full build + clippy.**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS. Fix leftover imports/refs.

- [ ] **Step 6: Commit.**

```bash
git add -A
git commit -m "refactor(core): #30 stage 3c — ftp por plugin en connect; retira norte-vfs-ftp"
```

---

## Task 9: Full CI + reviewers

- [ ] **Step 1: `just ci`.**

Run: `just ci`
Expected: EXIT=0. The new contract test runs (Linux + embedded wasm). Fix coverage/format/deny issues.

- [ ] **Step 2: Reviewers.** Per CLAUDE.md, WIT changes touch the plugin protocol (ADR 0032 → protocol-guardian) and this is security-sensitive (plugin sandbox, net capability, credential crossing, CR/LF injection). Dispatch:
  - **protocol-guardian** — the WIT `0.3.0 → 0.4.0` bump, `configure` addition, N-1 window.
  - **security-reviewer** — net-by-IP grant scope, DNS resolution (SSRF/link-local? the old `dial` had a PASV NAT workaround + anti-hijack; the guest connects by the host-resolved IP — confirm no metadata-IP escape; the old code deferred link-local deny-list to wiring, `runtime.rs:336` — CHECK whether resolved IP should be screened against 169.254/fe80/loopback-for-non-loopback), credential-in-guest-memory, CR/LF defenses preserved.
  - **rust-reviewer** — the `spawn_blocking` usage, `Component::from_binary`, `RefCell` writer state, error mapping.
  - **encoding-auditor** — raw-byte name round-trip through the WIT boundary, MLSD lossy `U+FFFD` handling, `≤255` byte cap, the hostile-name corpus passing.

- [ ] **Step 3: Apply review feedback** (use `superpowers:receiving-code-review`), re-run `just ci`, commit fixes.

- [ ] **Step 4: Finish the branch** with `superpowers:finishing-a-development-branch`.

---

## Self-review notes

- **Spec coverage:** all six requested pieces map to tasks — ops (T2–T4), CR/LF defenses (T2 `remote`/`configure`), WIT `configure` (T1), host wiring resolve+grant+configure (T5), `provider_contract!` vs libunftp (T7), retire `norte-vfs-ftp` (T8). FTPS-deferred documented (T6 ADR, T8 warning).
- **Security screening gap flagged:** the old provider deferred link-local/metadata deny-listing to "stage 3b wiring" (`runtime.rs:336`). T5 resolves DNS host-side; T9 security review must decide whether `resolve_ip` screens `169.254.0.0/16`, `fe80::/10`, and loopback (SSRF hardening). If required, add the deny-list to `resolve_ip` before the net grant.
- **suppaftp sync API risk:** T2/T4 assume v6 sync method names match the async originals. If they differ, adapt at build time (T4 step 2). The `ftp-probe` proof already compiled `suppaftp` sync `connect`/`login`/`transfer_type`/`list` to wasm — the surface exists.
- **Contract root:** scheme-only root (`None` authority) required by `PluginProvider::segments`; do NOT reuse the old authority-bearing root.
- **Placeholder scan:** the `# placeholder` in T2 step 3 is explicitly resolved by T3–T4 (write the file whole before compiling).
