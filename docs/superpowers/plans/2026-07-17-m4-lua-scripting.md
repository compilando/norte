# M4 Lua scripting (comandos + statusbar) — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `init.lua` del usuario define comandos (`lua:nombre` en keymap.toml) y un hook de statusbar; todo FS vía Backend→engine (journal/policy/undo); `./.norte/init.lua` solo con trust TOFU.

**Architecture:** módulo `norte-tui/src/lua/` (lib) con `mlua` async: el script ve API síncrona, el driver es un future !Send **polleado inline en el select! del main loop** (JAMÁS `tokio::spawn` — `#[tokio::main]` ejecuta el main task con `block_on`, Lua es `!Send`). Un comando en vuelo, cola FIFO. Spec: `docs/superpowers/specs/2026-07-17-m4-lua-scripting-design.md`.

**Tech Stack:** mlua (lua54, vendored, async, serialize), Backend (norte-core), patrón modal/TOFU existente (#45), `detail_for_bar` (#73).

**Convenciones de SIEMPRE (cada task):** test rojo primero → implementación → verde → `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --all` → commit convencional. Tests en español. Sin `unwrap` fuera de tests. Strings de UI por Fluent.

---

### Task 1: ADR 0026 + dep mlua + scaffold del módulo

**Files:**
- Create: `docs/adr/0026-lua-scripting-mlua.md`
- Modify: `Cargo.toml` (workspace), `crates/norte-tui/Cargo.toml`, `crates/norte-tui/src/lib.rs`
- Create: `crates/norte-tui/src/lua/mod.rs`

- [ ] **Step 1: ADR 0026** — formato MADR como `docs/adr/0025-*.md`. Contenido: contexto (spec §7.2, ADR 0022 tabla de niveles); opciones (A módulo TUI+mlua ELEGIDA / B crate aparte / C core); decisión con el modelo de seguridad: **sin sandbox, permisos del usuario, es config**; trust TOFU por `(path,hash)` para la capa proyecto; stdlib completa documentada (`io.*`/`os.*` sin journal — como un shell; `norte.fs.*` con journal/policy/undo); consecuencias (dep estructural mlua MIT+C vendorizado; futures !Send inline en el main loop). Copiar las justificaciones de la spec, sección «Decisiones».

- [ ] **Step 2: dep mlua**

En `Cargo.toml` del workspace, sección `[workspace.dependencies]`:
```toml
mlua = { version = "0.10", features = ["lua54", "vendored", "async", "serialize"] }
```
En `crates/norte-tui/Cargo.toml` bajo `[dependencies]`: `mlua.workspace = true`.
Run: `cargo build -p norte-tui && cargo deny check licenses 2>&1 | tail -3`
Expected: build OK; deny OK (mlua = MIT). Si la versión 0.10 no resuelve, usar la última publicada (`cargo add mlua -p norte-tui --features lua54,vendored,async,serialize` decide) y reflejarla en el ADR.

- [ ] **Step 3: scaffold** — `crates/norte-tui/src/lua/mod.rs`:
```rust
//! Scripting Lua (M4, spec §7.2 + ADR 0026): comandos de usuario y hook de
//! statusbar. SIN sandbox — es config del usuario, no software de terceros
//! (la capa proyecto pasa por trust TOFU, `trust.rs`). Todo FS vía
//! `Backend` → engine: journal + policy + undo. `io.*`/`os.*` crudos NO
//! dejan rastro (documentado; como un shell).

mod api;
mod driver;
mod statusbar;
mod trust;

pub use api::{LuaHost, LuaWarning};
pub use driver::{CommandRun, RunOutcome};
pub use statusbar::StatusInput;
pub use trust::{TrustDecision, TrustStore};
```
(los submódulos nacen vacíos con `//! TODO task N` — se rellenan en sus tasks; para que compile, crear cada fichero con su tipo mínimo público según las tasks siguientes, o comentar los `mod` aún no escritos y descomentarlos por task — elegir lo segundo, más honesto).
En `crates/norte-tui/src/lib.rs`: añadir `pub mod lua;`.

- [ ] **Step 4: Commit** — `feat(tui): ADR 0026 + dep mlua + scaffold del módulo lua (M4 Lua T1)`

---

### Task 2: `api.rs` — LuaHost y registro de comandos

**Files:**
- Create: `crates/norte-tui/src/lua/api.rs`
- Test: mod `#[cfg(test)]` en el propio fichero

- [ ] **Step 1: tests rojos**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> LuaHost {
        LuaHost::new().expect("lua arranca")
    }

    #[test]
    fn registra_y_lista_comandos() {
        let h = host();
        let w = h
            .eval_layer(b"norte.command('sel-up', function() end)", Layer::User)
            .expect("eval");
        assert!(w.is_empty());
        assert_eq!(h.commands(), vec!["sel-up".to_string()]);
    }

    #[test]
    fn nombre_invalido_es_error_de_carga() {
        let h = host();
        // Mayúsculas, espacios, vacío, >64: fuera (charset [a-z0-9._-]{1,64}).
        for bad in ["'Mal'", "'con espacio'", "''", &format!("'{}'", "a".repeat(65))] {
            let src = format!("norte.command({bad}, function() end)");
            assert!(h.eval_layer(src.as_bytes(), Layer::User).is_err(), "{bad}");
        }
    }

    #[test]
    fn duplicado_en_la_misma_capa_es_error() {
        let h = host();
        let src = b"norte.command('x', function() end)\nnorte.command('x', function() end)";
        assert!(h.eval_layer(src, Layer::User).is_err());
    }

    #[test]
    fn capa_posterior_pisa_con_warning() {
        let h = host();
        h.eval_layer(b"norte.command('x', function() end)", Layer::System)
            .expect("sistema");
        let w = h
            .eval_layer(b"norte.command('x', function() end)", Layer::User)
            .expect("usuario");
        assert_eq!(w.len(), 1, "warning de pisado");
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn un_error_de_evaluacion_no_envenena_el_host() {
        let h = host();
        assert!(h.eval_layer(b"esto no es lua (", Layer::System).is_err());
        h.eval_layer(b"norte.command('ok', function() end)", Layer::User)
            .expect("la capa siguiente carga");
        assert_eq!(h.commands(), vec!["ok".to_string()]);
    }
}
```

- [ ] **Step 2: correr y ver rojo** — `cargo nextest run -p norte-tui lua::api` → FAIL compilación (tipos no existen).

- [ ] **Step 3: implementación**

```rust
//! `LuaHost`: estado Lua + registro de comandos/statusbar. Se reconstruye
//! ENTERO en hot-reload (jamás estado a medias); un comando en vuelo retiene
//! el estado viejo vía sus handles clonados (mlua es un handle Rc).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, Lua};

/// Capa de origen de un `init.lua` (precedencia ASCENDENTE, ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// `/etc/norte` (o ProgramData).
    System,
    /// `~/.config/norte`.
    User,
    /// `./.norte` — SOLO tras trust (task 6).
    Project,
}

/// Aviso no-fatal de carga (se muestra por barra, no aborta).
#[derive(Debug, Clone)]
pub struct LuaWarning {
    /// Clave Fluent + detalle ya saneable por el caller.
    pub detail: String,
}

#[derive(Default)]
struct Registry {
    commands: HashMap<String, (Layer, Function)>,
    statusbar: Option<Function>,
}

/// El anfitrión Lua del TUI. `!Send` — vive en el main task.
pub struct LuaHost {
    lua: Lua,
    registry: Rc<RefCell<Registry>>,
}

/// Charset de nombres de comando (mismo espíritu que `agent_session`):
/// `[a-z0-9._-]{1,64}`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

impl LuaHost {
    /// Estado Lua nuevo con stdlib COMPLETA (decisión de la spec) y la tabla
    /// `norte` inyectada. La tabla `norte.fs`/`norte.pane` se añade en la
    /// task 4 (necesita Backend/ctx).
    ///
    /// # Errors
    /// Fallo de arranque de Lua (sin memoria; no ocurre en la práctica).
    pub fn new() -> mlua::Result<Self> {
        let lua = Lua::new();
        let registry = Rc::new(RefCell::new(Registry::default()));
        let host = Self { lua, registry };
        host.install_norte_table()?;
        Ok(host)
    }

    fn install_norte_table(&self) -> mlua::Result<()> {
        let norte = self.lua.create_table()?;
        let ui = self.lua.create_table()?;
        norte.set("ui", ui)?;
        self.lua.globals().set("norte", norte)?;
        Ok(())
    }

    /// Evalúa un `init.lua` de una capa. `norte.command` registra contra la
    /// capa ACTIVA: duplicado en la misma capa = error; capa posterior pisa
    /// con warning. Un error deja el registro como estaba ANTES de la capa
    /// (se evalúa contra un staging y se aplica solo si OK).
    ///
    /// # Errors
    /// Sintaxis/runtime del script, o registro inválido (nombre, duplicado).
    pub fn eval_layer(&self, source: &[u8], layer: Layer) -> Result<Vec<LuaWarning>, LuaLoadError> {
        // Staging: los registros de ESTA capa; se funde al final si todo OK.
        let staged: Rc<RefCell<HashMap<String, Function>>> = Rc::default();
        let warnings: Rc<RefCell<Vec<LuaWarning>>> = Rc::default();
        let norte: mlua::Table = self.lua.globals().get("norte")?;
        {
            let staged = Rc::clone(&staged);
            let command = self
                .lua
                .create_function(move |_, (name, f): (String, Function)| {
                    if !valid_name(&name) {
                        return Err(mlua::Error::RuntimeError(format!(
                            "nombre de comando inválido: {name:?} (esperado [a-z0-9._-]{{1,64}})"
                        )));
                    }
                    if staged.borrow().contains_key(&name) {
                        return Err(mlua::Error::RuntimeError(format!(
                            "comando duplicado en la misma capa: {name}"
                        )));
                    }
                    staged.borrow_mut().insert(name, f);
                    Ok(())
                })?;
            norte.set("command", command)?;
        }
        self.lua
            .load(source)
            .set_name(match layer {
                Layer::System => "init.lua (sistema)",
                Layer::User => "init.lua (usuario)",
                Layer::Project => "init.lua (proyecto)",
            })
            .exec()?;
        // Fusión: capa posterior pisa a la anterior con warning.
        let mut reg = self.registry.borrow_mut();
        for (name, f) in staged.take() {
            if let Some((prev_layer, _)) = reg.commands.get(&name)
                && *prev_layer < layer
            {
                warnings.borrow_mut().push(LuaWarning {
                    detail: format!("comando {name} redefinido por una capa posterior"),
                });
            }
            reg.commands.insert(name, (layer, f));
        }
        Ok(warnings.take())
    }

    /// Nombres registrados, orden estable (para conflictos/palette futura).
    #[must_use]
    pub fn commands(&self) -> Vec<String> {
        let mut v: Vec<String> = self.registry.borrow().commands.keys().cloned().collect();
        v.sort();
        v
    }
}

/// Error de carga de una capa.
#[derive(Debug, thiserror::Error)]
pub enum LuaLoadError {
    /// Sintaxis, runtime o registro inválido.
    #[error(transparent)]
    Lua(#[from] mlua::Error),
}
```

Nota de implementación: si `mlua::Error::RuntimeError` con mensaje castellano molesta (va al detalle de la barra saneado, no a Fluent — aceptado v1, igual que el diagnóstico TOML), no cambiarlo ahora.

- [ ] **Step 4: verde** — `cargo nextest run -p norte-tui lua::api` → PASS (5 tests).
- [ ] **Step 5: Commit** — `feat(tui): LuaHost — registro de comandos por capas (M4 Lua T2)`

---

### Task 3: `Backend` — `Clone` + `stat` (norte-core)

**Files:**
- Modify: `crates/norte-core/src/backend.rs`
- Test: `crates/norte-tui/tests/tasks.rs` NO — test en `crates/norte-core` junto a los del backend si existen; si no, doctest/unit en `backend.rs`

- [ ] **Step 1: test rojo** (en `backend.rs`, mod tests existente o nuevo):
```rust
#[tokio::test]
async fn backend_clonado_comparte_engine_y_stat_funciona() {
    use norte_testkit::MemProvider;
    let engine = crate::Engine::new();
    let mem = std::sync::Arc::new(MemProvider::new());
    engine.register_provider(std::sync::Arc::clone(&mem) as std::sync::Arc<dyn norte_vfs::Provider>);
    let b = Backend::Embedded(std::sync::Arc::new(engine));
    // write vía provider directo (fixture), stat vía backend CLONADO.
    let mut sink = mem.write(&norte_proto::VPath::parse("mem:///f").unwrap()).await.unwrap();
    sink.write(bytes::Bytes::from_static(b"x")).await.unwrap();
    sink.commit().await.unwrap();
    let b2 = b.clone();
    let e = b2.stat(&norte_proto::VPath::parse("mem:///f").unwrap()).await.unwrap();
    assert_eq!(e.size, Some(1));
}
```
(ajustar imports/campos de `Entry` a los reales; norte-testkit ya es dev-dep de norte-core — verificar, si no añadirlo.)

- [ ] **Step 2: rojo** — `cargo nextest run -p norte-core backend_clonado` → FAIL (no `Clone`, no `stat`).

- [ ] **Step 3: implementación** en `backend.rs`:
```rust
impl Clone for Backend {
    /// Clon BARATO: comparte engine/conexión. OJO: los canales one-shot
    /// (`take_foreign_tasks` etc.) son del PRIMER dueño — un clon para
    /// scripting/lua no debe llamarlos (rustdoc de cada `take_*`).
    fn clone(&self) -> Self {
        match self {
            Self::Embedded(e) => Self::Embedded(Arc::clone(e)),
            #[cfg(unix)]
            Self::Remote(r) => Self::Remote(r.clone()),
        }
    }
}
```
y el método (calcar la forma de `capabilities`):
```rust
/// Metadatos de un nodo (`fs.stat`).
///
/// # Errors
/// Taxonomía del protocolo.
pub async fn stat(&self, path: &VPath) -> Result<Entry, Error> {
    match self {
        Self::Embedded(engine) => engine.stat(path).await,
        #[cfg(unix)]
        Self::Remote(r) => r.stat(path).await,
    }
}
```
y en el mod `remote`, junto a `capabilities` (calcar con `FsStatParams`/`FsStatResult` de proto — verificar nombres exactos en `norte-proto/src/methods.rs:281`):
```rust
pub(super) async fn stat(&self, path: &VPath) -> Result<Entry, Error> {
    let r: FsStatResult = self
        .call_timed(methods::FS_STAT, &FsStatParams { path: path.clone() })
        .await?;
    Ok(r.entry)
}
```

- [ ] **Step 4: verde** — `cargo nextest run -p norte-core backend` → PASS. `cargo clippy -p norte-core --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `feat(core): Backend Clone + stat (M4 Lua T3)`

---

### Task 4: `norte.fs` / `norte.pane` / `norte.ui.message` — bindings

**Files:**
- Create: `crates/norte-tui/src/lua/fs.rs` (añadir `mod fs;` en `lua/mod.rs`)
- Modify: `crates/norte-tui/src/lua/api.rs` (instalar tabla fs/pane con ctx)
- Test: `crates/norte-tui/tests/lua_fs.rs`

Diseño fino: la API fs/pane NO se instala en `LuaHost::new()` — se instala **por invocación** (task 5) porque necesita el snapshot `PaneCtx` y el token del run. Este task construye las piezas: `PaneCtx`, `install_fs(lua, backend, ctx, cancellers) -> Table` y la conversión bytes↔VPath.

- [ ] **Step 1: tests rojos** (`tests/lua_fs.rs`, patrón `backend_mem()` de `tests/tasks.rs`):

```rust
//! Bindings norte.fs sobre Engine+MemProvider real: byte strings, journal.

use std::sync::Arc;
use bytes::Bytes;
use norte_core::backend::Backend;
use norte_core::Engine;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_tui::lua::{LuaHost, PaneCtx};
use norte_vfs::Provider;

fn vp(w: &str) -> VPath { VPath::parse(w).expect("wire") }

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
}

fn backend_mem() -> (Backend, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (Backend::Embedded(Arc::new(engine)), mem)
}

fn ctx(cwd: &str) -> PaneCtx {
    PaneCtx {
        cwd: vp(cwd),
        other_cwd: vp(cwd),
        selection: vec![],
        current: None,
    }
}

#[tokio::test]
async fn copy_list_stat_desde_lua() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"datos").await;
    let h = LuaHost::new().expect("lua");
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                assert(norte.fs.copy("mem:///a", "mem:///b"))
                local e = assert(norte.fs.stat("mem:///b"))
                assert(e.size == 5, "size")
                local l = assert(norte.fs.list("mem:///"))
                return #l
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 2);
}

#[tokio::test]
async fn bytes_no_utf8_round_trip() {
    let (backend, mem) = backend_mem();
    // Nombre con bytes 0xFF 0xFE: por el wire es percent-encoding.
    write_file(&mem, "mem:///%FF%FE", b"x").await;
    let h = LuaHost::new().expect("lua");
    // list devuelve el NOMBRE como byte string; copy con ese byte string
    // (concatenado en Lua) funciona — cero suposición UTF-8 (regla 1).
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                local l = assert(norte.fs.list("mem:///"))
                assert(#l == 1)
                local name = l[1].name
                assert(#name == 2, "dos bytes crudos")
                assert(norte.fs.copy("mem:///" .. name, "mem:///copia"))
                return true
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1); // true → 1 en la conversión del harness
}

#[tokio::test]
async fn error_del_protocolo_llega_como_nil_categoria() {
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    let out = h
        .run_script_for_test(
            backend,
            ctx("mem:///"),
            br#"
                local ok, err = norte.fs.stat("mem:///no-existe")
                assert(ok == nil and type(err) == "string")
                return true
            "#,
        )
        .await
        .expect("script ok");
    assert_eq!(out, 1);
}

#[tokio::test]
async fn pane_expone_el_snapshot() {
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    let mut c = ctx("mem:///");
    c.selection = vec![vp("mem:///a"), vp("mem:///b")];
    let out = h
        .run_script_for_test(backend, c, br#"return #norte.pane.selection()"#)
        .await
        .expect("script ok");
    assert_eq!(out, 2);
}
```

`run_script_for_test(backend, ctx, src) -> Result<i64, …>` es un helper `#[doc(hidden)] pub` de `LuaHost` que instala fs/pane con un token nunca-cancelado, ejecuta el chunk async y devuelve el retorno como entero (true=1). Existirá SOLO para tests (el camino real es `invoke`, task 5) pero es API honesta: mismo `install_fs`.

- [ ] **Step 2: rojo** — `cargo nextest run -p norte-tui --test lua_fs` → FAIL compilación.

- [ ] **Step 3: implementación** (`lua/fs.rs`):

```rust
//! Bindings `norte.fs`/`norte.pane`/`norte.ui.message`. Paths = BYTE STRINGS
//! de Lua en entrada y salida (regla 1: cero suposición UTF-8). Relativos se
//! resuelven contra `ctx.cwd`. Toda mutación es una Task del engine: journal
//! + policy + undo. Errores del protocolo → `nil, categoria` (convención Lua).

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, Table};
use norte_core::backend::{Backend, TaskCanceller};
use norte_proto::{DeleteMode, Error, TaskState, VPath};

/// Snapshot del estado de panes al INVOCAR el comando (congelado: la UI
/// sigue mutando mientras el script corre; determinismo > frescura).
#[derive(Debug, Clone)]
pub struct PaneCtx {
    pub cwd: VPath,
    pub other_cwd: VPath,
    pub selection: Vec<VPath>,
    pub current: Option<VPath>,
}

/// Cancellers de las Tasks lanzadas por ESTE run (el driver los cancela
/// todos si el usuario aborta — regla 3).
pub type RunCancellers = Rc<RefCell<Vec<TaskCanceller>>>;

/// bytes de Lua → VPath. Absoluto si parsea como wire con scheme; si no,
/// relativo a `base` (join de segmento). Inválido = err string (categoría).
fn to_vpath(base: &VPath, raw: &[u8]) -> Result<VPath, String> { /* … */ }

/// VPath → byte string de Lua con el path COMPLETO en forma wire;
/// `entry.name` = bytes crudos del último segmento.
pub(crate) fn install_fs(
    lua: &Lua,
    backend: Backend,
    ctx: PaneCtx,
    cancellers: RunCancellers,
) -> mlua::Result<()> { /* crea norte.fs/norte.pane/norte.ui.message.
    Cada fn de fs:
      - lectura (list/stat): create_async_function; Ok(valor) o (nil, cat).
      - mutación (copy/move/delete/mkdir):
          let task = backend.copy(...).await  →  cancellers.push(task.canceller())
          → task.join().await  →  Completed=true / Cancelled=(nil,"cancelled")
          / Failed=(nil,categoria).
      - delete = DeleteMode::Trash SIEMPRE (permanente no expuesto v1).
      - mkdir: Backend NO tiene mkdir → si Engine tampoco lo expone por
        Backend, QUITAR mkdir de v1 y anotarlo en la spec (desviación
        documentada) — verificar `grep -n "mkdir" crates/norte-core/src/backend.rs`.
    norte.pane.*: funciones síncronas sobre el clone de ctx.
    norte.ui.message(s): encola en un Rc<RefCell<Vec<String>>> compartido que
    el driver vuelca a la barra (el App no es accesible desde aquí). */ }
```
Los errores→categoría: reutilizar `norte_proto::Error` → string estable con la MISMA función `error_category` no es accesible desde lib (está en main.rs); mover `error_category` de `main.rs` a `norte_tui::app` (pub) en este task — main.rs la reimporta (mismo comportamiento, tests de main.rs siguen). El binding devuelve la CLAVE (`err-not-found`) no el texto localizado; el script compara contra claves estables.

- [ ] **Step 4: verde** — `cargo nextest run -p norte-tui --test lua_fs` → PASS (4).
- [ ] **Step 5: verificar journal**: añadir al test `copy_list_stat_desde_lua` un assert de que el copy dejó rastro: con `Backend::Embedded`, `engine` con observer de test si el patrón existe barato en `tests/tasks.rs`; si no, cubierto por E2E (task 9) — no inventar infraestructura nueva aquí.
- [ ] **Step 6: Commit** — `feat(tui): bindings norte.fs/pane/ui sobre Backend (M4 Lua T4)`

---

### Task 5: `driver.rs` — invoke, cola FIFO, cancelación, timeout

**Files:**
- Create: `crates/norte-tui/src/lua/driver.rs`
- Modify: `crates/norte-tui/src/lua/api.rs` (método `invoke`)
- Test: `crates/norte-tui/tests/lua_driver.rs`

- [ ] **Step 1: tests rojos**

```rust
//! Driver: el future del comando se pollea inline; cancelación limpia.

use std::time::Duration;
// … imports como lua_fs.rs …

#[tokio::test]
async fn invoke_ejecuta_y_reporta_ok() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"x").await;
    let h = LuaHost::new().expect("lua");
    h.eval_layer(
        b"norte.command('dup', function() assert(norte.fs.copy('mem:///a', 'mem:///b')) end)",
        norte_tui::lua::Layer::User,
    )
    .expect("carga");
    let token = tokio_util::sync::CancellationToken::new();
    let run = h.invoke("dup", backend, ctx("mem:///"), token).expect("existe");
    let outcome = run.await;
    assert!(matches!(outcome, norte_tui::lua::RunOutcome::Ok { .. }));
    assert!(mem.stat(&vp("mem:///b")).await.is_ok(), "el copy ocurrió");
}

#[tokio::test]
async fn invoke_desconocido_es_none() {
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    assert!(h
        .invoke("nadie", backend, ctx("mem:///"), Default::default())
        .is_none());
}

#[tokio::test]
async fn cancelar_mata_el_script_y_sus_tasks() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"x").await;
    let h = LuaHost::new().expect("lua");
    // Script que copia y luego SE QUEDA en bucle Lua puro (sin puntos await):
    // el token debe matarlo vía hook de instrucciones, no solo por await.
    h.eval_layer(
        b"norte.command('loop', function() assert(norte.fs.copy('mem:///a', 'mem:///b')) while true do end end)",
        norte_tui::lua::Layer::User,
    )
    .expect("carga");
    let token = tokio_util::sync::CancellationToken::new();
    let run = h.invoke("loop", backend, ctx("mem:///"), token.clone()).expect("existe");
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    };
    let (outcome, ()) = tokio::join!(run, cancel);
    assert!(matches!(outcome, norte_tui::lua::RunOutcome::Cancelled));
}

#[tokio::test]
async fn timeout_duro_abandona() {
    // Igual que el anterior pero SIN cancelar y con timeout corto inyectado.
    let (backend, _mem) = backend_mem();
    let h = LuaHost::new().expect("lua");
    h.eval_layer(b"norte.command('loop', function() while true do end end)", norte_tui::lua::Layer::User)
        .expect("carga");
    let run = h
        .invoke_with_timeout("loop", backend, ctx("mem:///"), Default::default(), Duration::from_millis(200))
        .expect("existe");
    assert!(matches!(run.await, norte_tui::lua::RunOutcome::TimedOut));
}
```

- [ ] **Step 2: rojo.**

- [ ] **Step 3: implementación** (`driver.rs`):

```rust
//! Ejecución de un comando: future !Send que el main loop pollea inline
//! (JAMÁS tokio::spawn). Cancelación en dos frentes (regla 3):
//! 1. las Tasks del engine lanzadas por el run (cancellers registrados);
//! 2. el propio script, vía hook de instrucciones (mata bucles Lua puros).
//! Un script clavado en C (os.execute) no responde a ninguno: timeout duro
//! y el driver ABANDONA (el estado Lua se tira en el próximo reload).

pub enum RunOutcome {
    Ok { messages: Vec<String> },
    Err { detail: String, messages: Vec<String> },
    Cancelled,
    TimedOut,
}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

impl LuaHost {
    pub fn invoke(&self, name, backend, ctx, token) -> Option<CommandRun> {
        self.invoke_with_timeout(name, backend, ctx, token, DEFAULT_TIMEOUT)
    }
    pub fn invoke_with_timeout(&self, …) -> Option<CommandRun> {
        let f = self.registry.borrow().commands.get(name)?.1.clone();
        let lua = self.lua.clone();          // handle Rc barato
        let cancellers: RunCancellers = Rc::default();
        let messages: Rc<RefCell<Vec<String>>> = Rc::default();
        fs::install_fs(&lua, backend, ctx, Rc::clone(&cancellers))…;
        Some(CommandRun { /* future async move:
            tokio::select! {
                biased;
                r = f.call_async::<()>(()) => Ok/Err según r (+ messages),
                () = token.cancelled() => {
                    for c in cancellers.borrow().iter() { c.cancel(); }
                    lua.set_hook(HookTriggers::new().every_nth_instruction(1),
                        |_, _| Err(mlua::Error::RuntimeError("cancelled".into())));
                    // gracia acotada: la corrutina muere al siguiente paso Lua
                    match tokio::time::timeout(Duration::from_secs(5), f2_continue).await { … }
                    RunOutcome::Cancelled
                }
                () = tokio::time::sleep(timeout) => RunOutcome::TimedOut  // abandona
            }
            al salir SIEMPRE lua.remove_hook();
        */ })
    }
}
```
Detalle real de la gracia: no se puede "continuar" un `call_async` ya seleccionado-fuera — estructura correcta: pin el future del call fuera del select y usar `select!` en LOOP: brazo call completa → outcome; brazo token (una vez) → cancelar tasks + set_hook y SEGUIR el loop esperando el call (que ahora muere solo) con deadline de gracia; brazo deadline → `TimedOut`/`Cancelled` abandonando el future (drop al salir). El executor debe escribirlo así, con el call pineado (`let mut call = std::pin::pin!(f.call_async::<()>(()));`).

- [ ] **Step 4: verde** — 4 tests PASS. El de cancelación verifica además destino limpio si aplica (`mem.stat("mem:///b")` puede existir — la copia PUDO completar antes del cancel; no assertar sobre b, solo sobre el outcome — quitar asserts frágiles).
- [ ] **Step 5: Commit** — `feat(tui): driver Lua — invoke con cancelación en dos frentes y timeout (M4 Lua T5)`

---

### Task 6: `trust.rs` — TOFU del init.lua de proyecto

**Files:**
- Create: `crates/norte-tui/src/lua/trust.rs`
- Test: mod tests en el fichero (tempfile ya es dev-dep)

- [ ] **Step 1: tests rojos**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desconocido_pregunta_aprobado_carga_denegado_persiste() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let content = b"norte.command('x', function() end)";
        assert_eq!(store.check("/repo/.norte/init.lua", content), TrustDecision::Unknown);
        store.record("/repo/.norte/init.lua", content, true).unwrap();
        assert_eq!(store.check("/repo/.norte/init.lua", content), TrustDecision::Trusted);
        // Contenido distinto = hash distinto = re-preguntar.
        assert_eq!(store.check("/repo/.norte/init.lua", b"otro"), TrustDecision::Unknown);
        // Denegado persiste (no re-preguntar hasta cambiar).
        store.record("/repo/.norte/init.lua", b"otro", false).unwrap();
        assert_eq!(store.check("/repo/.norte/init.lua", b"otro"), TrustDecision::Denied);
    }

    #[test]
    fn el_store_reabre_lo_persistido() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        TrustStore::open(p.clone()).unwrap().record("/x/.norte/init.lua", b"c", true).unwrap();
        let store = TrustStore::open(p).unwrap();
        assert_eq!(store.check("/x/.norte/init.lua", b"c"), TrustDecision::Trusted);
    }

    #[cfg(unix)]
    #[test]
    fn el_fichero_nace_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        TrustStore::open(p.clone()).unwrap().record("/x", b"c", true).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
```

- [ ] **Step 2: rojo.**

- [ ] **Step 3: implementación**: sha256 (`sha2` ya en el árbol — añadir a norte-tui deps `sha2.workspace = true`), TOML plano `[[entry]] path/hash/allow/date`, escritura atómica (write temp + rename), 0600 unix al crear (calcar `KnownHostsStore` de norte-connect: `grep -n "0o600" crates/norte-connect/src/known_hosts.rs`). El path clave = **canónico** (`std::fs::canonicalize` del dir del init.lua; si falla, no-trust). API:
```rust
pub enum TrustDecision { Trusted, Denied, Unknown }
pub struct TrustStore { path: PathBuf, entries: Vec<Entry> }
impl TrustStore {
    pub fn open(path: PathBuf) -> std::io::Result<Self>;
    pub fn check(&self, script_path: &str, content: &[u8]) -> TrustDecision;
    pub fn record(&mut self, script_path: &str, content: &[u8], allow: bool) -> std::io::Result<()>;
}
```
El hash SE CALCULA SOBRE LOS BYTES LEÍDOS UNA VEZ: el caller (task 7) lee el fichero, llama `check(bytes)`, y si se aprueba evalúa ESOS MISMOS bytes (anti-TOCTOU — anclar con comentario).
El I/O del store es síncrono pero corre en arranque/reload — envolver las llamadas del caller en `spawn_blocking` (regla 2) en task 7.

- [ ] **Step 4: verde.** `cargo nextest run -p norte-tui lua::trust` → PASS (3).
- [ ] **Step 5: Commit** — `feat(tui): trust store TOFU del init.lua de proyecto (M4 Lua T6)`

---

### Task 7: `statusbar.rs` — hook con presupuesto

**Files:**
- Create: `crates/norte-tui/src/lua/statusbar.rs`
- Modify: `crates/norte-tui/src/lua/api.rs` (registrar `norte.ui.statusbar`)
- Test: mod tests en el fichero

- [ ] **Step 1: tests rojos**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::{Layer, LuaHost};

    fn input() -> StatusInput {
        StatusInput { cwd: b"mem:///d".to_vec(), selected: 2, selected_bytes: 10, entries: 5, tasks: 0 }
    }

    #[test]
    fn hook_pinta_y_cachea() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(b"norte.ui.statusbar(function(s) return s.selected .. ' sel' end)", Layer::User).unwrap();
        assert_eq!(h.statusbar(&input()).as_deref(), Some("2 sel"));
        // Mismo input = cache (se comprueba que una función con contador
        // global de Lua solo corre una vez para el mismo snapshot).
        let h2 = LuaHost::new().unwrap();
        h2.eval_layer(b"n = 0; norte.ui.statusbar(function(s) n = n + 1 return tostring(n) end)", Layer::User).unwrap();
        assert_eq!(h2.statusbar(&input()).as_deref(), Some("1"));
        assert_eq!(h2.statusbar(&input()).as_deref(), Some("1"), "cacheado");
    }

    #[test]
    fn presupuesto_excedido_deshabilita_sin_panic() {
        let h = LuaHost::new().unwrap();
        h.eval_layer(b"norte.ui.statusbar(function() while true do end end)", Layer::User).unwrap();
        assert_eq!(h.statusbar(&input()), None, "excedido → None + deshabilitado");
        assert_eq!(h.statusbar(&input()), None, "sigue deshabilitado");
        assert!(h.statusbar_error().is_some(), "el error queda para la barra");
    }

    #[test]
    fn salida_hostil_enmascarada() {
        let h = LuaHost::new().unwrap();
        h.eval_layer("norte.ui.statusbar(function() return 'a\u{202E}b' end)".as_bytes(), Layer::User).unwrap();
        let s = h.statusbar(&input()).unwrap();
        assert!(!s.contains('\u{202E}'), "sin bidi: {s}");
    }
}
```

- [ ] **Step 2: rojo.**

- [ ] **Step 3: implementación**: `StatusInput` (`PartialEq + Clone`) con los 5 campos; `LuaHost::statusbar(&self, input)`:
  - cache `RefCell<Option<(StatusInput, Option<String>)>>` — mismo input = respuesta cacheada.
  - hook: `lua.set_hook(HookTriggers::new().every_nth_instruction(50_000), |_,_| Err(RuntimeError("statusbar budget".into())))`, llamada SÍNCRONA `f.call::<String>(tabla)`, `lua.remove_hook()` SIEMPRE (incluso en Err — usar un guard o try/finally manual).
  - error (presupuesto o runtime o llamar API async → mlua da error de "attempt to yield") → deshabilita: `statusbar_disabled = true`, guarda `statusbar_error: Option<String>` para que el wiring lo muestre UNA vez.
  - salida por `crate::app::display_name(bytes).0` + tope (usar `detail_for_bar`... está en main.rs → al mover `error_category` en task 4, mover TAMBIÉN `detail_for_bar` y `DETAIL_MAX_CHARS` a `app.rs` pub; main.rs reimporta).
  - la tabla del input: `cwd` como byte string.

- [ ] **Step 4: verde.** — PASS (3).
- [ ] **Step 5: Commit** — `feat(tui): hook Lua de statusbar con presupuesto de instrucciones (M4 Lua T7)`

---

### Task 8: wiring TUI — carga, modal trust, keymap `lua:`, Esc, ftl

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (run loop, dispatch, reload_config)
- Modify: `crates/norte-tui/src/app.rs` (`Modal::TrustLuaInit`, campos lua en App)
- Modify: `crates/norte-tui/src/ui.rs` (`draw_modal` brazo nuevo, `draw_status` usa hook)
- Modify: `crates/norte-tui/src/keymap.rs` (aceptar `lua:*` en validación)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl`
- Test: `crates/norte-tui/tests/snapshots_ui.rs` (snapshot modal), tests de keymap en `keymap.rs`

- [ ] **Step 1: test rojo keymap** (en keymap.rs, junto a los existentes):
```rust
#[test]
fn lua_prefijado_pasa_la_validacion_de_comandos() {
    // Un binding a `lua:mi-comando` es válido aunque no esté en COMMANDS;
    // charset del nombre validado; `lua:` con nombre ilegal NO pasa.
    // (montar un KeymapFile mínimo con append_keymap → build_for OK / err,
    // calcando el patrón de los tests de validación existentes en este mod)
}
```
Implementar en la validación de comandos de `Effective::build_for` (donde se compara contra `known_commands`): aceptar también `cmd.strip_prefix("lua:")` con el charset `[a-z0-9._-]{1,64}` (duplicar `valid_name` aquí o exponerlo desde `norte_tui::lua`). Conflicto de nombre inexistente en runtime (comando lua no registrado) NO es error de keymap: al invocar, barra `err-lua-unknown`.

- [ ] **Step 2: claves Fluent** (en + es), añadir tras el bloque `err-*`:
```
err-lua-load = init.lua ({ $layer }): { $detail }
err-lua-unknown = unknown Lua command: { $name }
err-lua-command = Lua command failed: { $detail }
err-lua-cancelled = Lua command cancelled
err-lua-timeout = Lua command timed out
err-lua-statusbar = Lua statusbar disabled: { $detail }
msg-lua-busy = a Lua command is already running (queued)
modal-lua-trust-title = Run project init.lua?
modal-lua-trust-body = { $path } (sha256 { $hash }) wants to run WITH YOUR PERMISSIONS. Scripts from a cloned repo can do anything you can. y = trust and run, n/Esc = deny (remembered until the file changes).
```
(es: traducción equivalente; mantener y/n literales.)

- [ ] **Step 3: App + Modal**: en `app.rs`:
```rust
pub enum Modal {
    …
    /// TOFU del init.lua de PROYECTO (M4 Lua): path saneado + hash corto.
    TrustLuaInit { path: String, hash_abbrev: String },
}
```
En `App`: `pub lua_pending_trust: Option<Vec<u8>>` (los bytes leídos, para evaluar EXACTAMENTE lo aprobado — anti-TOCTOU).
`dialog_key`/outcome: `y` = Trust, `n`/`Esc` = Deny, Enter NO aprueba (calcar `ApproveAgentOp`).

- [ ] **Step 4: carga en `run()` y `reload_config`**: función nueva en main.rs:
```rust
/// Construye el LuaHost desde las capas (spawn_blocking para el I/O, regla
/// 2): sistema y usuario evalúan directo; proyecto consulta el TrustStore —
/// Unknown deja el modal pendiente (los bytes quedan en App), Denied no
/// carga, Trusted evalúa. Errores/warnings → barra por categoría.
async fn load_lua(app: &mut App, layers: &Layers) -> LuaHost { … }
```
En hot-reload: reconstruir el host entero (`*lua_host = load_lua(…)`); el `CommandRun` en vuelo sigue con su estado viejo (retiene handles) — no tocarlo.
Al resolver el modal Trust: `record` en el store (spawn_blocking) y, si allow, `eval_layer(bytes, Layer::Project)`.
El TrustStore vive en `$XDG_STATE_HOME/norte/lua-trust.toml` (fallback `~/.local/state/norte`): función `state_dir()` — calcar la resolución que ya haga norte (grep `XDG_STATE_HOME`/`data_dir` en el workspace; si no hay precedente, implementarla aquí con rustdoc).

- [ ] **Step 5: dispatch + Esc + drive en el select!**: en `run()`:
  - `Resolution::Run(cmd)` (main.rs:452): brazo nuevo ANTES del match de comandos fijos:
```rust
if let Some(name) = cmd.strip_prefix("lua:") {
    match (&app.lua_run, app.lua_queue.len()) {
        (Some(_), n) if n >= 8 => app.message = Some(t("msg-lua-busy")),
        (Some(_), _) => { app.lua_queue.push_back(name.to_owned()); app.message = Some(t("msg-lua-busy")); }
        (None, _) => start_lua_run(app, backend, name),
    }
    continue; // o el flujo equivalente del loop
}
```
    `start_lua_run` captura `PaneCtx` del estado ACTUAL de los panes, crea token, `host.invoke(…)` → `app.lua_run = Some(pin!(run))`; desconocido → `err-lua-unknown`.
  - En el `tokio::select!` principal del loop: brazo `outcome = &mut run, if app.lua_run.is_some()` → volcar `messages` a la barra, mostrar outcome (`err-lua-command`/`err-lua-cancelled`/`err-lua-timeout` o nada si Ok), `app.lua_run = None`, arrancar siguiente de la cola.
  - Esc con `lua_run` activo (y sin modal/overlay abierto): `token.cancel()` (guardar el token junto al future). Ver cómo Esc se enruta hoy (Resolution o tecla fija) y colgar ahí SIN robar el Esc de modales/viewer.
  - `draw_status`: si `app.lua_status: Option<String>` es Some, pintarla (línea del pane con foco); recomputarla en cada vuelta del loop: `app.lua_status = host.statusbar(&snapshot_actual)`; error de statusbar (una vez) → barra.

- [ ] **Step 6: snapshot del modal**: en `tests/snapshots_ui.rs`, calcar `snapshot_modal_colision`: `app.modal = Some(Modal::TrustLuaInit { path: "….norte/init.lua".into(), hash_abbrev: "ab12cd34".into() })` → `insta::assert_snapshot!`. Correr con `cargo nextest run -p norte-tui --test snapshots_ui`, revisar el snapshot generado, `cargo insta accept` (o el flujo del repo: revisar trampas conocidas de fixtures/insta en la memoria del proyecto).

- [ ] **Step 7: verde total del crate** — `cargo nextest run -p norte-tui` → PASS todo.
- [ ] **Step 8: Commit** — `feat(tui): wiring Lua — carga por capas con trust, lua: en keymap, Esc cancela (M4 Lua T8)`

---

### Task 9: E2E + presupuesto + reviewers + cierre

**Files:**
- Create: `crates/norte-tui/tests/lua_e2e.rs`
- Modify: spec (desviaciones), `docs/notes/` si aplica

- [ ] **Step 1: E2E** (`tests/lua_e2e.rs`): criterio de salida de la spec sin terminal — con `backend_mem()`:
```rust
//! E2E del criterio de salida: comando Lua real que copia la selección al
//! otro pane y renombra, vía engine (journal); cancelación limpia.
// 1. init.lua (string) con:
//    norte.command('copiar-sel', function()
//      for _, p in ipairs(norte.pane.selection()) do
//        assert(norte.fs.copy(p, norte.pane.other_cwd() .. "/copia-" .. basename(p)))
//      end
//    end)
//    (basename en Lua puro sobre byte strings: string.match no — usar
//    find/sub por byte '/'; escribirlo en el propio init de test.)
// 2. eval_layer(User) + invoke con PaneCtx{selection=[mem:///a, mem:///b]}.
// 3. assert: mem:///dst/copia-a y copia-b existen (stat).
// 4. nombre hostil: selection con mem:///%FF → copia-%FF existe.
```

- [ ] **Step 2: presupuesto**: si `benches/presupuestos.rs` tiene patrón por feature, añadir bench del hook de statusbar (llamada cacheada < 1µs, no cacheada < 1ms con script trivial); si el harness no encaja, anotar en la spec que el presupuesto queda por instrucciones (50k) y saltar el bench.

- [ ] **Step 3: reviewers** (obligatorios aquí):
  - `security-reviewer`: trust store (TOCTOU, canónico, 0600), modelo no-sandbox documentado, `lua:` desde keymap de PROYECTO (keymap.toml de `./.norte` puede bindear `lua:` — ¿escala a ejecución sin trust? NO debe: el comando lo define init.lua que SÍ pasa trust — verificar y anclar con test si el reviewer confirma el vector).
  - `encoding-auditor`: byte strings end-to-end, statusbar/mensajes por mask, modal con path saneado.
  - `rust-reviewer`: reglas duras (unwraps, regla 2 en I/O del store, !Send discipline).
  Aplicar hallazgos (test rojo primero si son bugs).

- [ ] **Step 4: `just ci`** → EXIT=0. Actualizar spec con desviaciones reales (p.ej. mkdir fuera si Backend no lo tenía). Cerrar con nota en la spec: estado → implementado.
- [ ] **Step 5: Commit** — `feat(tui): E2E Lua + reviewers aplicados — M4 Lua scripting completo (M4 Lua T9)` + push.

---

## Self-review del plan (hecho)

- Cobertura spec: carga/trust (T6+T8), registro (T2), API fs/pane/ui bytes (T3+T4), driver/cancelación/timeout/FIFO (T5+T8), statusbar+presupuesto+mask (T7), errores Fluent (T8), tests 1-7 de la spec (T2/T5/T6/T7/T8/T9), ADR+deny (T1). `norte.fs.mkdir`: condicionado a que Backend lo exponga — desviación documentada si no (T4).
- Tipos consistentes: `LuaHost::{new, eval_layer, commands, invoke, invoke_with_timeout, statusbar, statusbar_error}`, `Layer`, `PaneCtx`, `RunOutcome`, `TrustStore::{open, check, record}`, `TrustDecision` — nombres únicos en todo el plan.
- Sin placeholders: los `/* … */` de T4/T5 van acompañados de contrato exacto (firmas, semántica, estructura del select pineado) — decisión consciente: el detalle mecánico de mlua se resuelve contra la doc del crate en el momento (versión exacta se fija en T1).
