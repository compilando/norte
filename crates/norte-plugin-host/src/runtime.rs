//! Runtime wasmtime del host de plugins (M4-P2, ADR 0022 D1/D4): motor,
//! sandbox WASI VACÍO, instanciación del Component Model y el ENFORCEMENT
//! host-side de las capabilities (`fs-read`).
//!
//! El enforcement vive en el HOST, no en el guest (ADR 0022 D4): un plugin
//! hostil no puede evadir el gating porque la comprobación se hace en la
//! implementación host de `host-log::read-scoped`, antes de entregar bytes.

use std::collections::HashMap;
use std::path::Path;

use thiserror::Error;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::NortePlugin;
use crate::bindings::exports::norte::plugin::previewer::PreviewInput;
use crate::bindings::norte::plugin::host_log;
use crate::capability::Capabilities;

/// Tope de líneas de log que un plugin puede acumular (anti-DoS: el guest no
/// hace crecer la memoria del host sin límite vía `host-log::log`).
const MAX_LOGS: usize = 1024;

/// Fallo del runtime de plugins.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// El artefacto no es un componente WASM válido (o no se pudo leer).
    #[error("componente WASM inválido: {0}")]
    Component(String),
    /// El linker o la instanciación fallaron.
    #[error("instanciación fallida: {0}")]
    Instantiate(String),
    /// El guest atrapó (trap) durante la ejecución de un export.
    #[error("trap del guest: {0}")]
    Trap(String),
    /// El guest devolvió un `Err` legible desde su lógica.
    #[error("error del plugin: {0}")]
    Guest(String),
}

/// El estado que vive en el `Store<T>` de wasmtime: contexto WASI (vacío),
/// tabla de recursos, capabilities declaradas, buffer de logs y los recursos
/// scoped que el HOST preparó para el guest.
pub struct HostState {
    ctx: WasiCtx,
    table: ResourceTable,
    caps: Capabilities,
    logs: Vec<String>,
    scoped_resources: HashMap<String, Vec<u8>>,
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
            self.logs.push(message);
        }
    }

    fn read_scoped(&mut self, token: String) -> Result<Vec<u8>, String> {
        // ENFORCEMENT host-side (ADR 0022 D4): sin la capability declarada, ni
        // siquiera se mira el token.
        if !self.caps.fs_read.granted() {
            return Err("fs-read no declarada".into());
        }
        match self.scoped_resources.get(&token) {
            Some(bytes) => Ok(bytes.clone()),
            None => Err("token desconocido".into()),
        }
    }
}

/// El motor wasmtime del host, reutilizable entre instanciaciones.
pub struct PluginRuntime {
    engine: Engine,
}

impl PluginRuntime {
    /// Construye el motor con el Component Model activado.
    ///
    /// # Errors
    /// Falla si la configuración del motor wasmtime es inválida en esta
    /// plataforma.
    pub fn new() -> Result<Self, RuntimeError> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config).map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(Self { engine })
    }

    /// Instancia un plugin desde un componente WASM en disco, con las
    /// `caps` declaradas y un sandbox WASI VACÍO.
    ///
    /// # Errors
    /// - [`RuntimeError::Component`] si el artefacto no es un componente válido.
    /// - [`RuntimeError::Instantiate`] si el linker o la instanciación fallan.
    pub fn instantiate(
        &self,
        wasm_path: &Path,
        caps: Capabilities,
    ) -> Result<PluginInstance, RuntimeError> {
        let component = Component::from_file(&self.engine, wasm_path)
            .map_err(|e| RuntimeError::Component(e.to_string()))?;

        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        host_log::add_to_linker::<HostState, wasmtime::component::HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        // Sandbox WASI VACÍO: sin stdio heredado, sin preopens, sin red, sin
        // env. Esta línea es el corazón del aislamiento (revísala, seguridad).
        let ctx = WasiCtxBuilder::new().build();
        let state = HostState {
            ctx,
            table: ResourceTable::new(),
            caps,
            logs: Vec::new(),
            scoped_resources: HashMap::new(),
        };

        // TODO M4-P2b: límite de memoria por store (Store::limiter +
        // StoreLimitsBuilder) — de momento el motor usa los defaults.
        let mut store = Store::new(&self.engine, state);
        let bindings = NortePlugin::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        Ok(PluginInstance { store, bindings })
    }
}

/// Una instancia viva de un plugin: su `Store` (estado host) y los bindings del
/// world para llamar a sus exports.
pub struct PluginInstance {
    store: Store<HostState>,
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
    /// Las líneas de log que el plugin ha acumulado vía `host-log::log`.
    #[must_use]
    pub fn logs(&self) -> &[String] {
        &self.store.data().logs
    }

    /// Prepara un recurso scoped que el guest podrá leer con `read-scoped`
    /// usando `token` (solo si declaró `fs-read`).
    pub fn preload_scoped(&mut self, token: &str, bytes: Vec<u8>) {
        self.store
            .data_mut()
            .scoped_resources
            .insert(token.to_owned(), bytes);
    }

    /// Invoca el export `previewer::render` del guest.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] si el guest atrapa.
    /// - [`RuntimeError::Guest`] si el guest devuelve un `Err` de lógica.
    pub fn render_preview(
        &mut self,
        mimetype: &str,
        content: &[u8],
    ) -> Result<String, RuntimeError> {
        let input = PreviewInput {
            mimetype: mimetype.to_owned(),
            content: content.to_vec(),
        };
        self.bindings
            .norte_plugin_previewer()
            .call_render(&mut self.store, &input)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?
            .map_err(RuntimeError::Guest)
    }

    /// Invoca el export `command::run` del guest.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] si el guest atrapa.
    /// - [`RuntimeError::Guest`] si el guest devuelve un `Err` de lógica.
    pub fn run_command(&mut self, id: &str, arg: &str) -> Result<String, RuntimeError> {
        self.bindings
            .norte_plugin_command()
            .call_run(&mut self.store, id, arg)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?
            .map_err(RuntimeError::Guest)
    }
}
