# FTP Provider as WASM Plugin (issue #30, final stage) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the complete FTP provider into a WASM guest plugin (all ops over sync suppaftp + the CR/LF anti-injection defenses), add a `configure` WIT function, wire the host (hostname→IP resolution, scoped `net` grant, config hand-off), pass `provider_contract!` against in-process libunftp, and retire `norte-vfs-ftp`. FTPS becomes tracked debt (aws-lc-rs does not compile to wasm).

**Architecture:** A new standalone guest crate `examples-wasm/provider-ftp` implements WIT world `norte-provider` with two sync FTP control connections (main + dedicated reader, preserving the issue #39 B1 design) and a cached in-flight RETR stream so the host's chunked `read` calls don't reopen the transfer per chunk. The WIT `provider` interface bumps 0.3.0→0.4.0: adds `configure(config: list<u8>)` (opaque, plugin-specific) and projects `case-sensitive`/`case-preserving` capabilities (needed to pass the contract's case tests). Host side: `ProviderInstance::configure`, `PluginProvider` grows a config parameter + capability mapping, `ConnectionManager`'s `ftp` branch resolves the hostname to an IP (deny-list: link-local/metadata), looks up an approved+enabled provider plugin for scheme `ftp` in the `PluginRegistry`, grants `net` scoped to the resolved IP only, and passes credentials via `configure`. `norte-vfs-ftp` and `norte-connect`'s `FtpConnector` are deleted.

**Tech Stack:** wasmtime Component Model (existing `norte-plugin-host`), suppaftp **sync** compiled to `wasm32-wasip2` (proven by the stage-3b `ftp-probe` guest), libunftp in-process test server, `provider_contract!` (extended with an optional `skip_if:`).

**Security posture (review focus):**
- CR/LF/NUL rejection per path segment AND on `user`/`pass` (USER/PASS are line commands too — new defense vs the old provider, where login lived host-side).
- `net` allow-list = the single resolved control IP (all ports — PASV requires it). PASV-hijack defense: `set_passive_nat_workaround(true)` in the guest + the wasmtime `socket_addr_check` blocks any data connection to a non-allow-listed IP.
- Hostname resolution host-side, deny-list for link-local/metadata (169.254.0.0/16, fe80::/10, multicast, unspecified, broadcast).
- The plugin sees the FTP password (it must log in). This is inherent to a provider plugin; the human approved the plugin (fail-closed governance) and the grant is per-connection. Document, don't hide.
- TLS policy: `tls = "require"` (default) fails closed with a clear log (FTPS unsupported by the plugin — debt issue). `allow` degrades with a `ConnectionWarning`. `plain` proceeds with the existing cleartext warning.

---

## Task 1: De-risk — probe the suppaftp sync API surface on wasm32-wasip2

The whole port assumes the sync API mirrors the async one. `ftp-probe` pinned `suppaftp = "6"` (default-features off); the workspace async provider used `"10"`. Verify which version compiles to wasm AND has: `feat`, `opts`, `mlsd`, `mlst`, `list`, `resume_transfer`, `retr_as_stream`/`finalize_retr_stream`, `put_with_stream`/`finalize_put_stream`, `append_with_stream`, `size`, `mkdir`/`rmdir`/`rm`/`rename`, `set_passive_nat_workaround`, `ListParser::{parse_posix,parse_dos,parse_mlsd,parse_mlst}`, `File::{is_file,is_directory,is_symlink,name,size}`, `FtpError`/`Status` shapes.

**Files:**
- Create: `crates/norte-plugin-host/examples-wasm/provider-ftp/Cargo.toml`
- Create: `crates/norte-plugin-host/examples-wasm/provider-ftp/src/lib.rs` (skeleton)
- Create: `crates/norte-plugin-host/examples-wasm/provider-ftp/wit/norte-plugin.wit` (copy of current 0.3.0 for now)

- [ ] **Step 1: Scaffold the guest crate** (standalone, NOT a workspace member — same as `ftp-probe`):

```toml
# Guest WASM (#30 final stage): el provider FTP COMPLETO como plugin sobre
# wasi:sockets gateada. suppaftp SYNC sin TLS (aws-lc-rs no compila a wasm;
# FTPS = deuda). NO es miembro del workspace.
[workspace]

[package]
name = "provider-ftp"
edition = "2021"
version = "0.0.0"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.46"
suppaftp = { version = "6", default-features = false }

[profile.release]
panic = "abort"
opt-level = "s"
```

Copy `crates/norte-plugin-host/wit/norte-plugin.wit` into `provider-ftp/wit/`. Skeleton `src/lib.rs`: the probe below.

- [ ] **Step 2: Probe compile.** Write a temporary `src/lib.rs` that references every API listed above (a `#[allow(dead_code)] fn probe()` calling each method on a `FtpStream`, plus the `ListParser`/`File` items), with a minimal `Guest` impl stubbed as `todo!()`-free `Err(VfsError::Unsupported)` bodies. Run:

```bash
cd crates/norte-plugin-host/examples-wasm/provider-ftp
cargo build --release --target wasm32-wasip2
```

Expected: compiles. If an API is missing on v6, try `suppaftp = "10"` (or the highest that compiles to wasm) — prefer the version whose sync API matches the async provider 1:1. Record the chosen version and any API renames in a comment in `Cargo.toml`; adjust Task 5's code accordingly. If `set_passive_nat_workaround` is absent: omit it — the `socket_addr_check` still blocks off-allow-list PASV targets (fail-closed), note it in the lib.rs docs.

- [ ] **Step 3: Commit** (`feat(plugin-host): #30 port FTP — scaffold guest provider-ftp + probe API suppaftp sync`).

---

## Task 2: WIT 0.4.0 — `configure` + projected case capabilities; host bindings

**Files:**
- Modify: `crates/norte-plugin-host/wit/norte-plugin.wit`
- Modify: `crates/norte-plugin-host/src/runtime.rs` (ProviderInstance::configure, RuntimeError::Configure)
- Modify: `crates/norte-plugin-host/examples-wasm/provider-mem/src/lib.rs` + `wit/`
- Modify: `crates/norte-plugin-host/examples-wasm/provider-mem-rw/src/lib.rs` + `wit/`
- Modify: all other `examples-wasm/*/wit/norte-plugin.wit` copies (previewer-demo, command-demo, previewer-syntect, net-probe, provider-ftp; `ftp-probe` gets deleted in Task 9 — update it only if Task 9 hasn't run)
- Modify: `crates/norte-plugin-host/tests/provider_e2e.rs`

- [ ] **Step 1: WIT changes.** In `wit/norte-plugin.wit`: bump header comment + `package norte:plugin@0.4.0;`. Extend `caps`:

```wit
    /// Capabilities proyectadas. `read-only` desde stage 2; `case-sensitive`/
    /// `case-preserving` (#30 stage 4): sin ellas el host no puede declarar
    /// flags de caja honestos y la suite contractual de caja no aplica.
    record caps {
        read-only: bool,
        /// Nombres que difieren solo en caja son entradas DISTINTAS.
        case-sensitive: bool,
        /// El remoto conserva la caja tal cual se escribió.
        case-preserving: bool,
    }
```

Add to `interface provider` (before `capabilities`):

```wit
    /// Configura la instancia ANTES de operar (#30 stage 4). `config` es un
    /// blob OPACO propio de cada plugin (p. ej. endpoint+credenciales del FTP);
    /// el host lo construye en su wiring y NO forma parte del contrato WIT.
    /// Sin `configure` (o tras un Err) las operaciones devuelven
    /// `provider-unavailable`. Reconfigurar una instancia viva no está
    /// soportado (`unsupported`).
    configure: func(config: list<u8>) -> result<_, vfs-error>;
```

Update the version-history comment: 0.3.0→0.4.0 is BREAKING for provider guests (new export + record fields), safe because `provider` is pre-release with no published guests (same rationale as 0.2→0.3). Copy the updated file to every guest `wit/` dir.

- [ ] **Step 2: Host bindings.** `runtime.rs`: new `RuntimeError` variant:

```rust
    /// El guest RECHAZÓ `configure` (error lógico del provider: endpoint
    /// inalcanzable, credenciales inválidas…). Se conserva el `vfs-error` para
    /// que el adapter lo traduzca fiel (p. ej. permission-denied del login).
    #[error("configure del provider rechazado: {0:?}")]
    Configure(provider_iface::VfsError),
```

(the variant references `provider_iface`, already `pub use`d in this crate) and on `ProviderInstance`:

```rust
    /// Configura el guest (#30 stage 4) con un blob opaco propio del plugin.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa; [`RuntimeError::Configure`]
    /// si el guest rechaza la configuración.
    pub fn configure(&mut self, config: &[u8]) -> Result<(), RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_configure(&mut self.store, config)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?
            .map_err(RuntimeError::Configure)
    }
```

- [ ] **Step 3: Update mem guests.** Both `provider-mem` and `provider-mem-rw` `Guest` impls gain:

```rust
    fn configure(_config: Vec<u8>) -> Result<(), VfsError> {
        Ok(()) // sin estado que configurar: el árbol es fijo
    }
```

and their `capabilities()` return `Caps { read_only: <as today>, case_sensitive: true, case_preserving: true }` (byte-keyed HashMap = case-sensitive & preserving).

- [ ] **Step 4: Update `provider_e2e.rs`** — after `capabilities()` assert, add `assert!(caps.case_sensitive && caps.case_preserving)`; call `inst.configure(b"ignored").expect("configure no-op")` before the ops.

- [ ] **Step 5: Build + run:**

```bash
cargo build -p norte-plugin-host
cargo nextest run -p norte-plugin-host
```

Expected: green (wasm-target tests skip or pass).

- [ ] **Step 6: Commit** (`feat(plugin-host): #30 WIT provider 0.4.0 — configure + caps de caja`).

---

## Task 3: `PluginProvider` — config param, caps mapping, authority-rooted paths

**Files:**
- Modify: `crates/norte-core/src/plugin_provider.rs`
- Modify: `crates/norte-core/tests/plugin_provider_e2e.rs:71` and `crates/norte-core/tests/plugin_provider_rw_e2e.rs:42` (call-sites)

- [ ] **Step 1:** `PluginProvider::new` signature → `new(runtime, wasm, host_caps, scheme, config: Option<&[u8]>)`. After `instantiate_provider`, before `capabilities`:

```rust
        if let Some(cfg) = config {
            inst.configure(cfg)?;
        }
        let guest = inst.capabilities()?;
        let mut flags = CapabilityFlags::empty();
        if guest.read_only {
            flags |= CapabilityFlags::READ_ONLY;
        }
        if guest.case_sensitive {
            flags |= CapabilityFlags::CASE_SENSITIVE;
        }
        if guest.case_preserving {
            flags |= CapabilityFlags::CASE_PRESERVING;
        }
```

- [ ] **Step 2:** Remove the `debug_assert!(p.authority().is_none(), ...)` in `segments()`. Replace with a doc comment: the provider is bound to ONE connection; the authority in the `VPath` is the connection identity and does not project into the guest (segments are the path). Make `map_vfs_error` `pub(crate)` (the connect wiring will map `RuntimeError::Configure` faithfully).

- [ ] **Step 3:** Update both test call-sites to pass `None`. Add the encoder for the FTP plugin config (shared by wiring, contract test, and e2e — versioned host↔guest convention, NOT wire):

```rust
/// Codifica la configuración del plugin-provider FTP (#30 stage 4): blob
/// opaco `NFTP1` + 4 campos u32-LE longitud-prefijados (addr `ip:puerto`,
/// user, pass, base). Convención host↔guest del plugin FTP de norte,
/// versionada por el magic; NO es formato de wire del protocolo.
#[must_use]
pub fn ftp_plugin_config(addr: &str, user: &str, pass: &str, base: &str) -> Vec<u8> {
    let mut out = Vec::from(&b"NFTP1"[..]);
    for field in [addr, user, pass, base] {
        out.extend_from_slice(&(field.len() as u32).to_le_bytes());
        out.extend_from_slice(field.as_bytes());
    }
    out
}
```

(in `plugin_provider.rs`, `pub`; unit test: round-trip length/prefix layout.)

- [ ] **Step 4:** `cargo nextest run -p norte-core` → green. Commit (`feat(core): #30 PluginProvider — configure + mapping de caps + raíz con authority`).

---

## Task 4: `provider_contract!` — optional `skip_if:`

The plugin contract needs the literal macro but must SKIP when `wasm32-wasip2` isn't installed (repo convention: wasm e2e skip, `just ci` stays green anywhere).

**Files:**
- Modify: `crates/norte-vfs/src/contract.rs`

- [ ] **Step 1:** Add a forwarding arm (old arm delegates with `skip_if: false`) and thread the guard into EVERY generated test as its first statement:

```rust
    (
        mod $name:ident,
        factory: $factory:expr,
        root: $root:expr,
        hostile_names: $hostile:expr $(,)?
    ) => {
        $crate::provider_contract! {
            mod $name,
            factory: $factory,
            root: $root,
            hostile_names: $hostile,
            skip_if: false,
        }
    };
    (
        mod $name:ident,
        factory: $factory:expr,
        root: $root:expr,
        hostile_names: $hostile:expr,
        skip_if: $skip:expr $(,)?
    ) => { /* cuerpo actual */ }
```

Inside the body, define once:

```rust
            /// Precondición de ENTORNO del invocante (p. ej. target wasm
            /// instalado). `true` = el test se salta con aviso.
            macro_rules! skip_if_env {
                () => {
                    if $skip {
                        eprintln!("skip: precondición de entorno del contrato no satisfecha");
                        return;
                    }
                };
            }
```

and insert `skip_if_env!();` as the first line of every `#[tokio::test]` fn (before `let p = $factory;` — the factory must not run when skipping). Mechanical: ~30 insertions.

- [ ] **Step 2:** `cargo nextest run -p norte-testkit -p norte-vfs` (Mem contract still green through the forwarding arm), `cargo nextest run -p norte-vfs-sftp -p norte-vfs-object` if fast enough (they invoke the macro). Expected: green.

- [ ] **Step 3:** Commit (`feat(vfs): provider_contract! acepta skip_if de entorno`).

---

## Task 5: The guest — full FTP provider port

Direct port of `crates/norte-vfs-ftp/src/provider.rs` to sync, with the dual-connection design (main + reader) and a cached RETR stream. Read `provider.rs` side-by-side while porting — every defense comment travels with its code.

**Files:**
- Rewrite: `crates/norte-plugin-host/examples-wasm/provider-ftp/src/lib.rs`

- [ ] **Step 1: Write the guest.** Full source (adjust API names per Task 1's findings):

```rust
//! Guest WASM (#30 stage final): el provider FTP COMPLETO como plugin sobre
//! `wasi:sockets` gateada — el port del antiguo `norte-vfs-ftp` (ADR 0014) a
//! suppaftp SYNC. Conserva TODAS las defensas del provider host:
//!
//! - Anti-inyección FTP (protocolo de LÍNEAS): CR/LF/NUL se rechazan por
//!   SEGMENTO, en la `base`, y en `user`/`pass` (USER/PASS también son
//!   comandos de línea — defensa nueva: el login ahora vive en el guest).
//! - Bytes crudos (regla 1): un nombre no-UTF8 es rechazo LIMPIO
//!   (`invalid-path`); un nombre listado con U+FFFD o `/` corta el listado.
//! - `.` / `..` / `/` por segmento: jamás se escapa la base. NAME_MAX 255.
//! - Anti PASV-hijack: datos SIEMPRE a la IP del canal de control
//!   (`set_passive_nat_workaround`) — que es la ÚNICA IP del allow-list `net`
//!   del host: una IP PASV ajena la corta el `socket_addr_check` de wasmtime.
//!
//! DOS conexiones de control (issue #39 B1): `main` para stat/list/escrituras,
//! `reader` DEDICADA a RETR — una copia FTP→FTP mismo host intercala APPE y
//! chunks de lectura sin reabrir el RETR por chunk (el stream RETR en vuelo se
//! CACHEA entre llamadas `read` secuenciales; un acceso no secuencial lo drena
//! y reabre — equivalente sync del resync #39 M1).
//!
//! FTPS: NO (aws-lc-rs no compila a wasm) — deuda; el wiring host falla
//! cerrado con `tls = "require"`.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};

use suppaftp::list::{File, ListParser};
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, Status};

wit_bindgen::generate!({
    world: "norte-provider",
    path: "wit",
});

use exports::norte::plugin::provider::{
    Caps, Entry, EntryKind, Guest, GuestWriter, Page, VfsError, Writer,
};
use norte::plugin::host_log;

/// Cota defensiva de entradas materializadas por listado (issue #40).
const MAX_LIST_ENTRIES: usize = 1 << 20;
/// Prefijo del staging de escritura (ADR 0012, mismo convenio que local/sftp).
const PARTIAL_PREFIX: &str = ".norte-partial.";
/// Magic del blob de configuración (convención host↔guest, ver
/// `norte_core::plugin_provider::ftp_plugin_config`).
const CONFIG_MAGIC: &[u8] = b"NFTP1";
/// Tope de bytes servidos por llamada `read` (el host pide 64 KiB).
const MAX_READ_CHUNK: usize = 256 * 1024;

type Segs = Vec<Vec<u8>>;

/// RETR en vuelo cacheado entre llamadas `read` secuenciales.
struct ActiveRead {
    segs: Segs,
    next_offset: u64,
    reader: Box<dyn Read>,
}

struct State {
    main: FtpStream,
    reader: FtpStream,
    /// Raíz remota absoluta. Sin `..`, sin barra final, sin CR/LF/NUL.
    base: String,
    has_mlsd: bool,
    /// Contador de staging (nombre efímero único para `open-writer`).
    seq: u64,
    active: Option<ActiveRead>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Ejecuta `f` sobre el estado configurado; sin `configure` previo =
/// `provider-unavailable`.
fn with_state<T>(f: impl FnOnce(&mut State) -> Result<T, VfsError>) -> Result<T, VfsError> {
    STATE.with_borrow_mut(|slot| match slot.as_mut() {
        Some(st) => f(st),
        None => Err(VfsError::ProviderUnavailable),
    })
}

/// Decodifica el blob de configuración: magic + 4 campos u32-LE
/// longitud-prefijados (addr, user, pass, base). Malformado = invalid-path.
fn decode_config(cfg: &[u8]) -> Result<(String, String, String, String), VfsError> {
    let mut cur = cfg.strip_prefix(CONFIG_MAGIC).ok_or(VfsError::InvalidPath)?;
    let mut fields: Vec<String> = Vec::with_capacity(4);
    for _ in 0..4 {
        if cur.len() < 4 {
            return Err(VfsError::InvalidPath);
        }
        let (lenb, tail) = cur.split_at(4);
        // El slice es de 4 bytes exactos: el try_into no puede fallar.
        let len = u32::from_le_bytes(lenb.try_into().unwrap()) as usize;
        if tail.len() < len {
            return Err(VfsError::InvalidPath);
        }
        let (val, tail) = tail.split_at(len);
        fields.push(String::from_utf8(val.to_vec()).map_err(|_| VfsError::InvalidPath)?);
        cur = tail;
    }
    if !cur.is_empty() {
        return Err(VfsError::InvalidPath);
    }
    let base = fields.pop().unwrap();
    let pass = fields.pop().unwrap();
    let user = fields.pop().unwrap();
    let addr = fields.pop().unwrap();
    Ok((addr, user, pass, base))
}

/// Mapea el error de suppaftp a la taxonomía proyectada (port del map_err del
/// provider host, spec §17.7). El comodín cubre variantes que difieren entre
/// versiones de suppaftp (BadResponse, SecureError…): Io no-reintentable.
fn map_err(e: &FtpError) -> VfsError {
    match e {
        FtpError::UnexpectedResponse(r) => match r.status {
            // 550 es ambiguo en FTP (no existe / sin permiso): NotFound es el
            // caso común y el que el contrato espera para paths ausentes.
            Status::FileUnavailable => VfsError::NotFound,
            Status::NotLoggedIn => VfsError::PermissionDenied,
            Status::BadFilename => VfsError::InvalidPath,
            _ => VfsError::Io,
        },
        FtpError::ConnectionError(_) => VfsError::ProviderUnavailable,
        FtpError::InvalidAddress(_) => VfsError::InvalidPath,
        _ => VfsError::Io,
    }
}

/// Un fallo de LOGIN: cualquier respuesta inesperada es credenciales
/// rechazadas (permission-denied), sin interpolar el body (lo controla el
/// servidor; regla 10 — aquí ni siquiera hay logs con él).
fn login_err(e: &FtpError) -> VfsError {
    match e {
        FtpError::UnexpectedResponse(_) => VfsError::PermissionDenied,
        other => map_err(other),
    }
}

/// Prepara una conexión recién logueada: BINARIO (ASCII corrompe binarios),
/// detección MLSD/MLST y `OPTS UTF8 ON` best-effort (RFC 2640, ADR 0014 D2).
fn setup_conn(ftp: &mut FtpStream) -> Result<bool, VfsError> {
    ftp.transfer_type(FileType::Binary).map_err(|e| map_err(&e))?;
    let feats = ftp.feat().ok();
    let has_mlsd = feats.as_ref().is_some_and(|f| {
        f.keys()
            .any(|k| k.eq_ignore_ascii_case("MLST") || k.eq_ignore_ascii_case("MLSD"))
    });
    if feats
        .as_ref()
        .is_some_and(|f| f.keys().any(|k| k.eq_ignore_ascii_case("UTF8")))
    {
        let _ = ftp.opts("UTF8", Some("ON"));
    }
    Ok(has_mlsd)
}

/// Conecta + loguea + prepara UNA conexión de control.
fn dial(addr: &str, user: &str, pass: &str) -> Result<(FtpStream, bool), VfsError> {
    let mut ftp = FtpStream::connect(addr).map_err(|e| map_err(&e))?;
    // Anti PASV-hijack (como `curl --ftp-skip-pasv-ip`): la conexión de datos
    // va SIEMPRE a la IP del canal de control — la única del allow-list.
    ftp.set_passive_nat_workaround(true);
    ftp.login(user, pass).map_err(|e| login_err(&e))?;
    let has_mlsd = setup_conn(&mut ftp)?;
    Ok((ftp, has_mlsd))
}

/// Traduce los segmentos al path remoto absoluto bajo la `base`. Los segmentos
/// son BYTES; FTP (vía suppaftp) exige UTF-8 — no representable = rechazo
/// LIMPIO (regla 1, ADR 0014 D2). El path se construye SIEMPRE así, jamás
/// desde uno ecoado por el servidor.
fn remote(st: &State, segs: &[Vec<u8>]) -> Result<String, VfsError> {
    let mut out = String::from(&st.base);
    for seg in segs {
        let name = std::str::from_utf8(seg).map_err(|_| VfsError::InvalidPath)?;
        if name.contains('/') || name == "." || name == ".." {
            return Err(VfsError::InvalidPath);
        }
        // FTP es un protocolo de LÍNEAS: CR/LF en un nombre inyectaría un
        // comando arbitrario (`STOR x\r\nDELE víctima`). NUL también fuera
        // (defensa en profundidad: el host ya lo filtra vía Segment).
        if name.contains(['\r', '\n', '\0']) {
            return Err(VfsError::InvalidPath);
        }
        // NAME_MAX: >255 bytes fallaría a media operación con un 550 ambiguo.
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

/// El path remoto del DIRECTORIO padre.
fn remote_parent(st: &State, segs: &[Vec<u8>]) -> Result<String, VfsError> {
    let (_, parent) = segs.split_last().ok_or(VfsError::InvalidPath)?;
    remote(st, parent)
}

/// Parsea una línea de `LIST` (`ls -l` POSIX, con respaldo DOS). `None` para
/// líneas no parseables (`total N`…): se descartan.
fn parse_list_line(line: &str) -> Option<File> {
    ListParser::parse_posix(line)
        .ok()
        .or_else(|| ListParser::parse_dos(line).ok())
}

/// `stat` de `remote_path` sobre la conexión `ftp` (main). Con MLSD: MLST
/// directo. Sin MLSD: LIST del padre + búsqueda por nombre (pure-ftpd, ADR
/// 0014 C). `None` = no existe. Port literal del provider host.
fn stat_remote(
    ftp: &mut FtpStream,
    remote_path: &str,
    has_mlsd: bool,
    base: &str,
) -> Result<Option<File>, VfsError> {
    if has_mlsd {
        return match ftp.mlst(Some(remote_path)) {
            Ok(line) => {
                let f = ListParser::parse_mlst(&line).map_err(|_| VfsError::Io)?;
                Ok(Some(f))
            }
            Err(e) => match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            },
        };
    }
    // Contención: la rama LIST lista el PADRE; para la raíz (== base) el padre
    // quedaría FUERA de la base — jamás se lista por encima. Fail-safe: None.
    if remote_path == base {
        return Ok(None);
    }
    let (parent, child) = match remote_path.rfind('/') {
        Some(0) => ("/", &remote_path[1..]),
        Some(i) => (&remote_path[..i], &remote_path[i + 1..]),
        None => return Ok(None),
    };
    if child.is_empty() {
        return Ok(None);
    }
    let lines = match ftp.list(Some(parent)) {
        Ok(l) => l,
        Err(e) => {
            return match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            };
        }
    };
    for line in lines {
        let Some(f) = parse_list_line(&line) else {
            continue;
        };
        // Nombre lossy (U+FFFD) jamás casa fiable: se salta, no se compara.
        let n = f.name();
        if n.contains('\u{FFFD}') {
            continue;
        }
        if n == child {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

fn exists(
    ftp: &mut FtpStream,
    remote_path: &str,
    has_mlsd: bool,
    base: &str,
) -> Result<bool, VfsError> {
    Ok(stat_remote(ftp, remote_path, has_mlsd, base)?.is_some())
}

/// Crea `remote_path` como fichero VACÍO (STOR sin datos): base para APPE-ar.
/// FTP no tiene O_EXCL; divergencia consciente documentada en el provider
/// original (nombre con seq, sin symlinks en FTP, destino final sí comprobado).
fn create_empty(ftp: &mut FtpStream, remote_path: &str) -> Result<(), VfsError> {
    let data = ftp.put_with_stream(remote_path).map_err(|e| map_err(&e))?;
    ftp.finalize_put_stream(data).map_err(|e| map_err(&e))
}

/// Drena y finaliza el RETR cacheado (si lo hay): deja la conexión `reader`
/// limpia para el siguiente uso (equivalente sync del resync #39 M1).
fn drain_active(st: &mut State) {
    if let Some(mut a) = st.active.take() {
        let mut scratch = [0u8; 8192];
        loop {
            match a.reader.read(&mut scratch) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = st.reader.finalize_retr_stream(a.reader);
    }
}

fn entry_of(name: Vec<u8>, f: &File) -> Entry {
    let kind = if f.is_symlink() {
        EntryKind::Symlink
    } else if f.is_directory() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let size = (kind == EntryKind::File).then(|| f.size() as u64);
    Entry { name, kind, size }
}

struct FtpPlugin;

impl Guest for FtpPlugin {
    fn configure(config: Vec<u8>) -> Result<(), VfsError> {
        STATE.with_borrow_mut(|slot| {
            if slot.is_some() {
                // Reconfigurar una instancia viva no está soportado.
                return Err(VfsError::Unsupported);
            }
            let (addr, user, pass, base) = decode_config(&config)?;
            // USER/PASS son comandos de LÍNEA: CR/LF/NUL inyectarían comandos.
            if user.contains(['\r', '\n', '\0']) || pass.contains(['\r', '\n', '\0']) {
                return Err(VfsError::InvalidPath);
            }
            let mut base = base;
            while base.len() > 1 && base.ends_with('/') {
                base.pop();
            }
            if !base.starts_with('/') || base.contains(['\r', '\n', '\0']) {
                return Err(VfsError::InvalidPath);
            }
            // DOS conexiones (#39 B1): main + reader dedicada a RETR. El
            // has_mlsd manda el de la principal (mismo servidor).
            let (main, has_mlsd) = dial(&addr, &user, &pass)?;
            let (reader, _) = dial(&addr, &user, &pass)?;
            host_log::log("provider-ftp: configurado (2 conexiones de control)");
            *slot = Some(State {
                main,
                reader,
                base,
                has_mlsd,
                seq: 0,
                active: None,
            });
            Ok(())
        })
    }

    fn capabilities() -> Caps {
        // Honestas (ADR 0014): remoto POSIX case-sensitive y case-preserving.
        // NO se declara APPEND/resume: la interfaz WIT no proyecta
        // open-resumable (deuda) y el adapter usa los defaults del trait.
        Caps {
            read_only: false,
            case_sensitive: true,
            case_preserving: true,
        }
    }

    fn stat(p: Segs) -> Result<Entry, VfsError> {
        with_state(|st| {
            let name = p.last().cloned().unwrap_or_default();
            // La raíz del provider es el directorio base: existe siempre.
            if p.is_empty() {
                return Ok(Entry {
                    name,
                    kind: EntryKind::Dir,
                    size: None,
                });
            }
            let r = remote(st, &p)?;
            match stat_remote(&mut st.main, &r, st.has_mlsd, &st.base)? {
                Some(f) => Ok(entry_of(name, &f)),
                None => Err(VfsError::NotFound),
            }
        })
    }

    fn list_dir(p: Segs, cursor: Option<Vec<u8>>) -> Result<Page, VfsError> {
        // Este guest lista en UNA página y jamás emite cursor: recibir uno es
        // ajeno/expirado.
        if cursor.is_some() {
            return Err(VfsError::CursorExpired);
        }
        with_state(|st| {
            let r = remote(st, &p)?;
            let lines = if st.has_mlsd {
                st.main.mlsd(Some(&r))
            } else {
                st.main.list(Some(&r))
            }
            .map_err(|e| map_err(&e))?;
            let mut entries = Vec::new();
            for line in lines {
                // Cota defensiva (issue #40) — el bound duro contra OOM es el
                // límite de memoria del store del host.
                if entries.len() >= MAX_LIST_ENTRIES {
                    return Err(VfsError::Io);
                }
                let parsed = if st.has_mlsd {
                    ListParser::parse_mlsd(&line).ok()
                } else {
                    parse_list_line(&line)
                };
                let f = match parsed {
                    Some(f) => f,
                    // MLSD machine-readable: línea ilegible corta (fail-loud).
                    None if st.has_mlsd => return Err(VfsError::Io),
                    // `ls -l`: cabeceras (`total N`) se descartan.
                    None => continue,
                };
                // Nombre: en MLSD, CRUDO de la línea (RFC 3659 `facts SP
                // pathname`) — el extractor de suppaftp trunca en `;`.
                let name = if st.has_mlsd {
                    let Some((_, n)) = line.split_once(' ') else {
                        return Err(VfsError::Io);
                    };
                    n
                } else {
                    f.name()
                };
                if name == "." || name == ".." {
                    continue;
                }
                // U+FFFD = suppaftp ya perdió los bytes (regla 1, issue #37);
                // `/` inyectado busca escapar la base. Corta fail-loud (la
                // página WIT no transporta errores por-entrada).
                if name.contains('\u{FFFD}') || name.contains('/') {
                    return Err(VfsError::InvalidPath);
                }
                entries.push(entry_of(name.as_bytes().to_vec(), &f));
            }
            Ok(Page {
                entries,
                next_cursor: None,
            })
        })
    }

    fn read(p: Segs, offset: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        if len == 0 {
            return Ok(Vec::new());
        }
        with_state(|st| {
            let continues = st
                .active
                .as_ref()
                .is_some_and(|a| a.segs == p && a.next_offset == offset);
            if !continues {
                // Acceso nuevo o no secuencial: drena el RETR cacheado y abre
                // otro. El pre-stat va por `main` (libre): NotFound limpio y
                // leer un dir = conflict, como el provider original.
                drain_active(st);
                let r = remote(st, &p)?;
                match stat_remote(&mut st.main, &r, st.has_mlsd, &st.base)? {
                    None => return Err(VfsError::NotFound),
                    Some(f) if f.is_directory() => return Err(VfsError::Conflict),
                    Some(_) => {}
                }
                if offset > 0 {
                    // wasm32: usize es de 32 bits — REST >4 GiB no
                    // representable (deuda documentada; el read secuencial
                    // desde 0 no la sufre).
                    let off = usize::try_from(offset).map_err(|_| VfsError::Io)?;
                    st.reader.resume_transfer(off).map_err(|e| map_err(&e))?;
                }
                let data = st.reader.retr_as_stream(&r).map_err(|e| map_err(&e))?;
                st.active = Some(ActiveRead {
                    segs: p.clone(),
                    next_offset: offset,
                    reader: Box::new(data),
                });
            }
            // El estado activo existe: o continuaba, o se acaba de crear.
            let a = st.active.as_mut().unwrap();
            let want = usize::try_from(len).unwrap_or(MAX_READ_CHUNK).min(MAX_READ_CHUNK);
            let mut buf = vec![0u8; want];
            match a.reader.read(&mut buf) {
                Ok(0) => {
                    // EOF: finaliza (lee el 226) y limpia el cache. El chunk
                    // vacío señala EOF al host.
                    let done = st.active.take().unwrap();
                    let _ = st.reader.finalize_retr_stream(done.reader);
                    Ok(Vec::new())
                }
                Ok(n) => {
                    buf.truncate(n);
                    a.next_offset += n as u64;
                    Ok(buf)
                }
                Err(_) => {
                    let done = st.active.take().unwrap();
                    let _ = st.reader.finalize_retr_stream(done.reader);
                    Err(VfsError::Io)
                }
            }
        })
    }

    type Writer = FtpWriter;

    fn open_writer(p: Segs) -> Result<Writer, VfsError> {
        with_state(|st| {
            let final_remote = remote(st, &p)?;
            let parent = remote_parent(st, &p)?;
            st.seq += 1;
            let staging = format!("{parent}/{PARTIAL_PREFIX}eph.{}", st.seq);
            // El destino final no debe existir (create-new; la política de
            // sobrescritura es del core). Ventana TOCTOU documentada.
            if exists(&mut st.main, &final_remote, st.has_mlsd, &st.base)? {
                return Err(VfsError::Conflict);
            }
            // Staging VACÍO: un write de 0 bytes tiene qué renombrar y los
            // write() posteriores solo APPE-an. Padre inexistente → 550 →
            // not-found (contrato).
            create_empty(&mut st.main, &staging)?;
            Ok(FtpWriter {
                staging,
                final_remote,
                done: Cell::new(false),
            })
        })
        .map(Writer::new)
    }

    fn make_dir(p: Segs) -> Result<(), VfsError> {
        with_state(|st| {
            let r = remote(st, &p)?;
            if exists(&mut st.main, &r, st.has_mlsd, &st.base)? {
                return Err(VfsError::Conflict);
            }
            st.main.mkdir(&r).map_err(|e| map_err(&e))
        })
    }

    fn remove(p: Segs) -> Result<(), VfsError> {
        with_state(|st| {
            let r = remote(st, &p)?;
            let f = stat_remote(&mut st.main, &r, st.has_mlsd, &st.base)?
                .ok_or(VfsError::NotFound)?;
            if f.is_directory() {
                st.main.rmdir(&r).map_err(|e| map_err(&e))
            } else {
                st.main.rm(&r).map_err(|e| map_err(&e))
            }
        })
    }

    fn rename(src: Segs, dst: Segs) -> Result<(), VfsError> {
        with_state(|st| {
            let s = remote(st, &src)?;
            let d = remote(st, &dst)?;
            // RNFR/RNTO no garantiza no-replace: se comprueba antes (TOCTOU
            // documentada) para dar conflict, no pisar.
            if exists(&mut st.main, &d, st.has_mlsd, &st.base)? {
                return Err(VfsError::Conflict);
            }
            st.main.rename(&s, &d).map_err(|e| map_err(&e))
        })
    }
}

/// Writer transaccional sobre el staging (`main`): cada `write` es un APPE de
/// su chunk (la conexión queda libre entre chunks — un stat concurrente del
/// host no se bloquea); `commit` = check-exists + rename; `abort`/Drop sin
/// commit = rm best-effort del staging.
struct FtpWriter {
    staging: String,
    final_remote: String,
    done: Cell<bool>,
}

impl GuestWriter for FtpWriter {
    fn write(&self, chunk: Vec<u8>) -> Result<(), VfsError> {
        if chunk.is_empty() {
            return Ok(());
        }
        with_state(|st| {
            let mut data = st
                .main
                .append_with_stream(&self.staging)
                .map_err(|e| map_err(&e))?;
            let res = data.write_all(&chunk);
            // Cierra la conexión de datos y lee la respuesta SIEMPRE (aunque
            // el write fallara), o el control queda desincronizado.
            let fin = st.main.finalize_put_stream(data);
            res.map_err(|_| VfsError::Io)?;
            fin.map_err(|e| map_err(&e))
        })
    }

    fn commit(&self) -> Result<(), VfsError> {
        with_state(|st| {
            if exists(&mut st.main, &self.final_remote, st.has_mlsd, &st.base)? {
                return Err(VfsError::Conflict);
            }
            st.main
                .rename(&self.staging, &self.final_remote)
                .map_err(|e| map_err(&e))?;
            self.done.set(true);
            Ok(())
        })
    }

    fn abort(&self) -> Result<(), VfsError> {
        with_state(|st| {
            let _ = st.main.rm(&self.staging);
            self.done.set(true);
            Ok(())
        })
    }
}

impl Drop for FtpWriter {
    /// Soltar sin commit/abort limpia el staging (best-effort) — contrato de
    /// ByteSink proyectado.
    fn drop(&mut self) {
        if !self.done.get() {
            let _ = with_state(|st| {
                let _ = st.main.rm(&self.staging);
                Ok(())
            });
        }
    }
}

export!(FtpPlugin);
```

Note on `unwrap()`: guests are standalone crates outside workspace lints; each `unwrap` above carries a stated invariant (fresh `Some`, 4-byte slice, 4 pushed fields).

- [ ] **Step 2: Build:**

```bash
cd crates/norte-plugin-host/examples-wasm/provider-ftp
cargo build --release --target wasm32-wasip2
```

Expected: compiles clean. Fix API-name drift found by Task 1 here.

- [ ] **Step 3: Commit** (`feat(plugin-host): #30 guest provider-ftp — port completo del provider FTP a wasm`).

---

## Task 6: Contract suite against libunftp through the plugin

**Files:**
- Create: `crates/norte-core/tests/plugin_ftp_contract.rs`
- Modify: `crates/norte-core/Cargo.toml` (dev-deps: `libunftp`, `unftp-sbe-fs` — already workspace deps; `norte-testkit`, `sha2`, `tempfile` already present)

- [ ] **Step 1: Write the test file:**

```rust
//! `provider_contract!` sobre el PLUGIN FTP (#30 stage final): la MISMA suite
//! que pasaba `norte-vfs-ftp`, ahora a través del stack completo — guest
//! provider-ftp (suppaftp sync sobre wasi:sockets) → capability `net` gateada
//! → `PluginProvider` → servidor libunftp IN-PROCESS. Sin Docker.
//!
//! Solo-Linux (el harness libunftp mapea sobre el FS del host: exige POSIX
//! fiel) + SKIP sin el target `wasm32-wasip2` (vía `skip_if:` del macro).
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use norte_core::plugin_provider::{PluginProvider, ftp_plugin_config};
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_proto::Authority;

/// Compila el guest `provider-ftp` UNA vez por proceso de test.
fn guest_wasm() -> Option<&'static PathBuf> {
    static WASM: OnceLock<Option<PathBuf>> = OnceLock::new();
    WASM.get_or_init(|| build_guest("provider-ftp")).as_ref()
}

fn wasm_target_missing() -> bool {
    guest_wasm().is_none()
}

/// Arranca libunftp sobre un tempdir en un puerto efímero y devuelve el
/// puerto (el tempdir se `forget`-ea: tests efímeros, el SO limpia /tmp).
fn spawn_ftp_server() -> u16 {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    std::mem::forget(dir);
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind efímero");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);
    let server = libunftp::ServerBuilder::new(Box::new(move || {
        unftp_sbe_fs::Filesystem::new(home.clone()).expect("fs backend")
    }))
    .greeting("norte plugin ftp contract")
    .build()
    .expect("build server");
    tokio::spawn(async move {
        let _ = server.listen(format!("127.0.0.1:{port}")).await;
    });
    port
}

/// Espera a que el puerto escuche (el listen del task tarda un instante).
async fn wait_listening(port: u16) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("el servidor ftp no arrancó en :{port}");
}

/// Provider FRESCO: servidor libunftp propio + instancia nueva del guest con
/// `net` acotada a 127.0.0.1 (todos los puertos: el PASV negocia datos
/// dinámicos) y `configure` con login anónimo.
async fn fresh() -> PluginProvider {
    let wasm = guest_wasm().expect("guardado por skip_if").clone();
    let port = spawn_ftp_server();
    wait_listening(port).await;
    let caps = HostCaps::with_net(vec!["127.0.0.1".to_owned()]);
    let cfg = ftp_plugin_config(&format!("127.0.0.1:{port}"), "anonymous", "anonymous", "/");
    // La instanciación compila el componente (bloqueante): fuera del executor.
    tokio::task::spawn_blocking(move || {
        let rt = PluginRuntime::new().expect("runtime");
        PluginProvider::new(rt, &wasm, caps, "ftp", Some(&cfg)).expect("provider ftp")
    })
    .await
    .expect("join")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod ftp_plugin,
    factory: fresh().await,
    root: norte_vfs::proto::VPath::root(
        norte_vfs::proto::Scheme::new("ftp").expect("scheme"),
        Some(Authority::new("test:21").expect("authority válida")),
    ),
    hostile_names: hostile_names(),
    skip_if: wasm_target_missing(),
}

// ---- casos extra fuera del macro: resync, gating, inyección ----

/// Cancelar una lectura a mitad (drop del stream) deja un RETR en vuelo en el
/// guest; la siguiente operación debe funcionar (drenado — equivalente del
/// resync #39 M1).
#[tokio::test]
async fn dropped_read_stream_resyncs() {
    if wasm_target_missing() {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    }
    use futures::StreamExt;
    let p = fresh().await;
    let root = norte_vfs::proto::VPath::root(
        norte_vfs::proto::Scheme::new("ftp").expect("scheme"),
        Some(Authority::new("test:21").expect("authority")),
    );
    let f = root.join(norte_proto::Segment::new(b"grande.bin".to_vec()).expect("seg"));
    // 1 MiB: varias páginas de 64 KiB del adapter.
    let content = vec![0xA5u8; 1 << 20];
    let mut sink = norte_vfs::Provider::write(&p, &f).await.expect("write");
    sink.write(bytes::Bytes::from(content.clone())).await.expect("chunk");
    sink.commit().await.expect("commit");
    // Lee SOLO el primer chunk y suelta el stream (cancelación).
    let mut stream = norte_vfs::Provider::read(&p, &f, None).await.expect("read");
    let first = stream.next().await.expect("un chunk").expect("ok");
    assert!(!first.is_empty());
    drop(stream);
    // La conexión debe resincronizarse: stat y relectura completa funcionan.
    let st = norte_vfs::Provider::stat(&p, &f).await.expect("stat tras drop");
    assert_eq!(st.size, Some(content.len() as u64));
    let mut stream = norte_vfs::Provider::read(&p, &f, None).await.expect("re-read");
    let mut total = 0usize;
    while let Some(c) = stream.next().await {
        total += c.expect("chunk ok").len();
    }
    assert_eq!(total, content.len(), "relectura íntegra tras cancelación");
}

/// Sin la capability `net`, `configure` no puede ni conectar: fail-closed.
#[tokio::test]
async fn without_net_capability_configure_fails() {
    if wasm_target_missing() {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    }
    let wasm = guest_wasm().expect("guardado por skip").clone();
    let port = spawn_ftp_server();
    wait_listening(port).await;
    let cfg = ftp_plugin_config(&format!("127.0.0.1:{port}"), "anonymous", "anonymous", "/");
    let err = tokio::task::spawn_blocking(move || {
        let rt = PluginRuntime::new().expect("runtime");
        PluginProvider::new(rt, &wasm, HostCaps::default(), "ftp", Some(&cfg)).err()
    })
    .await
    .expect("join");
    assert!(err.is_some(), "sin net el configure debe fallar");
}

/// Credenciales con CR/LF: el guest las rechaza ANTES de tocar la red
/// (USER/PASS son comandos de línea — inyección FTP).
#[tokio::test]
async fn crlf_credentials_rejected() {
    if wasm_target_missing() {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    }
    let wasm = guest_wasm().expect("guardado por skip").clone();
    let port = spawn_ftp_server();
    wait_listening(port).await;
    let cfg = ftp_plugin_config(
        &format!("127.0.0.1:{port}"),
        "eve\r\nDELE victima",
        "x",
        "/",
    );
    let caps = HostCaps::with_net(vec!["127.0.0.1".to_owned()]);
    let err = tokio::task::spawn_blocking(move || {
        let rt = PluginRuntime::new().expect("runtime");
        PluginProvider::new(rt, &wasm, caps, "ftp", Some(&cfg)).err()
    })
    .await
    .expect("join");
    assert!(err.is_some(), "user con CR/LF debe rechazarse");
}

/// Un segmento con CR/LF (válido como bytes POSIX, letal en FTP) se rechaza
/// limpio como InvalidPath — la defensa anti-inyección del port.
#[tokio::test]
async fn crlf_segment_rejected() {
    if wasm_target_missing() {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    }
    let p = fresh().await;
    let root = norte_vfs::proto::VPath::root(
        nor