# M4-P3 — Gestor de extensiones (protocolo + core + TUI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development o superpowers:executing-plans. Pasos con checkbox (`- [ ]`).

**Goal:** El usuario ve y gobierna sus plugins desde una vista ordenada tipo VSCode (categorías + capability badges siempre visibles + aviso `⚠ sin aprobar`), y aprueba/activa cada uno — con la lógica en el CORE (regla 7) expuesta por `plugin.*` y el TUI como mero cliente.

**Architecture:** Regla 7: el descubrimiento del catálogo, el estado (aprobado/activado) y su persistencia viven en `norte-core` (que pasa a depender de `norte-plugin-host`, subsistema del core), expuestos por métodos `plugin.*` (proto 0.13.0). El TUI llama a `Backend::plugins_list/set_approval/set_enabled` y pinta un overlay agrupado por categoría; jamás lee el FS ni decide. El estado persiste en `config_dir/plugins-state.toml` (`[<id>] approved=bool enabled=bool`), fusionado sobre el catálogo descubierto en `config_dir/plugins/<id>/`. NO ejecuta plugins (eso es wiring posterior, gated por la integración policy M3); esto es descubrir + mostrar + gobernar estado.

**Tech Stack:** Rust, JSON-RPC (proto), `toml_edit` (persistencia preservando formato), ratatui (overlay), nextest. Reviewers: **protocol-guardian OBLIGATORIO** (Task 1), **security-reviewer** (Task 3: la aprobación es una decisión de seguridad — solo User; persistencia), rust-reviewer por task, encoding-auditor no aplica (ids reverse-DNS ASCII).

**Nota de threat model:** aprobar un plugin = consentir sus capabilities (ADR 0022 D4). `plugin.set_approval`/`set_enabled` son actos HUMANOS → solo conexiones `User` (como `policy.grant_scope`); un agente jamás aprueba plugins. `plugin.list` es de solo lectura, abierto.

---

## File Structure

- `crates/norte-core/src/plugins.rs` — `PluginRegistry`: descubre el catálogo, fusiona/persiste estado, list/set. Depende de `norte-plugin-host`.
- `crates/norte-core/src/lib.rs`, `Cargo.toml` — wiring (modificar).
- `crates/norte-proto/src/methods.rs` — `plugin.*` + tipos (modificar) + goldens.
- `crates/norte-core/src/backend.rs` — `Backend::plugins_*` (embedded + remote) (modificar).
- `crates/norte-core/src/daemon/server.rs` — handlers `plugin.*` (modificar).
- `crates/norte-tui/src/app.rs` — estado `ExtensionManager` (modificar).
- `crates/norte-tui/src/ui.rs` — render del overlay (modificar).
- `crates/norte-tui/src/main.rs` — comando `app.extensions` + key handler (modificar).
- `crates/norte-tui/src/keymap.rs` — registrar `app.extensions` en `COMMANDS` (modificar).
- `crates/norte-tui/Cargo.toml` — dep `norte-plugin-host` NO (el TUI usa los tipos de proto, no el modelo del host).
- `crates/norte-i18n/i18n/{es,en}.ftl` — strings (modificar).

---

## Task 1: proto 0.13.0 — `plugin.list` / `plugin.set_approval` / `plugin.set_enabled`

**Files:** `crates/norte-proto/src/methods.rs`, tests `types.rs`/`golden_types.rs`, goldens `methods.json`, test N-1 del daemon.

- [ ] **Step 1: consts y tipos** en `methods.rs`. Junto a las familias existentes:

```rust
/// `plugin.list` — enumera los plugins descubiertos + los que fallaron al
/// cargar (M4-P3). Solo lectura, abierto a cualquier conexión.
pub const PLUGIN_LIST: &str = "plugin.list";
/// `plugin.set_approval` — un HUMANO aprueba/desaprueba las capabilities de un
/// plugin (acto de seguridad, solo conexiones User).
pub const PLUGIN_SET_APPROVAL: &str = "plugin.set_approval";
/// `plugin.set_enabled` — un HUMANO activa/desactiva un plugin (solo User).
pub const PLUGIN_SET_ENABLED: &str = "plugin.set_enabled";
```

Tipos (todos `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]`):

```rust
/// Un plugin descubierto, tal como lo pinta el gestor (M4-P3).
pub struct PluginInfo {
    /// Id reverse-DNS (`org.norte.demo`).
    pub id: String,
    /// Nombre legible.
    pub name: String,
    /// Publicador.
    pub publisher: String,
    /// Versión declarada.
    pub version: String,
    /// Categoría primaria (`previewer|provider|command|columns|hook`).
    pub category: String,
    /// Etiquetas de las capabilities concedidas (`["fs-read","net"]`).
    pub capabilities: Vec<String>,
    /// El usuario aprobó sus capabilities (si no, el host no lo cargará).
    pub approved: bool,
    /// El usuario lo tiene activado.
    pub enabled: bool,
}

/// Un manifiesto que no cargó (se muestra como error, no se oculta).
pub struct PluginLoadError {
    /// Directorio culpable (display).
    pub dir: String,
    /// Causa legible.
    pub reason: String,
}

/// Result de [`PLUGIN_LIST`]: plugins válidos (ordenados por categoría e id) +
/// manifiestos inválidos.
pub struct PluginListResult {
    /// Plugins descubiertos y válidos.
    pub plugins: Vec<PluginInfo>,
    /// Manifiestos que fallaron al cargar.
    pub errors: Vec<PluginLoadError>,
}

/// Params de [`PLUGIN_SET_APPROVAL`].
pub struct PluginSetApprovalParams {
    /// Id del plugin.
    pub id: String,
    /// Nuevo estado de aprobación.
    pub approved: bool,
}
/// Result de [`PLUGIN_SET_APPROVAL`].
pub struct PluginSetApprovalResult {}

/// Params de [`PLUGIN_SET_ENABLED`].
pub struct PluginSetEnabledParams {
    /// Id del plugin.
    pub id: String,
    /// Nuevo estado de activación.
    pub enabled: bool,
}
/// Result de [`PLUGIN_SET_ENABLED`].
pub struct PluginSetEnabledResult {}
```

`plugin.list` no tiene params (como `task.list` que usa un struct vacío `TaskListParams {}` — crea `PluginListParams {}` por simetría del dispatcher, o reutiliza el patrón null-params). Mira cómo `TASK_LIST`/`TaskListParams` se manejan en el daemon y replica.

- [ ] **Step 2: bump** `PROTOCOL_VERSION` a `"0.13.0"` con rustdoc del changelog (aditivo sobre 0.12.x). Actualiza la ventana N/N-1 en `types.rs` `version_ventana_actual` (0.13/0.12, reject 0.11), `golden_types.rs` assert `"0.13.0"`, y el test N-1 del daemon (`crates/norte-core/tests/daemon.rs`: los clientes que envían `0.11.x`/`0.11.0` raw → `0.12.x`/`0.12.0`).

- [ ] **Step 3: tests** — roundtrip serde de los tipos nuevos en `types.rs`; goldens nuevos en `methods.json` (`plugin_info`, `plugin_list_result`, `plugin_set_approval_params`, etc.) + checker `check_methods_plugin` en `golden_types.rs` (sube el `assert_eq!(fixtures.len(), N)`); pins en `method_names_frozen` de los 3 consts nuevos.

- [ ] **Step 4: verde** — `cargo nextest run -p norte-proto` y `-p norte-core -E 'binary(daemon)'`. **protocol-guardian OBLIGATORIO** (aditividad, ventana, goldens, naming `plugin.*`). Commit: `feat(proto): plugin.list/set_approval/set_enabled, bump 0.13.0 + goldens (M4-P3 T1)`.

---

## Task 2: core — `PluginRegistry` (descubrir + estado + persistir)

**Files:**
- Create: `crates/norte-core/src/plugins.rs`
- Modify: `crates/norte-core/src/lib.rs`, `crates/norte-core/Cargo.toml`
- Test: en `plugins.rs` (`#[cfg(test)]`)

- [ ] **Step 1: dep** — añade `norte-plugin-host.workspace = true` a `crates/norte-core/Cargo.toml` (`[dependencies]`). `toml_edit` para la persistencia: comprueba si ya está en el workspace (el TUI lo usa); si no, añádelo a `[workspace.dependencies]` y úsalo.

- [ ] **Step 2: test rojo** — en `plugins.rs`, un test que: crea un tempdir con `plugins/org.norte.demo/plugin.toml` (un manifiesto válido — mira el formato en `crates/norte-plugin-host/src/lib.rs` doctest), llama a `PluginRegistry::discover(dir)`, `list()` devuelve 1 plugin con `approved=false enabled=false`; luego `set_approval("org.norte.demo", true)` + `set_enabled(..., true)`, `list()` refleja el cambio, y una NUEVA `PluginRegistry::discover(mismo dir)` LO RECUERDA (persistió en `plugins-state.toml`).

- [ ] **Step 3: implementación** — `crates/norte-core/src/plugins.rs`:

```rust
//! Registro de plugins (M4-P3): descubre el catálogo local (via
//! `norte-plugin-host`), fusiona el estado aprobado/activado persistido y lo
//! expone al protocolo `plugin.*`. La lógica vive AQUÍ (regla 7); el TUI solo
//! pinta lo que este registro entrega.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norte_plugin_host::{Catalog, Category};
use norte_proto::methods::{PluginInfo, PluginListResult, PluginLoadError};

/// Estado por-plugin persistido (aprobado/activado), fusionado sobre el
/// catálogo descubierto.
#[derive(Debug, Clone, Copy, Default)]
struct PluginState {
    approved: bool,
    enabled: bool,
}

/// El registro: catálogo descubierto + estado persistido + el dir base.
pub struct PluginRegistry {
    /// Dir de config (`plugins/` cuelga de aquí; el estado es `plugins-state.toml`).
    config_dir: PathBuf,
    /// Estado por id (persistido en `plugins-state.toml`).
    state: BTreeMap<String, PluginState>,
    /// Último catálogo descubierto (id → info base, sin fusionar estado).
    catalog: Catalog,
}

impl PluginRegistry {
    /// Descubre `config_dir/plugins/` y carga `config_dir/plugins-state.toml`.
    /// Descubrimiento y lectura de estado son I/O SÍNCRONO: llámalo una vez al
    /// arrancar o vía `spawn_blocking`.
    ///
    /// # Errors
    /// Nunca falla por catálogo/estado ausentes (vacío = sin plugins); solo por
    /// un `plugins-state.toml` presente pero corrupto.
    pub fn discover(config_dir: &Path) -> std::io::Result<Self> {
        let catalog = Catalog::load_dir(&config_dir.join("plugins"));
        let state = load_state(config_dir)?;
        Ok(Self {
            config_dir: config_dir.to_path_buf(),
            state,
            catalog,
        })
    }

    /// Lista los plugins (fusionando estado) + los errores de carga.
    #[must_use]
    pub fn list(&self) -> PluginListResult {
        let plugins = self
            .catalog
            .plugins
            .iter()
            .map(|p| {
                let st = self.state.get(&p.manifest.id).copied().unwrap_or_default();
                PluginInfo {
                    id: p.manifest.id.clone(),
                    name: p.manifest.name.clone(),
                    publisher: p.manifest.publisher.clone(),
                    version: p.manifest.version.clone(),
                    category: category_str(p.manifest.category).to_string(),
                    capabilities: p
                        .manifest
                        .capabilities
                        .badges()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    approved: st.approved,
                    enabled: st.enabled,
                }
            })
            .collect();
        let errors = self
            .catalog
            .errors
            .iter()
            .map(|e| PluginLoadError {
                dir: e.dir.display().to_string(),
                reason: e.error.to_string(),
            })
            .collect();
        PluginListResult { plugins, errors }
    }

    /// Aprueba/desaprueba un plugin (persiste). `false` si el id no existe en
    /// el catálogo (no se inventa estado para plugins fantasma).
    ///
    /// # Errors
    /// I/O al persistir.
    pub fn set_approval(&mut self, id: &str, approved: bool) -> std::io::Result<bool> {
        self.set(id, |st| st.approved = approved)
    }

    /// Activa/desactiva un plugin (persiste). `false` si el id no existe.
    ///
    /// # Errors
    /// I/O al persistir.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> std::io::Result<bool> {
        self.set(id, |st| st.enabled = enabled)
    }

    fn set(&mut self, id: &str, f: impl FnOnce(&mut PluginState)) -> std::io::Result<bool> {
        // Solo para plugins DESCUBIERTOS: no se persiste estado de un id que no
        // existe (evita basura y estados huérfanos).
        if !self.catalog.plugins.iter().any(|p| p.manifest.id == id) {
            return Ok(false);
        }
        f(self.state.entry(id.to_string()).or_default());
        persist_state(&self.config_dir, &self.state)?;
        Ok(true)
    }
}

fn category_str(c: Category) -> &'static str {
    c.as_str()
}

/// Lee `plugins-state.toml` (`[<id>] approved=bool enabled=bool`). Ausente =
/// vacío.
fn load_state(config_dir: &Path) -> std::io::Result<BTreeMap<String, PluginState>> {
    let path = config_dir.join("plugins-state.toml");
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let doc: toml_edit::DocumentMut = src
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut out = BTreeMap::new();
    for (id, item) in doc.as_table() {
        let approved = item.get("approved").and_then(|v| v.as_bool()).unwrap_or(false);
        let enabled = item.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        out.insert(id.to_string(), PluginState { approved, enabled });
    }
    Ok(out)
}

/// Persiste el estado preservando lo que ya hubiera (toml_edit).
fn persist_state(config_dir: &Path, state: &BTreeMap<String, PluginState>) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("plugins-state.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    for (id, st) in state {
        let tbl = doc.as_table_mut().entry(id).or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(false);
            toml_edit::Item::Table(t)
        });
        tbl["approved"] = toml_edit::value(st.approved);
        tbl["enabled"] = toml_edit::value(st.enabled);
    }
    std::fs::write(&path, doc.to_string())
}
```

**OJO id como clave TOML:** un id reverse-DNS lleva puntos (`org.norte.demo`); `toml_edit` interpretaría `[org.norte.demo]` como tablas anidadas. Verifica que `entry(id)` con un id con puntos crea una clave LITERAL (quizá necesites `doc[id]` con el id entre comillas, o iterar la tabla raíz de otra forma). Si `toml_edit` anida por los puntos, cambia el formato a `[plugins]` con `"org.norte.demo" = { approved=true, enabled=false }` (una tabla `plugins` con claves-string entrecomilladas) — ajusta `load_state`/`persist_state` en consecuencia. RESUÉLVELO con un test que haga round-trip de un id con puntos.

- [ ] **Step 4: expón** en `lib.rs`: `pub mod plugins;` + `pub use plugins::PluginRegistry;`.

- [ ] **Step 5: verde** — `cargo nextest run -p norte-core -E 'test(plugins)'`. Commit: `feat(core): PluginRegistry — descubre catálogo + estado persistido (M4-P3 T2)`. rust-reviewer.

---

## Task 3: daemon + Backend — cablear `plugin.*`

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (handlers + `Shared` gana el registry)
- Modify: `crates/norte-core/src/backend.rs` (`Backend::plugins_*`)
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: el registry en el daemon.** El daemon necesita un `PluginRegistry` compartido (Mutex — set persiste, es mutable). En `Shared` añade `plugins: std::sync::Mutex<crate::plugins::PluginRegistry>`. Lo construye `Daemon::bind*` con `PluginRegistry::discover(config_dir())` (mira de dónde saca el daemon el config_dir; el binario `norte daemon run` ya usa `norte_core::connect::config_dir()` — el registry se construye ahí y se pasa, o el `bind` lo descubre). Decisión simple: `bind_with_policy`/`bind` descubren el registry de `connect::config_dir()` internamente (spawn_blocking, regla 2) — si prefieres inyectarlo para testear, añade un `bind_with_plugins` como con scopes. Para los TESTS, hace falta poder apuntar a un tempdir: añade el registry como parámetro inyectable o un `DaemonConfig.plugins_dir: Option<PathBuf>`.

- [ ] **Step 2: handlers** en `dispatch` (junto a `policy.*`):

```rust
        methods::PLUGIN_LIST => {
            let _p: methods::PluginListParams = parse_params(
                req.params.filter(|v| !v.is_null()).or_else(|| Some(serde_json::json!({}))),
            )?;
            let list = shared.plugins.lock().expect("plugins lock sano").list();
            to_value(&list)
        }
        methods::PLUGIN_SET_APPROVAL => {
            let p: methods::PluginSetApprovalParams = parse_params(req.params)?;
            handle_plugin_set_approval(&conn.actor, &p, shared)
        }
        methods::PLUGIN_SET_ENABLED => {
            let p: methods::PluginSetEnabledParams = parse_params(req.params)?;
            handle_plugin_set_enabled(&conn.actor, &p, shared)
        }
```

Handlers (patrón de `handle_grant_scope` — solo `Actor::User`):

```rust
/// `plugin.set_approval` (M4-P3): aprobar es un acto de seguridad HUMANO
/// (consentir las capabilities del plugin) — solo conexiones User.
fn handle_plugin_set_approval(
    actor: &Actor,
    p: &methods::PluginSetApprovalParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may approve a plugin",
        ));
    }
    let ok = shared
        .plugins
        .lock()
        .expect("plugins lock sano")
        .set_approval(&p.id, p.approved)
        .map_err(|e| RpcError::protocol(codes::INTERNAL_ERROR, format!("persist: {e}")))?;
    if !ok {
        return Err(RpcError::protocol(codes::INVALID_PARAMS, "unknown plugin id"));
    }
    to_value(&methods::PluginSetApprovalResult {})
}
```

(y `handle_plugin_set_enabled` idéntico con `set_enabled` y `PluginSetEnabledResult`.)

- [ ] **Step 3: Backend passthrough** en `backend.rs`:

```rust
    /// Lista los plugins descubiertos (M4-P3).
    ///
    /// # Errors
    /// Taxonomía; en embebido usa un `PluginRegistry` de `config_dir()`.
    pub async fn plugins_list(&self) -> Result<methods::PluginListResult, Error> { ... }
    pub async fn plugins_set_approval(&self, id: &str, approved: bool) -> Result<(), Error> { ... }
    pub async fn plugins_set_enabled(&self, id: &str, enabled: bool) -> Result<(), Error> { ... }
```

Embedded: construye/retiene un `PluginRegistry` (el `Backend::Embedded` hoy es solo `Arc<Engine>`; añade el registry al lado — un `Backend::Embedded { engine, plugins: Arc<Mutex<PluginRegistry>> }`, o un nuevo campo). Remote: `call_timed` a los métodos. Importa `methods::PluginListResult` etc. Mira cómo `capabilities`/`trust_host_key` hacen el passthrough embedded/remote y replica.

- [ ] **Step 4: tests daemon** — un cliente `plugin.list` sobre un daemon con `plugins_dir` = tempdir sembrado devuelve el plugin; un humano `plugin.set_approval` lo aprueba (y `plugin.list` lo refleja); un AGENTE (`agent_session`) que intenta `set_approval` → `INVALID_REQUEST`; id desconocido → `INVALID_PARAMS`. Commit: `feat(core): daemon+Backend cablean plugin.* (M4-P3 T3)`. **security-reviewer** (aprobar solo-User, persistencia, id validado) + rust-reviewer.

---

## Task 4: TUI — el gestor de extensiones (overlay)

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (estado `ExtensionManager`)
- Modify: `crates/norte-tui/src/ui.rs` (render)
- Modify: `crates/norte-tui/src/main.rs` (comando + key handler)
- Modify: `crates/norte-tui/src/keymap.rs` (`COMMANDS`)
- Modify: `crates/norte-i18n/i18n/{es,en}.ftl`
- Test: `crates/norte-tui/tests/` (estado del overlay)

- [ ] **Step 1: estado** en `app.rs` — un `Option<ExtensionManager>` en `App` (como `theme_picker`):

```rust
/// Vista del gestor de extensiones (M4-P3, ADR 0022 D5): el catálogo agrupado
/// por categoría, con capability badges y el estado aprobado/activado. Los
/// datos vienen del protocolo (`plugin.list`); el TUI solo navega y pinta.
#[derive(Debug, Clone)]
pub struct ExtensionManager {
    /// Plugins tal como los entregó `plugin.list` (ya ordenados por el core).
    pub plugins: Vec<norte_proto::methods::PluginInfo>,
    /// Errores de carga (se muestran al final, no se ocultan).
    pub errors: Vec<norte_proto::methods::PluginLoadError>,
    /// Índice resaltado.
    pub cursor: usize,
}
```

Métodos: `up`/`down` (clamp), `selected() -> Option<&PluginInfo>`. Y en `App`: `extensions: Option<ExtensionManager>` (init `None` en `App::new`). Un enum `ExtAction { Up, Down, ToggleApprove, ToggleEnable, Close }` (el frontend traduce las teclas; el efecto —llamar al Backend— vive en main.rs).

- [ ] **Step 2: render** en `ui.rs` — `draw_extensions(frame, mgr, theme)`: un overlay centrado (patrón `draw_theme_picker`/`draw_help`) que lista, AGRUPADO por `category` (los plugins ya vienen ordenados por categoría desde el core; inserta una cabecera de grupo al cambiar de categoría), cada plugin como `<nombre> <v> [badges] <estado>` donde estado = `✓` si enabled y `⚠` si NOT approved (rol `Warning` para el `⚠`). Los `errors` al final en rol de error. Hint de teclas abajo (`[a]probar [e]activar [esc]cerrar`). Los nombres se pasan por `display_name` (aunque los ids son ASCII, el `name` es libre — enmascara controles/bidi como en los panes; encoding-auditor no se dispara pero es la disciplina de la casa).

- [ ] **Step 3: comando + wiring** en `keymap.rs` añade `"app.extensions"` a `COMMANDS` (y su `help-cmd-app-extensions` en los ftl). En `main.rs`:
  - `dispatch`: brazo `"app.extensions" => { /* carga plugin.list del backend y abre el overlay */ }`. Como `dispatch` no es async-friendly para I/O larga aquí, sigue el patrón: llama `backend.plugins_list().await`, construye el `ExtensionManager`, `app.extensions = Some(...)`. Si falla, `app.message`.
  - En el loop de teclas (antes de `app.modal`), un brazo `if app.extensions.is_some() { on_extensions_key(app, backend, key).await; }` como el de `theme_picker`.
  - `on_extensions_key`: Up/Down/`k`/`j` navegan; `a` → `backend.plugins_set_approval(id, !approved)` y re-carga o togglea local; `e` → `set_enabled(id, !enabled)`; Esc/`q` cierran (`app.extensions = None`). Tras un set OK, refresca el `PluginInfo` local (togglea el bool en el `ExtensionManager` para feedback inmediato, o re-llama `plugin.list`).

- [ ] **Step 4: strings** (es/en, paridad): `ext-title = Extensiones` / `Extensions`; `ext-hint = [a]probar  [e]activar  [↑↓] mover  [esc] cerrar` / `[a]pprove [e]nable [↑↓] move [esc] close`; `ext-badge-unapproved = ⚠ sin aprobar` / `⚠ not approved`; `ext-empty = no hay extensiones instaladas` / `no extensions installed`; `help-cmd-app-extensions = gestor de extensiones` / `extension manager`.

- [ ] **Step 5: test** — en `crates/norte-tui/tests/` (patrón de `theme_picker.rs`/`modal.rs`): construye un `ExtensionManager` con 2-3 `PluginInfo` mock, verifica `up`/`down`/`selected` y que `ExtAction` mapea las teclas esperadas; un render smoke con `TestBackend` que afirme que aparecen el nombre, un badge y el `⚠` de uno sin aprobar. Commit: `feat(tui): gestor de extensiones — overlay del catálogo (M4-P3 T4)`. rust-reviewer.

---

## Task 5: E2E + cierre

**Files:**
- Test: `crates/norte-core/tests/` o `crates/norte-mcp`/donde encaje un E2E daemon
- Modify: `docs/adr/0022-*.md` (addendum P3), memoria

- [ ] **Step 1: E2E** — daemon con `plugins_dir` sembrado (2 plugins, uno con manifiesto inválido): un cliente hace `plugin.list` (ve 1 válido + 1 error), aprueba y activa el válido, y una `PluginRegistry::discover` fresca del mismo dir lo recuerda (persistió). Verifica el round-trip completo por el wire.

- [ ] **Step 2: verde total** — `just ci` COMPLETO.

- [ ] **Step 3: cierre** — addendum P3 en ADR 0022 (qué quedó: catálogo por protocolo, estado persistido, overlay TUI; deuda: el gestor MUESTRA pero aún no CARGA/EJECUTA plugins —eso es el wiring runtime↔core bajo policy M3—, no hay instalación/desinstalación desde la UI, no hay registro remoto). Memoria: **M4-P3 COMPLETA**. Commit: `docs,test: E2E gestor de extensiones + cierre M4-P3 (T5)`.

---

## Riesgos / verificar

1. **Id reverse-DNS como clave TOML** (Task 2 Step 3): los puntos → anidamiento en `toml_edit`. Test de round-trip de un id con puntos ANTES de dar por buena la persistencia; si anida, usa `[plugins]` con claves entrecomilladas.
2. **`norte-core` → `norte-plugin-host`**: dep nueva de un crate del core a otro subsistema del core (ambos AGPL) — OK por licencia; justifícalo en la PR (regla 8: es el modelo de plugins que el protocolo expone).
3. **`Backend::Embedded` cambia de forma** (Task 3): hoy es `Embedded(Arc<Engine>)`; añadirle el registry toca todos los `match self` del backend. Alternativa menos invasiva: un `OnceLock`/`Mutex<Option<PluginRegistry>>` lazy en el embedded, o pasar el registry como campo del enum. Elige lo que menos rompa y compila.
4. **El daemon descubre en `config_dir()` real**: los tests DEBEN poder apuntar a un tempdir (`DaemonConfig.plugins_dir` o inyección) — sin esto el test lee el `~/.config` de quien corre CI. NO uses el dir real en tests.
5. **Regla 7**: el TUI JAMÁS llama a `PluginRegistry`/`Catalog` directo — todo por `Backend::plugins_*`. Si te tienta importar `norte-plugin-host` en el TUI, para: eso es la lógica que va en core.
6. **Aprobar es solo-User** (regla de seguridad): un agente MCP jamás aprueba un plugin. Test que lo pinnee.
