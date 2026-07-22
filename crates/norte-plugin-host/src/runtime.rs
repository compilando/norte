//! Runtime wasmtime del host de plugins (M4-P2, ADR 0022 D1/D4): motor,
//! sandbox WASI VACÍO, instanciación del Component Model y el ENFORCEMENT
//! host-side de las capabilities (`fs-read`).
//!
//! El enforcement vive en el HOST, no en el guest (ADR 0022 D4): un plugin
//! hostil no puede evadir el gating porque la comprobación se hace en la
//! implementación host de `host-log::read-scoped`, antes de entregar bytes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use thiserror::Error;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::NortePlugin;
use crate::bindings::exports::norte::plugin::previewer::PreviewInput;
use crate::bindings::norte::plugin::host_log;
use crate::capability::Capabilities;

/// Tope de líneas de log que un plugin puede acumular (anti-DoS: el guest no
/// hace crecer la memoria del host sin límite vía `host-log::log`).
const MAX_LOGS: usize = 1024;

/// Tope de caracteres por línea de log: un guest no puede hacer crecer la
/// memoria del host con una única línea gigante (anti-DoS, complementa
/// [`MAX_LOGS`]). Se trunca en frontera de char (no parte un code point).
const MAX_LOG_CHARS: usize = 4096;

/// Límite de memoria lineal por store del guest (64 MiB, holgado): un plugin no
/// puede agotar la RAM del host haciendo crecer su memoria lineal sin fin.
const MAX_STORE_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Tope del valor de RETORNO del guest (`run_command`/`render_preview`), en
/// bytes (issue #68): un guest no puede hacer crecer la memoria del host
/// devolviendo una `String` gigante. 4 MiB es holgado para texto de preview o un
/// mensaje de barra de estado, y coherente con el tope de lectura de 1 MiB del
/// core al previsualizar. Por encima se RECHAZA (fail-loud), no se trunca a
/// medias — un valor cortado no es el que el plugin quiso devolver.
const MAX_RETURN_BYTES: usize = 4 * 1024 * 1024;

/// Tope del tamaño del ARTEFACTO `.wasm` en disco ANTES de compilarlo (issue
/// #68): compilar un componente con cranelift cuesta CPU y memoria proporcional
/// al tamaño; no se gasta ese trabajo en un artefacto arbitrariamente grande. 64
/// MiB es amplísimo para un componente legítimo (los guests de ejemplo pesan
/// cientos de KiB). Por encima se rechaza sin llegar a `Component::from_file`.
const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// Periodo del hilo "ticker" que incrementa la época del motor. Junto con el
/// deadline por store define el timeout de CPU efectivo (≈ deadline × periodo).
const EPOCH_TICK: Duration = Duration::from_millis(50);

/// Deadline de época por defecto en producción: ≈ [`EPOCH_TICK`] × 200 ≈ 10 s.
/// Un guest que consuma CPU más allá de esto TRAPA (regla dura 3: nada de
/// operaciones largas sin corte). Holgado a propósito para no matar plugins
/// legítimos; los tests usan [`PluginRuntime::with_epoch_deadline`] con un valor
/// mucho menor para no tardar.
const DEFAULT_EPOCH_DEADLINE: u64 = 200;

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
    /// El artefacto `.wasm` en disco supera el tope `MAX_ARTIFACT_BYTES`: se
    /// rechaza ANTES de compilarlo (issue #68).
    #[error("artefacto demasiado grande: {len} bytes (máx {cap})")]
    ArtifactTooLarge {
        /// Tamaño real del `.wasm` en disco.
        len: u64,
        /// Tope permitido (`MAX_ARTIFACT_BYTES`).
        cap: u64,
    },
    /// El valor de retorno del guest supera el tope `MAX_RETURN_BYTES` (issue
    /// #68): se rechaza fail-loud en vez de crecer la memoria del host.
    #[error("valor de retorno del plugin demasiado grande: {len} bytes (máx {cap})")]
    ReturnTooLarge {
        /// Longitud del valor devuelto por el guest.
        len: usize,
        /// Tope permitido (`MAX_RETURN_BYTES`).
        cap: usize,
    },
}

/// Aplica el tope de tamaño al valor de retorno del guest (issue #68). Fail-loud:
/// por encima de `MAX_RETURN_BYTES` devuelve [`RuntimeError::ReturnTooLarge`]
/// en vez de entregar (o truncar) la cadena.
fn cap_return_value(value: String) -> Result<String, RuntimeError> {
    if value.len() > MAX_RETURN_BYTES {
        return Err(RuntimeError::ReturnTooLarge {
            len: value.len(),
            cap: MAX_RETURN_BYTES,
        });
    }
    Ok(value)
}

/// Comprueba que el artefacto en disco no excede [`MAX_ARTIFACT_BYTES`] (issue
/// #68). Separada para poder testear la decisión sin escribir un fichero enorme.
fn check_artifact_size(len: u64) -> Result<(), RuntimeError> {
    if len > MAX_ARTIFACT_BYTES {
        return Err(RuntimeError::ArtifactTooLarge {
            len,
            cap: MAX_ARTIFACT_BYTES,
        });
    }
    Ok(())
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
    /// Límites de recursos del store (memoria lineal). Referenciado por
    /// `Store::limiter` vía [`WasiView`]-adyacente closure en `instantiate`.
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
            // Cap por línea en frontera de char (anti-DoS de línea gigante).
            let capped = if message.chars().count() > MAX_LOG_CHARS {
                message.chars().take(MAX_LOG_CHARS).collect()
            } else {
                message
            };
            self.logs.push(capped);
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
///
/// Arranca un hilo "ticker" que incrementa la época del motor cada `EPOCH_TICK`
/// (50 ms); combinado con el deadline por store ([`Store::set_epoch_deadline`])
/// da un timeout de CPU real para el guest (regla dura 3). El hilo se para
/// limpio en [`Drop`].
pub struct PluginRuntime {
    engine: Engine,
    /// Ticks de época que un store puede consumir antes de trapar.
    epoch_deadline: u64,
    /// Señal de parada del hilo ticker.
    ticker_stop: Arc<AtomicBool>,
    /// Handle del hilo ticker; `take()`-ado en `Drop` para hacer join.
    ticker: Option<JoinHandle<()>>,
}

impl PluginRuntime {
    /// Construye el motor con el Component Model activado y el deadline de época
    /// de producción (`DEFAULT_EPOCH_DEADLINE`, ≈ 10 s de CPU).
    ///
    /// # Errors
    /// Falla si la configuración del motor wasmtime es inválida en esta
    /// plataforma.
    pub fn new() -> Result<Self, RuntimeError> {
        Self::with_epoch_deadline(DEFAULT_EPOCH_DEADLINE)
    }

    /// Como [`PluginRuntime::new`] pero con un deadline de época explícito (en
    /// ticks de `EPOCH_TICK`, 50 ms). Pensado para tests que necesitan un timeout
    /// corto (p. ej. verificar que un guest en bucle trapa sin colgar el host)
    /// sin esperar los ~10 s del default de producción.
    ///
    /// # Errors
    /// Falla si la configuración del motor wasmtime es inválida en esta
    /// plataforma.
    pub fn with_epoch_deadline(epoch_deadline: u64) -> Result<Self, RuntimeError> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        // Interrupción por época: el motor comprueba el deadline en los bordes
        // de bucle/función del guest y trapa al superarlo (regla dura 3).
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        // Hilo ticker: incrementa la época cada EPOCH_TICK hasta que Drop lo
        // pare. `Engine` es Clone (Arc por dentro), así que el hilo comparte el
        // mismo motor.
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
        })
    }

    /// Instancia un plugin desde un componente WASM en disco, con las
    /// `caps` declaradas y un sandbox WASI VACÍO.
    ///
    /// # Errors
    /// - [`RuntimeError::ArtifactTooLarge`] si el `.wasm` en disco supera
    ///   `MAX_ARTIFACT_BYTES` (se rechaza antes de compilar).
    /// - [`RuntimeError::Component`] si el artefacto no es un componente válido.
    /// - [`RuntimeError::Instantiate`] si el linker o la instanciación fallan.
    pub fn instantiate(
        &self,
        wasm_path: &Path,
        caps: Capabilities,
    ) -> Result<PluginInstance, RuntimeError> {
        let (mut store, component, linker) = self.prepare(wasm_path, caps)?;
        let bindings = NortePlugin::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(PluginInstance { store, bindings })
    }

    /// Instancia un guest PROVIDER (world `norte-provider`, #30 stage 2) con el
    /// MISMO sandbox y límites que [`Self::instantiate`]. Devuelve una
    /// [`ProviderInstance`] para llamar a sus exports (`capabilities`/`stat`/
    /// `list-dir`/`read`).
    ///
    /// # Errors
    /// Igual que [`Self::instantiate`].
    pub fn instantiate_provider(
        &self,
        wasm_path: &Path,
        caps: Capabilities,
    ) -> Result<ProviderInstance, RuntimeError> {
        use crate::bindings::provider_world::NorteProvider;
        let (mut store, component, linker) = self.prepare(wasm_path, caps)?;
        let bindings = NorteProvider::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ProviderInstance { store, bindings })
    }

    /// Prepara el `Store` (sandbox WASI vacío + límites + deadline) y el
    /// `Linker` (WASI + `host-log`) comunes a cualquier world, y carga el
    /// componente. El world concreto lo instancia el caller.
    fn prepare(
        &self,
        wasm_path: &Path,
        caps: Capabilities,
    ) -> Result<(Store<HostState>, Component, Linker<HostState>), RuntimeError> {
        // Cap del artefacto ANTES de compilar (issue #68): un `.wasm` gigante no
        // debe gastar CPU/memoria de cranelift. `metadata` es una llamada barata
        // que no lee el contenido; el propio `Component::from_file` fallará luego
        // si el fichero desaparece entre medias.
        let len = std::fs::metadata(wasm_path)
            .map_err(|e| RuntimeError::Component(e.to_string()))?
            .len();
        check_artifact_size(len)?;

        let component = Component::from_file(&self.engine, wasm_path)
            .map_err(|e| RuntimeError::Component(e.to_string()))?;

        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        // Linker WASI COMPLETO a propósito (issue #68, punto 3 — evaluado y
        // DESCARTADO reducirlo): los guests se compilan a `wasm32-wasip2` con la
        // std de Rust, que importa la superficie estándar (wasi:cli, wasi:io,
        // wasi:clocks, wasi:random, wasi:filesystem…) para su runtime (panic,
        // asignación, formateo). Recortar el linker haría fallar la
        // instanciación de guests legítimos por "import no satisfecho", sin ganar
        // seguridad: el aislamiento REAL no es la ausencia de imports en el
        // linker sino el `WasiCtx` VACÍO de abajo — sin preopens, stdio, red ni
        // env, esas interfaces existen pero no conceden NINGUNA capacidad. La
        // puerta con estado (`fs-read` scoped) la sigue gateando el HOST.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        host_log::add_to_linker::<HostState, wasmtime::component::HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        // Sandbox WASI VACÍO: sin stdio heredado, sin preopens, sin red, sin
        // env. Esta línea es el corazón del aislamiento (revísala, seguridad).
        let ctx = WasiCtxBuilder::new().build();
        // Límite de memoria lineal por store (cierra M4-P2b): un guest no puede
        // agotar la RAM del host. `StoreLimits` impl `ResourceLimiter`.
        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_STORE_MEMORY_BYTES)
            .build();
        let state = HostState {
            ctx,
            table: ResourceTable::new(),
            caps,
            logs: Vec::new(),
            scoped_resources: HashMap::new(),
            limits,
        };

        let mut store = Store::new(&self.engine, state);
        // Enforcement del límite de memoria: el limiter apunta a `limits`.
        store.limiter(|s: &mut HostState| &mut s.limits);
        // Timeout de CPU: el guest trapa si consume más de `epoch_deadline`
        // ticks de época (el hilo ticker los avanza). El trap se mapea a
        // `RuntimeError::Trap` en run_command/render_preview.
        store.set_epoch_deadline(self.epoch_deadline);

        Ok((store, component, linker))
    }
}

impl Drop for PluginRuntime {
    /// Para el hilo ticker limpio: señala la parada y hace join para no dejar
    /// hilos huérfanos (ni "thread leak" en tests) al morir el runtime.
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.ticker.take() {
            let _ = handle.join();
        }
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
    /// - [`RuntimeError::ReturnTooLarge`] si el texto devuelto supera
    ///   `MAX_RETURN_BYTES`.
    pub fn render_preview(
        &mut self,
        mimetype: &str,
        content: &[u8],
    ) -> Result<String, RuntimeError> {
        let input = PreviewInput {
            mimetype: mimetype.to_owned(),
            content: content.to_vec(),
        };
        let out = self
            .bindings
            .norte_plugin_previewer()
            .call_render(&mut self.store, &input)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?
            .map_err(RuntimeError::Guest)?;
        cap_return_value(out)
    }

    /// Invoca el export `command::run` del guest.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] si el guest atrapa.
    /// - [`RuntimeError::Guest`] si el guest devuelve un `Err` de lógica.
    /// - [`RuntimeError::ReturnTooLarge`] si el texto devuelto supera
    ///   `MAX_RETURN_BYTES`.
    pub fn run_command(&mut self, id: &str, arg: &str) -> Result<String, RuntimeError> {
        let out = self
            .bindings
            .norte_plugin_command()
            .call_run(&mut self.store, id, arg)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?
            .map_err(RuntimeError::Guest)?;
        cap_return_value(out)
    }
}

/// Tipos del export `provider` (records/enums generados: `Entry`, `Page`,
/// `Caps`, `VfsError`, `EntryKind`) — re-exportados para que el adapter host
/// los use sin cavar en el módulo de bindings generado (#30 stage 2).
pub use crate::bindings::provider_world::exports::norte::plugin::provider as provider_iface;

/// Una instancia viva de un guest PROVIDER (#30 stage 2, world
/// `norte-provider`): su `Store` (estado host + sandbox) y los bindings para
/// llamar a los exports de la interfaz `provider`. Cada método es UNA llamada
/// síncrona al guest; el adapter host (`Provider`) reensambla los streams
/// llamando en bucle (list paginado, read por rango).
pub struct ProviderInstance {
    store: Store<HostState>,
    bindings: crate::bindings::provider_world::NorteProvider,
}

impl std::fmt::Debug for ProviderInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderInstance").finish_non_exhaustive()
    }
}

impl ProviderInstance {
    /// Las capabilities que el guest declara (stage 2: solo `read-only`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn capabilities(&mut self) -> Result<provider_iface::Caps, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_capabilities(&mut self.store)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// `stat` de una entrada por sus segmentos. El `Ok` interno es el resultado
    /// LÓGICO del guest (`Entry` o `VfsError`); el `Err` externo es un trap.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn stat(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<provider_iface::Entry, provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_stat(&mut self.store, segments)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Una PÁGINA del listado de un directorio (equiv. un tramo del
    /// `EntryStream`). `cursor` = `None` empieza; el `next-cursor` de la página
    /// alimenta la siguiente llamada.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn list_dir(
        &mut self,
        segments: &[Vec<u8>],
        cursor: Option<&[u8]>,
    ) -> Result<Result<provider_iface::Page, provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_list_dir(&mut self.store, segments, cursor)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Un RANGO acotado de un fichero (equiv. un chunk del `ByteStream`): a lo
    /// sumo `len` bytes desde `offset`. Se RECHAZA fail-loud
    /// ([`RuntimeError::ReturnTooLarge`]) un valor devuelto mayor que
    /// `MAX_RETURN_BYTES` — no es un guard de asignación (el valor ya se
    /// materializó en memoria del host al bajar del guest; la cota transitoria
    /// real es el límite de 64 MiB del store), sino un rechazo honesto. Deuda
    /// stage-2b: `list_dir`/`stat` aún NO acotan el nº de entradas / longitud de
    /// nombres — el adapter host `Provider` lo hará al reensamblar.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa; [`RuntimeError::ReturnTooLarge`]
    /// si el guest devuelve más de `MAX_RETURN_BYTES`.
    pub fn read(
        &mut self,
        segments: &[Vec<u8>],
        offset: u64,
        len: u64,
    ) -> Result<Result<Vec<u8>, provider_iface::VfsError>, RuntimeError> {
        let out = self
            .bindings
            .norte_plugin_provider()
            .call_read(&mut self.store, segments, offset, len)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_return_value_pasa_por_debajo_del_tope() {
        let ok = "x".repeat(MAX_RETURN_BYTES);
        assert_eq!(cap_return_value(ok.clone()).unwrap().len(), ok.len());
    }

    #[test]
    fn cap_return_value_rechaza_por_encima_del_tope() {
        let big = "x".repeat(MAX_RETURN_BYTES + 1);
        let err = cap_return_value(big).unwrap_err();
        assert!(
            matches!(err, RuntimeError::ReturnTooLarge { len, cap }
                if len == MAX_RETURN_BYTES + 1 && cap == MAX_RETURN_BYTES),
            "fue {err:?}"
        );
    }

    #[test]
    fn check_artifact_size_acepta_en_el_tope_y_rechaza_por_encima() {
        assert!(check_artifact_size(MAX_ARTIFACT_BYTES).is_ok());
        let err = check_artifact_size(MAX_ARTIFACT_BYTES + 1).unwrap_err();
        assert!(
            matches!(err, RuntimeError::ArtifactTooLarge { len, cap }
                if len == MAX_ARTIFACT_BYTES + 1 && cap == MAX_ARTIFACT_BYTES),
            "fue {err:?}"
        );
    }
}
