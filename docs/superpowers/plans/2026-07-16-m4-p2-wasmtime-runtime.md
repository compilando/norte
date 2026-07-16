# M4-P2 — Runtime wasmtime + Component Model (WIT) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development o superpowers:executing-plans. Los pasos usan checkbox (`- [ ]`).

**Goal:** `norte-plugin-host` carga y EJECUTA componentes WASM de terceros, sandboxeados por sus capabilities declaradas — un plugin `previewer` (renderiza un preview de un archivo que el host le pasa) y un plugin `command` (ejecuta una acción y devuelve un mensaje) corren de verdad, aislados, con el FS negado por defecto.

**Architecture:** wasmtime + Component Model (ADR 0022 D1). El host define un world `norte:plugin` en WIT con dos interfaces de guest (`previewer`, `command`) y una interfaz de host (`host-log`, la puerta mínima que demuestra el gating). Bindings con `wasmtime::component::bindgen!` (host-side, sin dep `wit-bindgen` aparte). El sandbox = WASI preview 2 con builder VACÍO por defecto (sin FS, sin red, sin env, sin stdio real) + las capabilities del manifiesto abren SOLO lo declarado (`fs-read=scoped` → el host lee el archivo ÉL y pasa los bytes, jamás monta el FS en el componente — regla dura 9). Las otras 3 interfaces (provider, columns, hook) y la UI del gestor (P3) quedan fuera de este plan.

**Tech Stack:** Rust, wasmtime 46 (`component-model` + `runtime`), `wasmtime-wasi` 36 (p2), WIT, target `wasm32-wasip2` para los ejemplos-guest. `cargo deny` (wasmtime trae Apache-2.0-WITH-LLVM-exception y BSD — hay que ampliar el allow-list). Reviewers: **security-reviewer OBLIGATORIO** (sandbox, capability enforcement, deps), rust-reviewer por task.

**Nota de versiones:** las rutas EXACTAS de la API de `wasmtime-wasi` p2 cambian entre versiones. Este plan fija `wasmtime = "46"` y da la FORMA del código; cada task marca los imports a CONFIRMAR contra `cargo doc -p wasmtime-wasi` de la versión resuelta antes de dar por bueno un paso. Nunca copies un import sin que compile.

**Nota de threat model:** el sandbox WASI aísla al plugin del FS/red/proceso del host. La integración con el POLICY ENGINE (M3, ask/allow/deny + journal) se superpone después; hoy el host media toda syscall vía capabilities (ADR 0022 D7).

---

## File Structure

- `crates/norte-plugin-host/wit/norte-plugin.wit` — el world `norte:plugin` y sus interfaces (contrato host↔guest).
- `crates/norte-plugin-host/src/runtime.rs` — el runtime: `Engine`, carga/instanciación de un componente, `HostState` (WASI ctx + capabilities + resource table), imports de host gateados.
- `crates/norte-plugin-host/src/bindings.rs` — el `bindgen!` de wasmtime (aislado; genera mucho código).
- `crates/norte-plugin-host/src/lib.rs` — re-exports (modificar).
- `crates/norte-plugin-host/Cargo.toml` — deps wasmtime (modificar).
- `crates/norte-plugin-host/examples-wasm/previewer-demo/` — guest de ejemplo (crate FUERA del workspace).
- `crates/norte-plugin-host/examples-wasm/command-demo/` — guest de ejemplo.
- `crates/norte-plugin-host/tests/runtime.rs` — tests de integración (construyen los guests bajo demanda).
- `crates/norte-plugin-host/tests/support/mod.rs` — helper que compila un guest a `wasm32-wasip2` y devuelve la ruta del `.wasm` (o SKIP si el target no está).
- `deny.toml` — ampliar allow-list de licencias (modificar).
- `Cargo.toml` (raíz) — `exclude` de `examples-wasm/*` (modificar).
- `docs/adr/0022-plugin-host-wasm-manifiesto.md` — addendum P2 (modificar).

---

## Task 1: deps wasmtime + licencias + target WASM

**Files:**
- Modify: `crates/norte-plugin-host/Cargo.toml`
- Modify: `deny.toml`
- Modify: `Cargo.toml` (raíz: `exclude`)

- [ ] **Step 1: instala el target guest**

Run: `rustup target add wasm32-wasip2`
Expected: `info: downloading component 'rust-std' for 'wasm32-wasip2'` (o "up to date").

- [ ] **Step 2: añade las deps** a `crates/norte-plugin-host/Cargo.toml`. wasmtime es dep GRANDE y estructural (ADR 0022 D1 lo justifica): esto va en la descripción de la PR.

```toml
[dependencies]
serde = { workspace = true, features = ["derive"] }
toml.workspace = true
thiserror.workspace = true
wasmtime = { version = "46", default-features = false, features = ["runtime", "component-model", "cranelift", "std"] }
wasmtime-wasi = "36"
anyhow = "1"           # wasmtime usa anyhow en su API; el host lo envuelve en thiserror hacia afuera

[dev-dependencies]
tempfile.workspace = true
```

Verifica que `wasmtime-wasi = "36"` es la versión COMPATIBLE con `wasmtime = "46"` (mismo release train): `cargo tree -p norte-plugin-host -i wasmtime` tras el add debe mostrar UNA sola versión de wasmtime. Si `cargo` resuelve un mismatch, ajusta `wasmtime-wasi` a la versión cuyo `Cargo.toml` depende de `wasmtime = "46"` (mira `cargo add wasmtime-wasi --dry-run` y su rango de wasmtime).

Añade `anyhow` a `[workspace.dependencies]` del `Cargo.toml` raíz si no está, y usa `anyhow.workspace = true`; si el workspace ya lo tiene, úsalo.

- [ ] **Step 3: excluye los guests del workspace** — en el `Cargo.toml` raíz, bajo `[workspace]`:

```toml
exclude = ["crates/norte-plugin-host/examples-wasm"]
```

(Los guests son crates `wasm32-wasip2` con su propio perfil; JAMÁS miembros del workspace host — romperían `cargo build --workspace`.)

- [ ] **Step 4: compila y corre deny** — esto BAJA todo el árbol de wasmtime (lento la primera vez, varios minutos):

Run: `cargo build -p norte-plugin-host 2>&1 | tail -5`
Expected: compila (o falla solo por el `bindings`/`runtime` que aún no existen — si el error es "unresolved import wasmtime", el add funcionó).

Run: `cargo deny check licenses 2>&1 | tail -20`
Expected: FALLA con crates de wasmtime/cranelift cuya licencia no está en el allow (`Apache-2.0 WITH LLVM-exception`, quizá `BSD-2-Clause`, `Zlib`, `MPL-2.0`).

- [ ] **Step 5: amplía el allow-list** en `deny.toml`, **una entrada por licencia nueva con comentario** (el estilo del fichero: cada excepción justificada). Añade al array `allow` las que sean licencias de proyecto entero de la familia LLVM/wasm:

```toml
allow = [
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",   # wasmtime/cranelift (ADR 0022 D1): la excepción LLVM es MÁS permisiva que Apache-2.0 pelada
    "MIT",
    "AGPL-3.0-only",
    "Unicode-3.0",
    "BSD-3-Clause",
    "BSD-2-Clause",                     # transitivas de wasmtime (p. ej. arbitrary/…): permisiva estilo BSD-3
    "ISC",
    "Zlib",
]
```

Si `deny` reporta OTRAS licencias (p. ej. `MPL-2.0`, que NO es permisiva sin matices), NO la metas al allow global: añádela a `exceptions` acotada al crate concreto con su justificación, o si es problemática, PARA y coméntalo (puede exigir decisión de oscar). Re-corre `cargo deny check` hasta verde. Documenta en el commit qué licencias entraron y por qué.

- [ ] **Step 6: Commit.**

```bash
git add crates/norte-plugin-host/Cargo.toml deny.toml Cargo.toml Cargo.lock
git commit -m "build(plugin-host): deps wasmtime 46 + ampliación de licencias deny (M4-P2 T1)"
```

rust-reviewer NO hace falta aquí (solo build/deps); **la ampliación de licencias la revisa security-reviewer al final (Task 7)**, junta con el sandbox.

---

## Task 2: el world WIT `norte:plugin`

**Files:**
- Create: `crates/norte-plugin-host/wit/norte-plugin.wit`

- [ ] **Step 1: escribe el contrato WIT.** Dos interfaces que EXPORTA el guest (`previewer`, `command`) y una que IMPORTA del host (`host-log`). Minimalismo deliberado: la superficie crece con las otras interfaces en P2b.

`crates/norte-plugin-host/wit/norte-plugin.wit`:

```wit
package norte:plugin@0.1.0;

/// La puerta de host MÍNIMA (demuestra el gating host↔guest): un plugin puede
/// pedir al host que registre una línea de log. Sin capability especial (es
/// inocua); las puertas gateadas (fs, net) llegan con sus interfaces.
interface host-log {
    log: func(message: string);
}

/// Genera una previsualización de UN recurso que el HOST ya leyó y le pasa
/// (regla 9: el guest jamás toca el FS; recibe los bytes). Devuelve texto
/// plano listo para pintar, o un error legible.
interface previewer {
    record preview-input {
        /// Mimetype detectado por el host.
        mimetype: string,
        /// Contenido del recurso (el host lo leyó bajo fs-read=scoped).
        content: list<u8>,
    }
    render: func(input: preview-input) -> result<string, string>;
}

/// Ejecuta un comando aportado por el plugin. `arg` es texto libre del
/// invocador; devuelve un mensaje para la barra de estado, o un error.
interface command {
    run: func(id: string, arg: string) -> result<string, string>;
}

/// El world que un plugin de norte implementa. Un plugin concreto exporta las
/// interfaces de su categoría (un previewer exporta `previewer`, etc.); las que
/// no implementa quedan sin exportar y el host no las llama.
world norte-plugin {
    import host-log;
    export previewer;
    export command;
}
```

**Nota:** un componente que exporta `norte-plugin` DEBE exportar TODAS las interfaces del world (`previewer` Y `command`). Para que un guest de una sola categoría no tenga que implementar la otra, los ejemplos de Task 5/6 implementan AMBAS (la que no les toca devuelve `Err("unsupported")`). Alternativa más limpia (deuda P2b): worlds separados por categoría (`previewer-plugin`, `command-plugin`) o interfaces `optional` — se anota, no se hace aquí.

- [ ] **Step 2: Verifica la sintaxis WIT** compilando el bindgen en Task 3 (el WIT no se valida solo; el `bindgen!` lo parsea). Aquí basta guardar el fichero.

- [ ] **Step 3: Commit.**

```bash
git add crates/norte-plugin-host/wit/norte-plugin.wit
git commit -m "feat(plugin-host): world WIT norte:plugin (previewer+command+host-log) (M4-P2 T2)"
```

---

## Task 3: el runtime — cargar, sandboxear, instanciar

**Files:**
- Create: `crates/norte-plugin-host/src/bindings.rs`
- Create: `crates/norte-plugin-host/src/runtime.rs`
- Modify: `crates/norte-plugin-host/src/lib.rs`
- Test: `crates/norte-plugin-host/tests/runtime.rs` (parte 1)

- [ ] **Step 1: genera los bindings.** `crates/norte-plugin-host/src/bindings.rs`:

```rust
//! Bindings del Component Model generados por wasmtime desde el WIT (M4-P2).
//! Aislado en su módulo: `bindgen!` emite MUCHO código.
#![allow(missing_docs)] // el código generado no lleva rustdoc

wasmtime::component::bindgen!({
    world: "norte-plugin",
    path: "wit/norte-plugin.wit",
    // El host implementa `host-log` de forma síncrona; los exports del guest
    // también se llaman síncronos (sin async runtime dentro del componente).
});
```

Confirma contra la doc de wasmtime 46 el nombre EXACTO del struct generado para el world (`NornePlugin`? `NortePlugin`? el macro camel-casa `norte-plugin` → `NortePlugin`) y de los traits de import (algo como `HostLogImports` o el trait de la interfaz `host-log`). Los nombres exactos los fija el macro; el resto del plan usa `NortePlugin` (struct del world) y `norte::plugin::host_log::Host` (trait de la interfaz de host) — AJUSTA a lo que el macro genere realmente (mira `cargo doc -p norte-plugin-host --open` o un `cargo expand` acotado).

- [ ] **Step 2: escribe el runtime.** `crates/norte-plugin-host/src/runtime.rs`:

```rust
//! Runtime de plugins (M4-P2, ADR 0022): compila un componente WASM, lo
//! sandboxea con WASI preview 2 (builder VACÍO por defecto — sin FS, red,
//! env ni stdio) y lo instancia. Las capabilities del manifiesto abren SOLO
//! lo declarado; el guest jamás toca el FS (regla 9).

use std::path::Path;

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView};
use wasmtime_wasi::ResourceTable;

use crate::bindings::NortePlugin;
use crate::capability::Capabilities;

/// Errores del runtime de plugins.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// El fichero `.wasm` no se pudo leer o no es un componente válido.
    #[error("componente inválido: {0}")]
    Component(String),
    /// Fallo instanciando o enlazando el componente.
    #[error("instanciación: {0}")]
    Instantiate(String),
    /// El guest atrapó un trap (panic/unreachable) durante una llamada.
    #[error("trap del plugin: {0}")]
    Trap(String),
    /// El guest devolvió un `Err` legible desde su función.
    #[error("el plugin falló: {0}")]
    Guest(String),
}

/// Estado del host visible al componente: el contexto WASI (lo que el sandbox
/// permite) + las capabilities declaradas (para gatear las puertas de host) +
/// la resource table de WASI. Un `Store` por instancia de plugin.
pub struct HostState {
    ctx: WasiCtx,
    table: ResourceTable,
    caps: Capabilities,
    /// Log capturado de las llamadas a `host-log` (para tests y auditoría).
    logs: Vec<String>,
}

// WASI exige estos dos accessors (IoView/WasiView) — confirma los nombres de
// trait/método contra wasmtime-wasi 36 (en algunas versiones es solo WasiView
// con `ctx()`+`table()`; en otras IoView aporta `table()`).
impl IoView for HostState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}
impl WasiView for HostState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }
}

/// Implementación de la puerta `host-log` (import del world). AJUSTA el nombre
/// del trait al que genere el `bindgen!` (`crate::bindings::norte::plugin::host_log::Host`).
impl crate::bindings::norte::plugin::host_log::Host for HostState {
    fn log(&mut self, message: String) {
        // Sin capability: es inocuo. Se captura (auditoría/tests) y se traza
        // redactado si hiciera falta (aquí no hay paths).
        tracing_message(&mut self.logs, message);
    }
}

fn tracing_message(logs: &mut Vec<String>, message: String) {
    // Tope anti-abuso: un plugin no infla memoria del host con logs.
    const MAX_LOGS: usize = 1024;
    if logs.len() < MAX_LOGS {
        logs.push(message);
    }
}

/// El runtime: un `Engine` compartido (caro de crear, barato de clonar-Arc por
/// dentro) que carga componentes bajo demanda.
pub struct PluginRuntime {
    engine: Engine,
}

impl PluginRuntime {
    /// Crea el runtime. El `Engine` fija las políticas de compilación/límites.
    ///
    /// # Errors
    /// [`RuntimeError::Component`] si la config del engine es inválida.
    pub fn new() -> Result<Self, RuntimeError> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        // Límite de memoria por instancia (anti-DoS): un plugin no agota la
        // RAM del host. 64 MiB es holgado para un previewer.
        // (La API de límites por-store se aplica en `instantiate`, ver abajo.)
        let engine = Engine::new(&config).map_err(|e| RuntimeError::Component(e.to_string()))?;
        Ok(Self { engine })
    }

    /// Carga un componente desde un `.wasm` y lo instancia con el sandbox
    /// derivado de `caps`. Devuelve una instancia lista para llamar.
    ///
    /// # Errors
    /// [`RuntimeError`] si el componente no compila, no enlaza o no instancia.
    pub fn instantiate(
        &self,
        wasm_path: &Path,
        caps: Capabilities,
    ) -> Result<PluginInstance, RuntimeError> {
        let component = Component::from_file(&self.engine, wasm_path)
            .map_err(|e| RuntimeError::Component(e.to_string()))?;

        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        // WASI p2 en el linker (aporta las interfaces wasi:* que el guest-std
        // necesita para arrancar). Confirma el nombre: `add_to_linker_sync`.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        // Nuestra puerta host-log. AJUSTA al add_to_linker que genere el macro
        // (algo como `NortePlugin::add_to_linker` o
        // `norte::plugin::host_log::add_to_linker`).
        crate::bindings::norte::plugin::host_log::add_to_linker(&mut linker, |s: &mut HostState| s)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        // SANDBOX: builder VACÍO. Sin `.inherit_stdio()`, sin `.preopened_dir`,
        // sin `.inherit_network()`, sin env. El componente arranca aislado; las
        // capabilities abren puertas DESPUÉS, explícitamente (fs-read pasa
        // BYTES, no monta el FS — así que aquí no se preabre nada). ESTA es la
        // línea que un review de seguridad mira primero.
        let ctx = WasiCtxBuilder::new().build();

        let state = HostState {
            ctx,
            table: ResourceTable::new(),
            caps,
            logs: Vec::new(),
        };
        let mut store = Store::new(&self.engine, state);
        // Límite de memoria por store (anti-DoS): confirma la API en wasmtime 46
        // (`store.limiter(...)` con un StoreLimits de `StoreLimitsBuilder`).
        // Si la API difiere, deja el límite como deuda anotada y un TODO con
        // issue — NO lo omitas en silencio.

        let bindings = NortePlugin::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        Ok(PluginInstance { store, bindings })
    }
}

/// Una instancia viva de plugin: su `Store` (estado + WASI) y los bindings
/// para llamar a sus exports. NO es `Send`-seguro entre hilos por diseño (un
/// plugin es single-thread); el host la usa desde un solo owner.
pub struct PluginInstance {
    store: Store<HostState>,
    bindings: NortePlugin,
}

impl PluginInstance {
    /// Los logs que el plugin emitió por `host-log` (auditoría/tests).
    #[must_use]
    pub fn logs(&self) -> &[String] {
        &self.store.data().logs
    }
}
```

**IMPORTANTE:** varios nombres de API (`IoView`/`WasiView`, `add_to_linker_sync`, el path del `bindgen!`, `StoreLimits`) DEBEN confirmarse contra la versión resuelta. El paso no está hecho hasta que `cargo build -p norte-plugin-host` compile. Si un import no existe, busca el equivalente en `cargo doc` — NO inventes.

- [ ] **Step 3: expón el runtime** en `lib.rs`:

```rust
mod bindings;
mod capability;
mod catalog;
mod manifest;
mod runtime;

pub use capability::{Capabilities, NetCap, Scope};
pub use catalog::{Catalog, LoadError, PluginEntry, Tier};
pub use manifest::{
    Category, ColumnContrib, CommandContrib, Contributions, HookContrib, Manifest, ManifestError,
    PreviewerContrib, ProviderContrib,
};
pub use runtime::{PluginInstance, PluginRuntime, RuntimeError};
```

- [ ] **Step 4: test rojo del runtime SIN componente** (siempre corre, no necesita el target wasm) — en `tests/runtime.rs`:

```rust
//! Runtime de plugins (M4-P2): construcción del engine + carga fallida clara.
//! Los tests que EJECUTAN un componente real viven aparte (gated por target).

use norte_plugin_host::{Capabilities, PluginRuntime, RuntimeError};

#[test]
fn runtime_se_construye() {
    let _rt = PluginRuntime::new().expect("engine");
}

#[test]
fn cargar_un_no_componente_falla_claro() {
    let rt = PluginRuntime::new().expect("engine");
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("no.wasm");
    std::fs::write(&fake, b"esto no es un componente wasm").unwrap();
    let err = rt
        .instantiate(&fake, Capabilities::default())
        .expect_err("bytes basura no son un componente");
    assert!(matches!(err, RuntimeError::Component(_)), "fue {err:?}");
}
```

- [ ] **Step 5: verde** — `cargo nextest run -p norte-plugin-host` (los 2 tests nuevos + los del modelo P1). Commit:

```bash
git add crates/norte-plugin-host/src crates/norte-plugin-host/tests
git commit -m "feat(plugin-host): runtime wasmtime — engine, sandbox WASI vacío, instanciación (M4-P2 T3)"
```

rust-reviewer (foco: el builder WASI vacío, el mapeo de errores, `#![forbid(unsafe_code)]` intacto — wasmtime NO exige unsafe en el host).

---

## Task 4: helper de build de guests + fixture del target

**Files:**
- Create: `crates/norte-plugin-host/tests/support/mod.rs`

- [ ] **Step 1: el helper que compila un guest a wasm.** Compila un crate de `examples-wasm/<name>/` a `wasm32-wasip2` y devuelve la ruta del `.wasm`, o `None` si el target no está instalado (para que `just ci` en una máquina sin el target SALTE el test en vez de fallarlo).

`crates/norte-plugin-host/tests/support/mod.rs`:

```rust
//! Soporte de los tests de integración: compila un guest de ejemplo a
//! `wasm32-wasip2` bajo demanda. Si el target no está instalado, devuelve
//! `None` y el test SALTA (no falla) — un contribuidor sin el toolchain wasm
//! sigue con `just ci` verde; la máquina de dev/nightly (con el target) sí
//! ejecuta el componente real.

use std::path::PathBuf;
use std::process::Command;

/// `true` si `wasm32-wasip2` está instalado (via `rustc --print target-list`
/// NO vale: lista los SOPORTADOS; usamos `rustup target list --installed`).
fn target_instalado() -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-wasip2"))
}

/// Compila el guest `name` (dir `examples-wasm/<name>`) y devuelve el `.wasm`.
/// `None` = target ausente → el test debe SKIP con un mensaje claro.
///
/// # Panics
/// Si el target ESTÁ pero el guest no compila (eso sí es un fallo real).
pub fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_instalado() {
        eprintln!("SKIP: target wasm32-wasip2 no instalado (rustup target add wasm32-wasip2)");
        return None;
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let guest_dir = PathBuf::from(manifest_dir)
        .join("examples-wasm")
        .join(name);
    // target-dir propio bajo el del host, para no ensuciar ni chocar locks.
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("lanzar cargo build del guest");
    assert!(status.success(), "el guest {name} debe compilar");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "el .wasm del guest {name} no apareció en {wasm:?}");
    Some(wasm)
}
```

**Nota:** `CARGO_TARGET_TMPDIR` lo da cargo a los tests de integración (dir temporal por-crate). El nombre del `.wasm` = el nombre del crate guest con `-`→`_` (Rust normaliza). Confirma el nombre real tras el primer build (algunos setups emiten `<name>.wasm` sin normalizar para componentes — ajusta el `format!`).

- [ ] **Step 2: no hay test aún** (el helper se ejercita en Task 5). Verifica que compila: `cargo build -p norte-plugin-host --tests 2>&1 | tail -3` (el módulo `support` compila aunque no se use todavía — puede avisar `dead_code`; se usa en Task 5, no lo silencies con `#[allow]` global).

- [ ] **Step 3: Commit.**

```bash
git add crates/norte-plugin-host/tests/support
git commit -m "test(plugin-host): helper de build de guests wasm (skip si falta el target) (M4-P2 T4)"
```

---

## Task 5: interfaz `previewer` end-to-end + guest de ejemplo

**Files:**
- Create: `crates/norte-plugin-host/examples-wasm/previewer-demo/Cargo.toml`
- Create: `crates/norte-plugin-host/examples-wasm/previewer-demo/src/lib.rs`
- Create: `crates/norte-plugin-host/examples-wasm/previewer-demo/wit/` (symlink o copia del WIT)
- Modify: `crates/norte-plugin-host/src/runtime.rs` (método `render_preview`)
- Test: `crates/norte-plugin-host/tests/runtime.rs`

- [ ] **Step 1: el guest previewer.** Un componente que implementa `previewer::render` (devuelve un resumen del contenido) y `command::run` (devuelve `Err("unsupported")`, porque el world exige ambos). Usa `wit-bindgen` GUEST-side (dep del guest, no del host).

`examples-wasm/previewer-demo/Cargo.toml`:

```toml
[package]
name = "previewer-demo"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.46"   # confirma que la versión guest es compatible con el Component Model de wasmtime 46

[profile.release]
# Componentes pequeños: sin panic-unwind (menos código), opt-size.
panic = "abort"
opt-level = "s"
strip = true
```

`examples-wasm/previewer-demo/src/lib.rs`:

```rust
//! Guest de ejemplo (M4-P2): un previewer que resume el contenido que el host
//! le pasa, y llama a `host-log` para demostrar la puerta host↔guest. NO toca
//! el FS (no puede: sandbox). Implementa `command` como no-soportado.
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
});

struct Demo;

impl exports::norte::plugin::previewer::Guest for Demo {
    fn render(input: exports::norte::plugin::previewer::PreviewInput) -> Result<String, String> {
        // Demuestra la puerta de host (import).
        norte::plugin::host_log::log(&format!(
            "previewer-demo: {} bytes de {}",
            input.content.len(),
            input.mimetype
        ));
        // "Preview": primeras 3 líneas + tamaño (lógica trivial de ejemplo).
        let texto = String::from_utf8_lossy(&input.content);
        let head: Vec<&str> = texto.lines().take(3).collect();
        Ok(format!(
            "[{}] {} bytes\n{}",
            input.mimetype,
            input.content.len(),
            head.join("\n")
        ))
    }
}

impl exports::norte::plugin::command::Guest for Demo {
    fn run(_id: String, _arg: String) -> Result<String, String> {
        Err("previewer-demo no aporta comandos".to_string())
    }
}

export!(Demo);
```

**Nota WIT del guest:** el `path: "wit"` busca `examples-wasm/previewer-demo/wit/`. Copia (o enlaza) `crates/norte-plugin-host/wit/norte-plugin.wit` ahí — el guest y el host DEBEN compartir el mismo WIT. Un symlink relativo (`ln -s ../../../wit wit`) evita la copia divergente; si prefieres copia, añade un test que verifique que son idénticos. Los nombres generados guest-side (`exports::norte::plugin::previewer::Guest`, `norte::plugin::host_log::log`) los fija `wit-bindgen`; AJUSTA a lo que emita (mira un `cargo build` del guest y el error si el path no existe).

- [ ] **Step 2: método `render_preview` en el host** — añade a `PluginInstance` en `runtime.rs`:

```rust
    /// Llama al export `previewer.render` del plugin con un recurso que el HOST
    /// ya leyó (regla 9). `RuntimeError::Guest` si el plugin devolvió `Err`;
    /// `RuntimeError::Trap` si el plugin panicó/trapeó.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] o [`RuntimeError::Guest`].
    pub fn render_preview(
        &mut self,
        mimetype: &str,
        content: &[u8],
    ) -> Result<String, RuntimeError> {
        // AJUSTA los nombres a lo que genere el bindgen host-side: el guard
        // `previewer()` y su método `call_render`. La forma típica es
        // `self.bindings.norte_plugin_previewer().call_render(&mut self.store, &input)`.
        let input = crate::bindings::norte::plugin::previewer::PreviewInput {
            mimetype: mimetype.to_string(),
            content: content.to_vec(),
        };
        let result = self
            .bindings
            .norte_plugin_previewer()
            .call_render(&mut self.store, &input)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?;
        result.map_err(RuntimeError::Guest)
    }
```

- [ ] **Step 3: test de integración** — en `tests/runtime.rs`, añade el módulo support y el test (SKIP si no hay target):

```rust
mod support;

#[test]
fn previewer_demo_renderiza_y_loguea() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return; // target wasm ausente: SKIP (el helper ya avisó)
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    // fs-read=scoped: el host lee ÉL y pasa bytes; el guest no monta FS.
    let caps = norte_plugin_host::Capabilities::default();
    let mut inst = rt.instantiate(&wasm, caps).expect("instancia");
    let out = inst
        .render_preview("text/plain", b"linea uno\nlinea dos\nlinea tres\nlinea cuatro")
        .expect("render");
    assert!(out.contains("text/plain"), "cabecera del preview: {out}");
    assert!(out.contains("linea uno") && out.contains("linea tres"));
    assert!(!out.contains("linea cuatro"), "solo 3 líneas de head");
    // La puerta host-log funcionó.
    assert!(
        inst.logs().iter().any(|l| l.contains("previewer-demo")),
        "el plugin llamó a host-log: {:?}",
        inst.logs()
    );
}
```

- [ ] **Step 4: verde** — `cargo nextest run -p norte-plugin-host` (con el target instalado, ejecuta el componente REAL; sin él, salta ese test y el resto pasa). Si falla el build del guest, itera sobre los nombres del bindgen guest-side. Commit:

```bash
git add crates/norte-plugin-host/examples-wasm/previewer-demo crates/norte-plugin-host/src/runtime.rs crates/norte-plugin-host/tests/runtime.rs
git commit -m "feat(plugin-host): interfaz previewer end-to-end + guest de ejemplo (M4-P2 T5)"
```

rust-reviewer.

---

## Task 6: interfaz `command` end-to-end + guest de ejemplo

**Files:**
- Create: `crates/norte-plugin-host/examples-wasm/command-demo/` (Cargo.toml, src/lib.rs, wit/)
- Modify: `crates/norte-plugin-host/src/runtime.rs` (método `run_command`)
- Test: `crates/norte-plugin-host/tests/runtime.rs`

- [ ] **Step 1: el guest command.** Estructura idéntica a `previewer-demo` (Cargo.toml igual salvo `name = "command-demo"`; wit/ compartido). `src/lib.rs`:

```rust
#![no_std]
extern crate alloc;

use alloc::string::{String, ToString};

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
});

struct Demo;

impl exports::norte::plugin::command::Guest for Demo {
    fn run(id: String, arg: String) -> Result<String, String> {
        norte::plugin::host_log::log("command-demo: run");
        match id.as_str() {
            "echo" => Ok(arg),
            "shout" => Ok(arg.to_uppercase()),
            other => Err(alloc::format!("comando desconocido: {other}")),
        }
    }
}

impl exports::norte::plugin::previewer::Guest for Demo {
    fn render(_input: exports::norte::plugin::previewer::PreviewInput) -> Result<String, String> {
        Err("command-demo no aporta previews".to_string())
    }
}

export!(Demo);
```

(Necesita `use alloc::string::ToString;` y `alloc::format`; ajusta los `use` si el compilador se queja.)

- [ ] **Step 2: método `run_command` en el host** — añade a `PluginInstance`:

```rust
    /// Llama al export `command.run` del plugin.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] o [`RuntimeError::Guest`].
    pub fn run_command(&mut self, id: &str, arg: &str) -> Result<String, RuntimeError> {
        let result = self
            .bindings
            .norte_plugin_command()
            .call_run(&mut self.store, id, arg)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?;
        result.map_err(RuntimeError::Guest)
    }
```

- [ ] **Step 3: test** — en `tests/runtime.rs`:

```rust
#[test]
fn command_demo_ejecuta_y_reporta_error_de_comando() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return; // SKIP
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    assert_eq!(inst.run_command("echo", "hola").expect("echo"), "hola");
    assert_eq!(inst.run_command("shout", "hola").expect("shout"), "HOLA");
    // Un comando desconocido = Err del guest (no un trap).
    let err = inst
        .run_command("nope", "")
        .expect_err("comando desconocido");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("desconocido")),
        "fue {err:?}"
    );
}
```

- [ ] **Step 4: verde** — `cargo nextest run -p norte-plugin-host`. Commit:

```bash
git add crates/norte-plugin-host/examples-wasm/command-demo crates/norte-plugin-host/src/runtime.rs crates/norte-plugin-host/tests/runtime.rs
git commit -m "feat(plugin-host): interfaz command end-to-end + guest de ejemplo (M4-P2 T6)"
```

---

## Task 7: enforcement de capabilities + cierre

**Files:**
- Modify: `crates/norte-plugin-host/src/runtime.rs` (una puerta de host GATEADA de verdad)
- Test: `crates/norte-plugin-host/tests/runtime.rs`
- Modify: `docs/adr/0022-plugin-host-wasm-manifiesto.md` (addendum P2)
- Modify: memoria del proyecto

- [ ] **Step 1: demuestra el gating con una puerta gateada.** `host-log` es inocua; añade UNA puerta que SÍ dependa de una capability para probar el enforcement end-to-end. Amplía el WIT (`host-log` gana una función) — pero para no re-tocar el world, añade al `interface host-log` una función `read-scoped: func(token: string) -> result<list<u8>, string>` que SOLO responde si `caps.fs_read` es `Scoped`:

En `norte-plugin.wit`, dentro de `interface host-log`:

```wit
    /// Lee un recurso que el host abrió y referenció con `token` (fs-read=
    /// scoped). Si el plugin NO declaró fs-read, el host devuelve Err — el
    /// enforcement vive en el HOST, no en el guest.
    read-scoped: func(token: string) -> result<list<u8>, string>;
```

Implementación en `HostState` (host-side):

```rust
    fn read_scoped(&mut self, token: String) -> Result<Vec<u8>, String> {
        // ENFORCEMENT (ADR 0022 D4): sin la capability declarada, la puerta se
        // cierra en el HOST — da igual lo que el guest intente.
        if !self.caps.fs_read.granted() {
            return Err("fs-read no declarada".to_string());
        }
        // El "token" indexa recursos que el host preparó para ESTE plugin
        // (aquí un mapa de prueba; en producción, lo que el engine abra bajo
        // policy). Un token desconocido = Err (jamás acceso arbitrario).
        self.scoped_resources
            .get(&token)
            .cloned()
            .ok_or_else(|| "token desconocido".to_string())
    }
```

Añade `scoped_resources: std::collections::HashMap<String, Vec<u8>>` a `HostState` y un método `PluginInstance::preload_scoped(token, bytes)` para que el test siembre un recurso. (En P3/M3 esto lo alimenta el engine bajo policy; aquí basta el mapa.)

- [ ] **Step 2: guest que ejercita la puerta gateada** — reutiliza `command-demo`: añade un comando `read` que llama a `host_log::read_scoped("demo")` y devuelve los bytes como texto (o el error). Añade el brazo al `match id`:

```rust
            "read" => match norte::plugin::host_log::read_scoped("demo") {
                Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                Err(e) => Err(e),
            },
```

- [ ] **Step 3: test del enforcement** — el MISMO guest, dos capabilities:

```rust
#[test]
fn fs_read_scoped_gatea_la_puerta_en_el_host() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return; // SKIP
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");

    // 1) SIN fs-read: la puerta se cierra en el host aunque el guest la llame.
    let mut sin = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    sin.preload_scoped("demo", b"secreto".to_vec());
    let err = sin.run_command("read", "").expect_err("sin capability, denegado");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("fs-read")),
        "fue {err:?}"
    );

    // 2) CON fs-read=scoped: la puerta se abre y devuelve el recurso sembrado.
    let caps = norte_plugin_host::Capabilities::scoped_read_for_test();
    let mut con = rt.instantiate(&wasm, caps).expect("instancia");
    con.preload_scoped("demo", b"contenido").to_vec();
    assert_eq!(con.run_command("read", "").expect("read"), "contenido");
}
```

Añade `Capabilities::scoped_read_for_test()` (constructor `#[cfg(test)]`-friendly O `#[doc(hidden)] pub`) que devuelve `Capabilities { fs_read: Scope::Scoped, ..Default::default() }` — hazlo `pub` con `#[doc(hidden)]` porque el test es de integración (fuera del crate). Corrige el typo del test (`con.preload_scoped("demo", b"contenido".to_vec());`).

- [ ] **Step 4: verde total** — `just ci` COMPLETO (con el target instalado en la máquina de dev). Verifica que sin el target los tests de componente SALTAN y el resto queda verde: `rustup target remove wasm32-wasip2 && cargo nextest run -p norte-plugin-host && rustup target add wasm32-wasip2` (opcional pero recomendado: prueba que el skip funciona).

- [ ] **Step 5: addendum ADR + cierre.** En `docs/adr/0022-*.md`, una sección "## Addendum P2 (2026-…)": qué se implementó (runtime, sandbox WASI vacío, 2 interfaces, enforcement de fs-read en el host, ejemplos guest), y la DEUDA: las 3 interfaces restantes (provider/columns/hook), worlds por-categoría (para no exigir implementar interfaces ajenas), límite de memoria/CPU por store si quedó como deuda, integración con el policy engine M3 (hoy el sandbox aísla; el gating fino ask/allow/deny + journal se superpone), y el gestor de extensiones P3. Memoria: **M4-P2 COMPLETA**.

- [ ] **Step 6: Commit + security-reviewer OBLIGATORIO.**

```bash
git add -A
git commit -m "feat(plugin-host): enforcement de fs-read en el host + cierre M4-P2 (T7)"
```

**security-reviewer** (foco: (a) el `WasiCtxBuilder` vacío es realmente vacío — sin FS/red/env/stdio heredados; (b) el enforcement de capabilities vive en el HOST, no en el guest, y un guest hostil no lo evade; (c) los topes anti-DoS (logs, memoria por store); (d) el árbol de licencias de wasmtime que entró en T1; (e) `#![forbid(unsafe_code)]` intacto en el host).

---

## Riesgos / verificar

1. **Nombres de la API de wasmtime 46 / wasmtime-wasi 36**: `IoView`/`WasiView`, `add_to_linker_sync`, `WasiCtxBuilder`, el struct del world del `bindgen!`, `StoreLimits`. Confírmalos con `cargo doc`; el plan da la FORMA, no el import garantizado. Ningún paso está hecho hasta que compila.
2. **WIT compartido host↔guest**: el mismo `.wit` en `wit/` del host y de cada guest. Symlink relativo o test de igualdad — una divergencia da errores de tipo opacos del bindgen.
3. **`just ci` sin el target wasm**: los tests de componente DEBEN saltar (helper `build_guest → None`), jamás fallar. El gate en máquinas de CI/contribuidores sin `wasm32-wasip2` sigue verde; la de dev/nightly ejecuta el componente real. Documenta `rustup target add wasm32-wasip2`.
4. **World que exige todas las interfaces**: un componente que exporta `norte-plugin` debe exportar `previewer` Y `command`. Los ejemplos implementan ambas (la ajena = `Err`). Worlds por-categoría = deuda P2b anotada, no se resuelve aquí.
5. **Licencias**: si `deny` reporta `MPL-2.0` u otra copyleft-débil en el árbol de wasmtime, NO al allow global — excepción por-crate o parar y consultar a oscar.
6. **Nombre del `.wasm` del componente**: `wasm32-wasip2` produce un COMPONENTE (no un core module); confirma el nombre/ubicación tras el primer build y ajusta `build_guest`.
7. **Coste de build**: el árbol de wasmtime tarda minutos la primera vez; los guests recompilan en cada `cargo test` salvo caché — el `--target-dir` dedicado ayuda. Si el nightly se ralentiza, cachear el `.wasm` es deuda.
