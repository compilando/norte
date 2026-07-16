# M4-P4 — Ejecutar plugins `command` bajo consentimiento Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development o superpowers:executing-plans. Pasos con checkbox (`- [ ]`).

**Goal:** Un plugin `command` APROBADO y ACTIVADO se EJECUTA de verdad — el core lo carga con el runtime WASM (M4-P2), corre su comando sandboxeado y devuelve el resultado — expuesto por `plugin.run_command` y `norte plugin run`, cerrando la cadena descubrir→aprobar→activar→ejecutar.

**Architecture:** El core gana un `PluginHost` que junta el `PluginRegistry` (M4-P3, estado aprobado/activado) con el `PluginRuntime` (M4-P2, sandbox). `run_command(id, command, arg)` exige que el plugin esté DESCUBIERTO + APROBADO + ACTIVADO (fail-closed: si no, error tipado), resuelve su `.wasm` por convención (`<dir>/plugin.wasm`), lo instancia con las capabilities del manifiesto y llama al export `command.run`. El comando es puro (devuelve string; los efectos de FS de un plugin van por las puertas host gateadas por capabilities de M4-P2, ya enforced). Expuesto por `plugin.run_command` (proto 0.14.0) + handler daemon + Backend + `norte plugin run`. NO toca la integración fina con el policy engine M3 (aprobar el plugin ES el consentimiento; ask/allow/deny por-op de las puertas FS del plugin es deuda).

**Tech Stack:** Rust, `norte-plugin-host` (PluginRuntime/PluginRegistry), JSON-RPC (proto), wasmtime (via el runtime), `wasm32-wasip2` para el guest del E2E. Reviewers: **protocol-guardian OBLIGATORIO** (Task 1), **security-reviewer OBLIGATORIO** (Task 2: fail-closed approved+enabled, carga del .wasm, no ejecutar lo no consentido), rust-reviewer por task.

**Nota de threat model:** ejecutar SOLO plugins aprobados+activados (consentidos). El aislamiento lo da el sandbox de M4-P2 (WASI vacío + capabilities + límites CPU/memoria). La integración con el policy engine M3 (gate por-operación de las puertas FS del plugin, journal de lo que hace un plugin) se superpone después — hoy la consentimiento es a nivel de plugin (aprobar), no por-op.

---

## File Structure

- `crates/norte-core/src/plugins.rs` — `PluginHost` (junta registry + runtime; `run_command`) (modificar).
- `crates/norte-plugin-host/src/lib.rs`/`catalog.rs` — exponer el `dir` del `PluginEntry` y una forma de resolver el `.wasm` (ya hay `PluginEntry.dir`; quizá un helper) (revisar/modificar).
- `crates/norte-proto/src/methods.rs` — `plugin.run_command` + tipos + goldens (modificar).
- `crates/norte-core/src/daemon/server.rs` — handler (modificar).
- `crates/norte-core/src/backend.rs` — `Backend::plugin_run_command` (modificar).
- `crates/norte-cli/src/main.rs` — `norte plugin run <id> <command> [arg]` (modificar).
- `crates/norte-i18n/i18n/{es,en}.ftl` — strings CLI (modificar).
- E2E: `crates/norte-core/tests/` o donde encaje con acceso al guest `.wasm` de M4-P2.

---

## Task 1: proto 0.14.0 — `plugin.run_command`

**Files:** `crates/norte-proto/src/methods.rs`, tests `types.rs`/`golden_types.rs`, goldens `methods.json`, test N-1 del daemon.

- [ ] **Step 1: const y tipos.** En `methods.rs`:

```rust
/// `plugin.run_command` — ejecuta un comando de un plugin `command` APROBADO
/// y ACTIVADO (M4-P4). El plugin corre sandboxeado; el resultado es el string
/// que devuelve, o un error del plugin/host.
pub const PLUGIN_RUN_COMMAND: &str = "plugin.run_command";

/// Params de [`PLUGIN_RUN_COMMAND`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunCommandParams {
    /// Id del plugin (reverse-DNS).
    pub id: String,
    /// Id del comando que aporta el plugin.
    pub command: String,
    /// Argumento de texto libre para el comando.
    #[serde(default)]
    pub arg: String,
}

/// Result de [`PLUGIN_RUN_COMMAND`]: el mensaje que devolvió el comando.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunCommandResult {
    /// Salida del comando (para la barra de estado / stdout de la CLI).
    pub output: String,
}
```

- [ ] **Step 2: bump** `PROTOCOL_VERSION` a `"0.14.0"` con changelog (aditivo sobre 0.13.x). Ventana N/N-1 → 0.14/0.13, reject 0.12: `types.rs` `version_ventana_actual`, `golden_types.rs` assert, `daemon.rs` tests N-1 (los `0.12.x`/`0.12.0`/"de 0.13.0" → `0.13.x`/`0.13.0`/"de 0.14.0").

- [ ] **Step 3: tests** — roundtrip en `types.rs` (incl. `arg` ausente = `""` por `#[serde(default)]`); goldens `plugin_run_command_params` (con y sin arg) + `plugin_run_command_result` en `methods.json`; añadir al `check_methods_plugin` (sube el count); pin del const en `method_names_frozen`.

- [ ] **Step 4: verde** — `cargo nextest run -p norte-proto` y `-p norte-core -E 'binary(daemon)'`. **protocol-guardian OBLIGATORIO**. Commit: `feat(proto): plugin.run_command, bump 0.14.0 + goldens (M4-P4 T1)`.

---

## Task 2: core — `PluginHost` (cargar + ejecutar bajo consentimiento)

**Files:**
- Modify: `crates/norte-core/src/plugins.rs`
- Modify: `crates/norte-plugin-host/src/` (exponer resolución del `.wasm` si hace falta)
- Test: en `plugins.rs`

- [ ] **Step 1: exponer el `.wasm` del plugin.** El `PluginEntry` de `norte-plugin-host` ya tiene `pub dir: PathBuf` y `pub manifest`. Convención (ADR 0022 D6): el binario es `<dir>/plugin.wasm`. Añade a `norte-plugin-host` un helper en `PluginEntry` o en el catálogo: `pub fn wasm_path(&self) -> PathBuf { self.dir.join("plugin.wasm") }` (o exponer que `Catalog`/`PluginRegistry` pueda dar el dir por id). Si prefieres que el manifiesto NOMBRE el wasm, es cambio de manifest (más scope) — usa la CONVENCIÓN `plugin.wasm` y anótalo en el rustdoc.

- [ ] **Step 2: `PluginHost` en `plugins.rs`.** Junta el registry (estado + catálogo, ya tiene el `catalog`) con un `PluginRuntime`:

```rust
use norte_plugin_host::{PluginRuntime, RuntimeError};

/// Errores de ejecutar un plugin (M4-P4).
#[derive(Debug, thiserror::Error)]
pub enum PluginRunError {
    /// No hay ningún plugin con ese id descubierto.
    #[error("plugin desconocido: {0}")]
    Unknown(String),
    /// El plugin existe pero no está aprobado (capabilities sin consentir).
    #[error("plugin sin aprobar: {0}")]
    NotApproved(String),
    /// El plugin existe pero está desactivado.
    #[error("plugin desactivado: {0}")]
    Disabled(String),
    /// No se encontró el `.wasm` del plugin.
    #[error("el plugin no tiene binario en {0}")]
    NoBinary(std::path::PathBuf),
    /// Fallo del runtime (carga/instanciación/trap/error del guest).
    #[error("runtime: {0}")]
    Runtime(#[from] RuntimeError),
}
```

`PluginRegistry` (o un `PluginHost` que lo envuelva) gana:

```rust
    /// Ejecuta un comando de un plugin APROBADO y ACTIVADO (fail-closed).
    ///
    /// # Errors
    /// [`PluginRunError`] si el plugin no existe, no está aprobado, está
    /// desactivado, no tiene `.wasm`, o el runtime falla.
    pub fn run_command(
        &self,
        runtime: &PluginRuntime,
        id: &str,
        command: &str,
        arg: &str,
    ) -> Result<String, PluginRunError> {
        // 1) Debe estar DESCUBIERTO.
        let entry = self
            .catalog
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| PluginRunError::Unknown(id.to_string()))?;
        // 2) Debe estar APROBADO y ACTIVADO (consentido). Fail-closed: el
        //    estado NO-aprobado/NO-activado JAMÁS ejecuta.
        let st = self.state.get(id).copied().unwrap_or_default();
        if !st.approved {
            return Err(PluginRunError::NotApproved(id.to_string()));
        }
        if !st.enabled {
            return Err(PluginRunError::Disabled(id.to_string()));
        }
        // 3) Resuelve el .wasm.
        let wasm = entry.dir.join("plugin.wasm");
        if !wasm.is_file() {
            return Err(PluginRunError::NoBinary(wasm));
        }
        // 4) Instancia con las capabilities DEL MANIFIESTO (el sandbox de
        //    M4-P2 las hace cumplir) y ejecuta.
        let mut inst = runtime.instantiate(&wasm, entry.manifest.capabilities.clone())?;
        Ok(inst.run_command(command, arg)?)
    }
```

`self.state`/`self.catalog` son privados de `PluginRegistry` — este método vive DENTRO de `plugins.rs` (mismo módulo, acceso a los campos). El `PluginRuntime` se pasa por referencia (el caller lo retiene; es caro de crear una vez). NOTA: `entry.manifest.capabilities` debe ser accesible y `Clone` — verifica en `norte-plugin-host` (`Capabilities` deriva Clone; `Manifest.capabilities` es `pub`).

- [ ] **Step 3: test rojo→verde** — en `plugins.rs` `#[cfg(test)]`: siembra un dir con `plugins/org.norte.cmd/plugin.toml` (category command) — SIN `.wasm` primero: `run_command` de un id no aprobado → `NotApproved`; apruébalo pero no actives → `Disabled`; aprueba+activa pero sin `.wasm` → `NoBinary`; id inexistente → `Unknown`. (El caso de éxito con `.wasm` real va en el E2E de Task 5, que tiene el guest de M4-P2.) Construye un `PluginRuntime::new()` para pasarlo.

- [ ] **Step 4: expón** `PluginRunError` y (si creaste un wrapper) `PluginHost` en `lib.rs`.

- [ ] **Step 5: verde** — `cargo nextest run -p norte-core -E 'test(plugins)'`. Commit: `feat(core): PluginHost ejecuta plugins command aprobados+activados (M4-P4 T2)`. **security-reviewer OBLIGATORIO** (fail-closed: NO ejecuta lo no consentido; carga del .wasm solo de plugins descubiertos; sandbox heredado de M4-P2) + rust-reviewer.

---

## Task 3: daemon + Backend + CLI — cablear `plugin.run_command`

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (handler; `Shared` gana un `PluginRuntime`)
- Modify: `crates/norte-core/src/backend.rs` (`Backend::plugin_run_command`)
- Modify: `crates/norte-cli/src/main.rs` (`plugin run`)
- Modify: `crates/norte-i18n/i18n/{es,en}.ftl`
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: `PluginRuntime` en el daemon.** `Shared` ya tiene `plugins: Mutex<PluginRegistry>`. Añade `plugin_runtime: PluginRuntime` (créalo en `bind*` con `PluginRuntime::new()`; si falla, degrada con warn a no-ejecutar o propaga — el runtime nuevo casi nunca falla; propaga como error de bind es aceptable). El runtime es `Send+Sync`? Verifica: el `Engine` de wasmtime es `Clone+Send+Sync`; `PluginRuntime` con el hilo ticker debería serlo — CONFIRMA (si no es Sync por algún campo, envuélvelo en lo mínimo). Como `run_command` toma `&PluginRegistry` + `&PluginRuntime`, el handler bloquea el registry lock brevemente para leer estado y luego ejecuta — pero `run_command` está DENTRO de PluginRegistry y toma `&self`, así que el handler hace `shared.plugins.lock().run_command(&shared.plugin_runtime, ...)`. OJO regla 2: `instantiate`+`run_command` de wasmtime son SÍNCRONOS y pueden tardar (compilar el componente, ejecutar) → el handler debe correr esto en `spawn_blocking`, NO en el hilo async con el lock tomado. Patrón: clona lo necesario (el runtime es Clone si el Engine lo es; o Arc), toma un snapshot del estado+dir+caps bajo el lock, suelta el lock, y en `spawn_blocking` instancia+ejecuta. Piensa la estructura: quizá `run_command` deba partirse en "resolver (id→wasm+caps, validando consentimiento) bajo lock" + "ejecutar (spawn_blocking)". Refactoriza así para respetar regla 2.

- [ ] **Step 2: handler** `plugin.run_command` en dispatch. Abierto o solo-User? Ejecutar un plugin consentido no es acto de aprobación — pero un agente ejecutando plugins es superficie. Decisión: **abierto** (list es abierto; ejecutar un plugin YA consentido por el humano es como invocar cualquier op — el sandbox lo contiene). Anótalo; si security lo objeta, gatéalo. Mapea `PluginRunError` a `RpcError`: `Unknown`/`NoBinary`→INVALID_PARAMS o un `Error` de taxonomía; `NotApproved`/`Disabled`→un error claro (¿PermissionDenied? o un mensaje). Devuelve `PluginRunCommandResult { output }`.

- [ ] **Step 3: Backend** `plugin_run_command(&self, id, command, arg) -> Result<String, Error>`: embedded construye/retiene un `PluginRuntime` (efímero por-llamada como el registry embebido, o un `OnceLock` — el runtime es caro; un `OnceLock<PluginRuntime>` en un lazy static del backend embebido, o efímero aceptando el coste) + registry de config_dir, en `spawn_blocking`; remote hace `call_timed`.

- [ ] **Step 4: CLI** `norte plugin run <id> <command> [arg]` — nuevo subcomando bajo un grupo `Plugin` (o extiende el `Policy`/crea `Plugin { cmd }` con `Run { id, command, arg }`). Conexión al daemon (o embedded), llama `backend.plugin_run_command`, imprime `output` a stdout, error a stderr. Strings i18n: `cli-plugin-run-failed = no se pudo ejecutar el plugin: { $error }` / `plugin run failed: { $error }`.

- [ ] **Step 5: test daemon** — un `plugin.run_command` de un id no aprobado → error claro; (el caso de éxito con wasm real = E2E Task 5). Commit: `feat(core,cli): daemon+Backend+CLI ejecutan plugin.run_command (M4-P4 T3)`. rust-reviewer.

---

## Task 4: TUI — invocar un command desde el gestor (opcional-mínimo)

**Files:**
- Modify: `crates/norte-tui/src/app.rs`, `ui.rs`, `main.rs`, i18n

- [ ] **Step 1: acción "ejecutar" en el overlay.** Si el plugin bajo el cursor es category `command` y está aprobado+activado, la tecla `r` (run) ejecuta su PRIMER comando (`manifest.contributions.command[0].id` — pero el TUI no tiene el manifiesto, solo `PluginInfo`; añade a `PluginInfo` los command ids? NO — eso agranda el wire). ALTERNATIVA sin ampliar proto: el TUI no invoca comandos concretos en P4 (no tiene la lista de comandos del plugin); deja la ejecución para la CLI y anota que la invocación desde el TUI (palette de comandos de plugin) es deuda. **Si eliges NO hacer TUI aquí, SALTA esta task** y anótalo en el cierre — el gestor sigue mostrando/gobernando; ejecutar es por CLI en P4. (Recomendado: SALTAR, para no ampliar el wire con la lista de comandos; la palette es una feature aparte.)

- [ ] Si la saltas: sin commit; nota en Task 5.

---

## Task 5: E2E con el `.wasm` real + cierre

**Files:**
- Test: donde tenga acceso al guest de M4-P2 (`crates/norte-plugin-host/examples-wasm/command-demo/`) — probablemente un test en `norte-core` que construya el guest con el helper de M4-P2 y lo coloque en un plugins-dir sembrado. Reusa el patrón `build_guest` de `crates/norte-plugin-host/tests/support/mod.rs` (cópialo o expón un helper).
- Modify: `docs/adr/0022-*.md` (addendum P4), memoria.

- [ ] **Step 1: E2E** — SKIP si falta el target `wasm32-wasip2` (como los tests de M4-P2). Construye `command-demo` a `.wasm`, crea un tempdir `cfg/plugins/org.norte.cmd/` con un `plugin.toml` (category command, id `org.norte.cmd`, sin capabilities especiales) + copia el `.wasm` a `cfg/plugins/org.norte.cmd/plugin.wasm`. Flujo: `PluginRegistry::discover(&cfg)`; `run_command` sin aprobar → `NotApproved`; `set_approval(true)`+`set_enabled(true)`; `run_command(&runtime, "org.norte.cmd", "echo", "hola")` → `"hola"`; `"shout","hola"` → `"HOLA"`; un comando desconocido → error del guest. (Si prefieres por el WIRE: daemon con plugins_dir=cfg, `plugin.set_approval`+`set_enabled`+`plugin.run_command` — más completo; hazlo por wire si el runtime es Sync en el daemon.)

- [ ] **Step 2: verde total** — `just ci` COMPLETO.

- [ ] **Step 3: cierre** — addendum P4 en ADR 0022 (qué quedó: ejecución de plugins command aprobados+activados por `plugin.run_command`/CLI, sandbox heredado de M4-P2, fail-closed; deuda: invocación desde el TUI —palette de comandos de plugin—, las interfaces previewer/provider/columns/hook wiring (previewer en el viewer, provider como scheme VFS…), integración fina con policy M3 (gate por-op de las puertas FS del plugin + journal), convención `plugin.wasm` vs manifiesto que nombre el binario). Memoria: **M4-P4 COMPLETA**. Commit: `docs,test: E2E ejecución de plugins + cierre M4-P4 (T5)`.

---

## Riesgos / verificar

1. **Regla 2**: `PluginRuntime::instantiate`+`run_command` son SÍNCRONOS y no-triviales (compilar el componente, ejecutar) → JAMÁS en el hilo async con un lock tomado. Parte `run_command` en resolver-bajo-lock + ejecutar-en-spawn_blocking. Es el riesgo principal del Task 3.
2. **`PluginRuntime` Send+Sync**: el hilo ticker de epoch + el Engine — confirma que es `Send+Sync` para vivir en `Shared`. Si no, `Arc` o reconstruir por-llamada.
3. **Fail-closed**: un plugin NO-aprobado o NO-activado JAMÁS se ejecuta. Test que lo pinnee; security-reviewer lo mira.
4. **Coste de instanciar por-llamada**: cada `run_command` compila+instancia el componente (no hay caché de instancias). Aceptable para P4 (invocación puntual); caché de instancias = deuda. Anótalo.
5. **`plugin.wasm` convención**: si un plugin no trae `plugin.wasm`, `NoBinary` claro. Documenta la convención.
6. **run_command abierto vs solo-User**: decidido abierto (plugin ya consentido, sandbox lo contiene); si security objeta, gatéalo. Anótalo para el reviewer.
