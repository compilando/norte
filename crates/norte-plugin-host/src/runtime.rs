//! Runtime wasmtime del host de plugins (M4-P2, ADR 0022 D1/D4): motor,
//! sandbox WASI VACÍO, instanciación del Component Model y el ENFORCEMENT
//! host-side de las capabilities (`fs-read`).
//!
//! El enforcement vive en el HOST, no en el guest (ADR 0022 D4): un plugin
//! hostil no puede evadir el gating porque la comprobación se hace en la
//! implementación host de `host-log::read-scoped`, antes de entregar bytes.

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
use crate::bindings::norte::plugin::{host_config, host_log};
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

/// Tope de LÍNEAS de un `render-styled` (ADR 0037 tabla de decisión 1):
/// anti-DoS sobre el nº de líneas que un guest puede devolver de un preview
/// estilizado. Aplicado POST-retorno del guest (regla dura del ADR: rechazo
/// entero, nunca truncado a medias).
const MAX_STYLED_LINES: usize = 10_000;

/// Tope de SPANS por línea de un `render-styled` (ADR 0037 tabla de decisión
/// 1).
const MAX_STYLED_SPANS_PER_LINE: usize = 64;

/// Tope de bytes UTF-8 del `text` de UN span (ADR 0037 tabla de decisión 1:
/// "4 KiB"). Medido en BYTES, no en caracteres — un WIT `string` no impone
/// límite de longitud por sí mismo y "carácter" es ambiguo (code point vs.
/// grafema); bytes es lo único no ambiguo y lo que realmente ocupa memoria.
const MAX_STYLED_SPAN_TEXT_BYTES: usize = 4 * 1024;

/// Tope TOTAL de bytes de `text` sumados de TODOS los spans de un
/// `render-styled` (ADR 0037 tabla de decisión 1): reutiliza el mismo tope
/// que `MAX_RETURN_BYTES` (el cap de retorno del runtime, issue #68) — un
/// preview estilizado no debe poder inflar la memoria del host más que
/// cualquier otro valor de retorno de un guest.
const MAX_STYLED_TOTAL_TEXT_BYTES: usize = MAX_RETURN_BYTES;

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
    /// El `render-styled` del guest supera alguno de los topes de la tabla
    /// ADR 0037 decisión 1 (líneas / spans por línea / bytes por span / bytes
    /// totales): se rechaza ENTERO, fail-closed — nunca se trunca a medias
    /// (el caller cae a la previsualización plana `render`).
    #[error("preview estilizado supera un tope: {0}")]
    StyledPreviewTooLarge(String),
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

/// Aplica los CUATRO topes de `render-styled` (ADR 0037 tabla de decisión 1)
/// al resultado devuelto por el guest, POST-retorno: nº de líneas, spans por
/// línea, bytes UTF-8 de `text` por span, y bytes totales de `text` sumados
/// de TODOS los spans. Fail-closed: la primera violación encontrada rechaza
/// el conjunto ENTERO — nunca se trunca a medias, el caller (norte-core) cae
/// a la previsualización plana.
fn cap_styled_text(lines: Vec<Vec<Span>>) -> Result<Vec<Vec<Span>>, RuntimeError> {
    if lines.len() > MAX_STYLED_LINES {
        return Err(RuntimeError::StyledPreviewTooLarge(format!(
            "{} líneas (máx {MAX_STYLED_LINES})",
            lines.len()
        )));
    }
    let mut total_text_bytes: usize = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.len() > MAX_STYLED_SPANS_PER_LINE {
            return Err(RuntimeError::StyledPreviewTooLarge(format!(
                "línea {i}: {} spans (máx {MAX_STYLED_SPANS_PER_LINE})",
                line.len()
            )));
        }
        for span in line {
            if span.text.len() > MAX_STYLED_SPAN_TEXT_BYTES {
                return Err(RuntimeError::StyledPreviewTooLarge(format!(
                    "span de {} bytes (máx {MAX_STYLED_SPAN_TEXT_BYTES})",
                    span.text.len()
                )));
            }
            total_text_bytes += span.text.len();
        }
    }
    if total_text_bytes > MAX_STYLED_TOTAL_TEXT_BYTES {
        return Err(RuntimeError::StyledPreviewTooLarge(format!(
            "{total_text_bytes} bytes totales de texto (máx {MAX_STYLED_TOTAL_TEXT_BYTES})"
        )));
    }
    Ok(lines)
}

/// Tope agregado sobre un LOTE de `decorate`/`column-values` (mismo
/// `MAX_RETURN_BYTES` que cualquier otro valor de retorno del runtime,
/// issue #68): `len` es la suma de bytes ÚTILES del batch (badges+roles, o
/// valores de columna), no el nº de entradas — un batch grande de celdas
/// diminutas es legítimo, un batch de pocas celdas gigantes no lo es.
fn cap_total_bytes(len: usize) -> Result<(), RuntimeError> {
    if len > MAX_RETURN_BYTES {
        return Err(RuntimeError::ReturnTooLarge {
            len,
            cap: MAX_RETURN_BYTES,
        });
    }
    Ok(())
}

/// El estado que vive en el `Store<T>` de wasmtime: contexto WASI (vacío),
/// tabla de recursos, capabilities declaradas, buffer de logs, los recursos
/// scoped que el HOST preparó para el guest, y los valores de `[config]` (P2
/// Task 3) que el guest puede leer vía `host-config`.
pub struct HostState {
    ctx: WasiCtx,
    table: ResourceTable,
    caps: Capabilities,
    logs: Vec<String>,
    scoped_resources: HashMap<String, Vec<u8>>,
    /// Valores VALIDADOS de `[config]` (P2 decisión 3/4): defaults del
    /// esquema del manifiesto con `config.toml` ya superpuesto —
    /// `norte-plugin-host::resolve_settings` corre ANTES de instanciar (en el
    /// catálogo, Task 2). Vacío por defecto ([`Self`] se construye antes de
    /// que el caller conozca el plugin concreto); [`PluginInstance::set_settings`]/
    /// [`ProviderInstance::set_settings`] lo rellenan ANTES de invocar
    /// cualquier export del guest (mismo patrón que
    /// [`PluginInstance::preload_scoped`]).
    settings: BTreeMap<String, String>,
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

/// `host-config` (P2 Task 3): lecturas PURAS del mapa `settings` ya resuelto
/// — ninguna rama toca FS, red ni reloj, a diferencia de `read-scoped` (que sí
/// gatea contra una capability). No hay nada que gatear aquí: `settings` es
/// SIEMPRE el resultado de una validación fail-closed hecha ANTES de llegar a
/// esta struct (Task 2, `resolve_settings`), así que cualquier clave presente
/// ya es segura de entregar tal cual — el sandbox invariant (regla dura 9,
/// ningún acceso directo del guest al mundo exterior) queda intacto.
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

    /// Instancia un guest DECORATOR (world `norte-decorator`, ADR 0037
    /// decisión 2) con el MISMO sandbox y límites que [`Self::instantiate`].
    /// Devuelve una [`DecoratorInstance`] para llamar a `decorate`.
    ///
    /// # Errors
    /// Igual que [`Self::instantiate`].
    pub fn instantiate_decorator(
        &self,
        wasm_path: &Path,
        caps: Capabilities,
    ) -> Result<DecoratorInstance, RuntimeError> {
        use crate::bindings::decorator_world::NorteDecorator;
        let (mut store, component, linker) = self.prepare(wasm_path, caps)?;
        let bindings = NorteDecorator::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(DecoratorInstance { store, bindings })
    }

    /// Instancia un guest COLUMNS (world `norte-columns`, ADR 0037 decisión
    /// 2) con el MISMO sandbox y límites que [`Self::instantiate`]. Devuelve
    /// una [`ColumnsInstance`] para llamar a `column-values`.
    ///
    /// # Errors
    /// Igual que [`Self::instantiate`].
    pub fn instantiate_columns(
        &self,
        wasm_path: &Path,
        caps: Capabilities,
    ) -> Result<ColumnsInstance, RuntimeError> {
        use crate::bindings::columns_world::NorteColumns;
        let (mut store, component, linker) = self.prepare(wasm_path, caps)?;
        let bindings = NorteColumns::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ColumnsInstance { store, bindings })
    }

    /// Como [`Self::instantiate_provider`] pero desde los BYTES de un componente
    /// en memoria (ADR 0033: el guest FTP va EMBEBIDO en el binario de norte, ya
    /// que el target `wasm32-wasip2` puede faltar en el host de compilación).
    /// Aplica el MISMO sandbox y límites que la ruta de disco, incluido el cap
    /// de tamaño del artefacto.
    ///
    /// # Errors
    /// [`RuntimeError::ArtifactTooLarge`] si los bytes exceden el tope;
    /// [`RuntimeError::Component`] si no son un componente válido;
    /// [`RuntimeError::Instantiate`] si el linker o la instanciación fallan.
    pub fn instantiate_provider_bytes(
        &self,
        bytes: &[u8],
        caps: Capabilities,
    ) -> Result<ProviderInstance, RuntimeError> {
        use crate::bindings::provider_world::NorteProvider;
        // El artefacto embebido es first-party, pero el cap cuesta nada y protege
        // a un futuro caller que pase bytes de terceros (rust review m3).
        check_artifact_size(bytes.len() as u64)?;
        let component = Component::from_binary(&self.engine, bytes)
            .map_err(|e| RuntimeError::Component(e.to_string()))?;
        let (mut store, linker) = self.prepare_common(caps)?;
        let bindings = NorteProvider::instantiate(&mut store, &component, &linker)
            .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;
        Ok(ProviderInstance { store, bindings })
    }

    /// Prepara el `Store` (sandbox WASI vacío + límites + deadline) y el
    /// `Linker` (WASI + `host-log`) comunes a cualquier world, y carga el
    /// componente DESDE DISCO. El world concreto lo instancia el caller.
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
        let (store, linker) = self.prepare_common(caps)?;
        Ok((store, component, linker))
    }

    /// El `Store` (sandbox WASI vacío, límites y deadline) y el `Linker` (WASI
    /// más `host-log`) comunes a cualquier world, SIN cargar el componente: el
    /// caller trae su `Component` (de disco vía [`Self::prepare`], o de bytes
    /// embebidos vía [`Self::instantiate_provider_bytes`], ADR 0033).
    fn prepare_common(
        &self,
        caps: Capabilities,
    ) -> Result<(Store<HostState>, Linker<HostState>), RuntimeError> {
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
        // `host-config` (P2 Task 3): se linka SIEMPRE, para CUALQUIER plugin,
        // igual que `host-log` — un guest 0.4.0 que no la importa simplemente
        // nunca la resuelve al instanciar (el `Linker` puede ofrecer MÁS
        // funciones de las que un world concreto exige; solo un import NO
        // resuelto rompe la instanciación, nunca uno de más).
        host_config::add_to_linker::<HostState, wasmtime::component::HasSelf<_>>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| RuntimeError::Instantiate(e.to_string()))?;

        // Sandbox WASI: sin stdio heredado, sin preopens, sin env. La RED se
        // concede SOLO si la capability `net` está declarada, y aun así
        // RESTRINGIDA a los hosts del allow-list (#30 stage 3a). Sin `net`, el
        // `socket_addr_check` por defecto RECHAZA toda dirección (fail-closed) —
        // el guest existe con `wasi:sockets` linkado pero sin ninguna conexión
        // concedida (mismo principio que `fs-read`: el linker completo, la
        // capacidad la da el HOST). Esta línea es el corazón del aislamiento de
        // red (revísala, seguridad).
        let mut ctx_builder = WasiCtxBuilder::new();
        if let Some(net) = &caps.net {
            // Allow-list por HOST resuelto. SOLO conexiones TCP SALIENTES
            // (`TcpConnect`): se rechazan bind/listen y TODO UDP — un provider de
            // red conecta, no escucha ni manda datagramas (mínimo privilegio,
            // review security). Una entrada `ip:puerto` fija el puerto; una de
            // solo `ip` autoriza CUALQUIER puerto de ese host — es deliberado (el
            // FTP pasivo negocia puertos de datos DINÁMICOS, no acotables a
            // priori) y el humano lo ve al aprobar el manifiesto. Sin DNS en el
            // guest (`allow_ip_name_lookup(false)`): se conecta por IP y el
            // allow-list es por IP; resolver hostnames + deny-list de
            // link-local/metadata (169.254/fe80) es del wiring de stage 3b.
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
            // Vacío hasta que el caller conozca el plugin concreto y llame a
            // `set_settings` (mismo patrón que `scoped_resources`/
            // `preload_scoped`, P2 Task 3) — equivale a un manifiesto sin
            // `[config]`, el comportamiento correcto para cualquier caller
            // que aún no fue extendido para entregar settings.
            settings: BTreeMap::new(),
            limits,
        };

        let mut store = Store::new(&self.engine, state);
        // Enforcement del límite de memoria: el limiter apunta a `limits`.
        store.limiter(|s: &mut HostState| &mut s.limits);
        // Timeout de CPU: el guest trapa si consume más de `epoch_deadline`
        // ticks de época (el hilo ticker los avanza). El trap se mapea a
        // `RuntimeError::Trap` en run_command/render_preview.
        store.set_epoch_deadline(self.epoch_deadline);

        Ok((store, linker))
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

    /// Instala los valores VALIDADOS de `[config]` (P2 Task 3) que el guest
    /// verá vía `host-config::get`/`all`. Debe llamarse ANTES de invocar
    /// cualquier export que pueda leerlos (mismo patrón que
    /// [`Self::preload_scoped`]). Un guest compilado contra un paquete
    /// anterior que no importe `host-config` simplemente nunca llama a estas
    /// funciones — instalar el mapa no cambia su comportamiento ni requiere
    /// que el caller sepa si el guest las usa.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
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

    /// Invoca el export `previewer::render-styled` del guest (ADR 0037
    /// decisión 2): el gemelo con estilo de [`Self::render_preview`]. Los
    /// topes de la tabla de decisión 1 se aplican POST-retorno, ANTES de
    /// devolver al caller (`norte-core`, que valida además `role` contra
    /// `norte_theme::Role` — este crate no conoce ese conjunto, decisión 1).
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] si el guest atrapa.
    /// - [`RuntimeError::Guest`] si el guest devuelve un `Err` de lógica.
    /// - [`RuntimeError::StyledPreviewTooLarge`] si el resultado supera
    ///   alguno de los topes de líneas/spans/bytes-por-span/bytes-totales.
    pub fn render_styled_preview(
        &mut self,
        mimetype: &str,
        content: &[u8],
    ) -> Result<Vec<Vec<Span>>, RuntimeError> {
        let input = PreviewInput {
            mimetype: mimetype.to_owned(),
            content: content.to_vec(),
        };
        let out = self
            .bindings
            .norte_plugin_previewer()
            .call_render_styled(&mut self.store, &input)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?
            .map_err(RuntimeError::Guest)?;
        cap_styled_text(out)
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
    /// Instala los valores VALIDADOS de `[config]` (P2 Task 3) que el guest
    /// PROVIDER verá vía `host-config::get`/`all`. Mismo contrato que
    /// [`PluginInstance::set_settings`]: llamar ANTES de invocar cualquier
    /// export.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

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

    /// Configura la conexión del guest-provider (#30 stage 3c): endpoint YA
    /// resuelto por el host, credenciales y base. El `Ok` interno es el
    /// resultado lógico del guest; el `Err` externo es un trap. Un provider sin
    /// conexión (mem) lo implementa como no-op.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn configure(
        &mut self,
        cfg: &provider_iface::ProviderConfig,
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_configure(&mut self.store, cfg)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    // ---- escritura (#30 stage 2b-write) ----

    /// Abre un `writer` transaccional sobre `segments` (equiv.
    /// `Provider::write`). Devuelve el handle del recurso del guest; el caller
    /// DEBE liberarlo con [`Self::writer_drop`] tras `commit`/`abort`.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn open_writer(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<ResourceAny, provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_open_writer(&mut self.store, segments)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Añade un chunk al staging del `writer`.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn writer_write(
        &mut self,
        writer: ResourceAny,
        chunk: &[u8],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .writer()
            .call_write(&mut self.store, writer, chunk)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Publica el staging del `writer` en el path final.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn writer_commit(
        &mut self,
        writer: ResourceAny,
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .writer()
            .call_commit(&mut self.store, writer)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Descarta el staging del `writer` sin publicar.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn writer_abort(
        &mut self,
        writer: ResourceAny,
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .writer()
            .call_abort(&mut self.store, writer)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Libera el handle del `writer` (drop del recurso del guest). Se llama
    /// SIEMPRE tras `commit`/`abort`.
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el drop del guest atrapa.
    pub fn writer_drop(&mut self, writer: ResourceAny) -> Result<(), RuntimeError> {
        writer
            .resource_drop(&mut self.store)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Crea un directorio (equiv. `Provider::mkdir`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn make_dir(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_make_dir(&mut self.store, segments)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Borra una entrada (equiv. `Provider::remove`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn remove(
        &mut self,
        segments: &[Vec<u8>],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_remove(&mut self.store, segments)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }

    /// Renombra/mueve (equiv. `Provider::rename`).
    ///
    /// # Errors
    /// [`RuntimeError::Trap`] si el guest atrapa.
    pub fn rename(
        &mut self,
        src: &[Vec<u8>],
        dst: &[Vec<u8>],
    ) -> Result<Result<(), provider_iface::VfsError>, RuntimeError> {
        self.bindings
            .norte_plugin_provider()
            .call_rename(&mut self.store, src, dst)
            .map_err(|e| RuntimeError::Trap(e.to_string()))
    }
}

/// Tipos del export `decorator` (record `Decoration`) — re-exportados igual
/// que [`provider_iface`], para que el adapter host los use sin cavar en el
/// módulo de bindings generado (ADR 0037 decisión 2).
pub use crate::bindings::decorator_world::exports::norte::plugin::decorator as decorator_iface;

/// Tipos del export `previewer` (record `Span`, alias `PreviewInput`) —
/// re-exportados igual que [`provider_iface`]/[`decorator_iface`]: el
/// caller (`norte-core`, G3a) necesita construir el `Vec<Vec<Span>>` que
/// devuelve [`PluginInstance::render_styled_preview`] hacia el tipo de
/// wire `SpanWire` (`norte-proto`) sin cavar en `crate::bindings`.
pub use crate::bindings::exports::norte::plugin::previewer as previewer_iface;

/// Una instancia viva de un guest DECORATOR (ADR 0037 decisión 2, world
/// `norte-decorator`): su `Store` (estado host + sandbox) y los bindings para
/// llamar a `decorate`.
pub struct DecoratorInstance {
    store: Store<HostState>,
    bindings: crate::bindings::decorator_world::NorteDecorator,
}

impl std::fmt::Debug for DecoratorInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecoratorInstance").finish_non_exhaustive()
    }
}

impl DecoratorInstance {
    /// Instala los valores VALIDADOS de `[config]` que el guest DECORATOR
    /// verá vía `host-config::get`/`all`. Mismo contrato que
    /// [`PluginInstance::set_settings`]: llamar ANTES de invocar `decorate`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Decora un LOTE de entradas (batched per visible page, ADR 0037
    /// decisión 2): `entries` son los nombres/paths crudos en el orden en
    /// que el host los lista; el resultado es POSICIONAL 1:1 — nunca
    /// reordenado, nunca disperso. Aplica el mismo tope agregado
    /// `MAX_RETURN_BYTES` que cualquier otro valor de retorno del runtime
    /// (issue #68), sumando los bytes de `badge`+`role` de TODAS las
    /// decoraciones del lote.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] si el guest atrapa.
    /// - [`RuntimeError::ReturnTooLarge`] si el lote devuelto supera el tope
    ///   agregado.
    pub fn decorate(
        &mut self,
        entries: &[Vec<u8>],
    ) -> Result<Vec<decorator_iface::Decoration>, RuntimeError> {
        let out = self
            .bindings
            .norte_plugin_decorator()
            .call_decorate(&mut self.store, entries)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?;
        let total: usize = out
            .iter()
            .map(|d| d.badge.as_deref().map_or(0, str::len) + d.role.as_deref().map_or(0, str::len))
            .sum();
        cap_total_bytes(total)?;
        Ok(out)
    }
}

/// Una instancia viva de un guest COLUMNS (ADR 0037 decisión 2, world
/// `norte-columns`): su `Store` (estado host + sandbox) y los bindings para
/// llamar a `column-values`.
pub struct ColumnsInstance {
    store: Store<HostState>,
    bindings: crate::bindings::columns_world::NorteColumns,
}

impl std::fmt::Debug for ColumnsInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnsInstance").finish_non_exhaustive()
    }
}

impl ColumnsInstance {
    /// Instala los valores VALIDADOS de `[config]` que el guest COLUMNS verá
    /// vía `host-config::get`/`all`. Mismo contrato que
    /// [`PluginInstance::set_settings`]: llamar ANTES de invocar
    /// `column-values`.
    pub fn set_settings(&mut self, settings: BTreeMap<String, String>) {
        self.store.data_mut().settings = settings;
    }

    /// Valores de la columna `id` para un LOTE de entradas: mismo contrato
    /// posicional 1:1 que [`DecoratorInstance::decorate`]. Cada celda es
    /// `Option<String>` — `None` = "no aplica a esta entrada", distinguible
    /// de un valor real vacío (ADR 0037 decisión 1). Aplica el mismo tope
    /// agregado `MAX_RETURN_BYTES`, sumando los bytes de las celdas
    /// `Some`.
    ///
    /// # Errors
    /// - [`RuntimeError::Trap`] si el guest atrapa.
    /// - [`RuntimeError::ReturnTooLarge`] si el lote devuelto supera el tope
    ///   agregado.
    pub fn column_values(
        &mut self,
        id: &str,
        entries: &[Vec<u8>],
    ) -> Result<Vec<Option<String>>, RuntimeError> {
        let out = self
            .bindings
            .norte_plugin_columns()
            .call_column_values(&mut self.store, id, entries)
            .map_err(|e| RuntimeError::Trap(e.to_string()))?;
        let total: usize = out.iter().map(|v| v.as_deref().map_or(0, str::len)).sum();
        cap_total_bytes(total)?;
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

    /// Un `HostState` mínimo (sin motor/engine: los campos WASI se
    /// construyen sueltos) para probar `host_config::Host` DIRECTAMENTE, sin
    /// compilar ningún componente WASM (P2 Task 3) — cubre `get`/`all` de
    /// forma barata en cualquier toolchain, incluidas las que no tienen el
    /// target `wasm32-wasip2`.
    fn bare_host_state(settings: BTreeMap<String, String>) -> HostState {
        HostState {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            caps: Capabilities::default(),
            logs: Vec::new(),
            scoped_resources: HashMap::new(),
            settings,
            limits: StoreLimitsBuilder::new().build(),
        }
    }

    #[test]
    fn host_config_get_devuelve_el_valor_o_none() {
        let mut state = bare_host_state(BTreeMap::from([(
            "greeting".to_string(),
            "hola".to_string(),
        )]));
        assert_eq!(
            host_config::Host::get(&mut state, "greeting".to_string()),
            Some("hola".to_string())
        );
        assert_eq!(
            host_config::Host::get(&mut state, "no-declarada".to_string()),
            None,
            "una clave ausente del mapa resuelto es None, no un error"
        );
    }

    #[test]
    fn host_config_all_devuelve_todos_los_pares() {
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
    fn host_config_sin_settings_es_mapa_vacio() {
        // El default de `prepare_common` antes de `set_settings` (mismo
        // criterio que un plugin sin `[config]`, Task 1/2): ni `get` ni `all`
        // deben devolver nada, jamás panicar. `HostState`/`host_config::Host`
        // es la ÚNICA implementación compartida por AMBOS worlds (`with:` en
        // bindings.rs) — este test cubre tanto `PluginInstance` (command/
        // previewer) como `ProviderInstance` (P2 Task 4a: el caso concreto de
        // un provider que nunca llama a `set_settings`, p. ej. FTP hoy, ver
        // `norte_core::plugin_provider`/`ftp_plugin`).
        let mut state = bare_host_state(BTreeMap::new());
        assert_eq!(
            host_config::Host::get(&mut state, "cualquiera".to_string()),
            None
        );
        assert!(host_config::Host::all(&mut state).is_empty());
    }
}
