//! Registro de plugins del daemon (M4-P3): descubre el catálogo local
//! ([`norte_plugin_host::Catalog`]), le fusiona el estado aprobado/activado que
//! persiste el usuario, y lo expone por protocolo ([`norte_proto::methods`]).
//!
//! El [`PluginEntry`](norte_plugin_host::PluginEntry) del catálogo nace SIEMPRE
//! `approved = false` / `enabled = false` (el descubridor no conoce el estado
//! del usuario): la verdad del estado vive en `plugins-state.toml` y este
//! registro es quien la fusiona.
//!
//! ## Formato de `plugins-state.toml`
//!
//! El id de un plugin es reverse-DNS (`org.norte.demo`) — CON PUNTOS. Escrito a
//! pelo como cabecera (`[org.norte.demo]`) TOML lo leería como tablas anidadas
//! (`org` → `norte` → `demo`), NO como una clave literal. Por eso el estado va
//! bajo una tabla `[plugins]` con la clave ENTRECOMILLADA:
//!
//! ```toml
//! [plugins]
//! "org.norte.demo" = { approved = true, enabled = false }
//! ```
//!
//! `toml_edit` entrecomilla la clave con puntos al re-emitir, así que el
//! round-trip descubrir → persistir → descubrir conserva el id intacto.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use norte_plugin_host::Catalog;
use norte_proto::methods::{
    PluginColumnInfo, PluginCommandInfo, PluginInfo, PluginListResult, PluginLoadError,
};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

/// Los bytes CRUDOS de un `OsStr`, o `None` si esta plataforma no los tiene.
///
/// En Unix son los bytes literales y no hay más que decir.
///
/// Fuera de Unix devuelve `None` **a propósito**, y no la forma lossy. Mandar
/// lossy sería peor que no mandar nada: el receptor toma `dir_bytes` por
/// crudos, así que unos bytes ya convertidos le devuelven `lossy = false,
/// masked = false` —un nombre alterado declarándose fiel— y además DESACTIVAN
/// la heurística de respaldo, que es lo único que hoy marca un sustituto
/// suelto de Windows. Con `None` el receptor cae a `dir` y a esa heurística,
/// que es exactamente lo que hacía antes de #265.
///
/// La conversión correcta allí es WTF-8 (la convención que documenta
/// `norte_proto::methods::Volume::label`), y llegará con el resto del soporte
/// de Windows.
// En Unix el `None` no existe —lo elimina el `cfg`— y clippy ve un `Option`
// que siempre es `Some`. Fuera de Unix es la única rama, y es la que hace
// correcto al campo del wire.
#[cfg_attr(unix, allow(clippy::unnecessary_wraps))]
fn bytes_de(s: &std::ffi::OsStr) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(s.as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        let _ = s;
        None
    }
}

/// Estado que el usuario fija sobre un plugin descubierto. Ausente = ambos
/// `false` (descubierto pero sin aprobar ni activar).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginState {
    /// Un humano aprobó las capabilities declaradas.
    pub approved: bool,
    /// Un humano lo tiene activado.
    pub enabled: bool,
    /// Digest (hex sha256) de las capabilities que el humano vio al aprobar
    /// (issue #69, defensa confused-deputy TOCTOU). `None` = aprobación sin
    /// digest anclado (estado heredado de antes de esta defensa, o sin aprobar):
    /// se trata fail-closed como NO-casante, forzando un re-consentimiento. Al
    /// aprobar se fija al digest ACTUAL del manifiesto; al revocar se limpia.
    pub approved_digest: Option<String>,
}

/// Fallo al ejecutar un comando de plugin. Los tres primeros variantes son el
/// veredicto fail-closed del consentimiento (desconocido / sin aprobar /
/// desactivado); los dos últimos, fallos de artefacto o de runtime.
#[derive(Debug, thiserror::Error)]
pub enum PluginRunError {
    /// No hay ningún plugin descubierto con ese id.
    #[error("plugin desconocido: {0}")]
    Unknown(String),
    /// El plugin existe pero un humano no ha aprobado sus capabilities.
    #[error("plugin sin aprobar: {0}")]
    NotApproved(String),
    /// El plugin está aprobado pero desactivado.
    #[error("plugin desactivado: {0}")]
    Disabled(String),
    /// El plugin no tiene `plugin.wasm` en su directorio. Lleva el ID (no la
    /// ruta absoluta: revelaría el home del usuario a un agente que llame a
    /// `plugin.run_command` — coherente con la redacción de `list()`,
    /// security-reviewer M4-P4).
    #[error("el plugin {0} no tiene binario (plugin.wasm)")]
    NoBinary(String),
    /// El runtime WASM falló al compilar, instanciar o ejecutar el componente.
    #[error("runtime: {0}")]
    Runtime(#[from] norte_plugin_host::RuntimeError),
}

/// Fallo al fijar UN valor de `[config]` vía [`PluginRegistry::set_config`]
/// (0.28.0, G3c). Igual que [`ConfigValueError`](norte_plugin_host::ConfigValueError)
/// (que envuelve en [`Self::Invalid`]), NINGUNA variante lleva el VALOR
/// submitted — solo la clave (issue #73, mismo criterio).
#[derive(Debug, thiserror::Error)]
pub enum PluginConfigSetError {
    /// No hay ningún plugin descubierto con ese id.
    #[error("plugin desconocido: {0}")]
    Unknown(String),
    /// `key` no está declarada en `[config]` del manifiesto.
    #[error("clave de config desconocida: {0}")]
    UnknownKey(String),
    /// El valor no valida contra el tipo/rango/enum de la clave.
    #[error("valor inválido: {0}")]
    Invalid(#[from] norte_plugin_host::ConfigValueError),
    /// Fallo de I/O al persistir o al re-resolver tras escribir.
    #[error("i/o: {0}")]
    Io(#[source] io::Error),
}

/// Tope de bytes que el core lee de un archivo al PREVISUALIZAR (1 MiB,
/// anti-DoS): el handler del daemon lee como mucho esto y se lo pasa al guest.
/// El guest de M4-P2 tiene ADEMÁS su propio límite; este es la primera barrera,
/// en el lado del host, para no cargar un archivo enorme en memoria solo porque
/// alguien pidió su preview.
pub(crate) const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// Decodifica los bytes ACOTADOS de un fichero para pasárselos al previewer
/// (§6.2, #29): el texto detectado (por `norte-encoding`) viaja como UTF-8 —
/// jamás bytes crudos sobre los que el guest asuma UTF-8 — y un binario
/// (sin encoding de texto) cae a los bytes tal cual (un guest de texto hará su
/// propio lossy). `bytes` YA viene acotado a [`PREVIEW_MAX_BYTES`].
///
/// Devuelve `(contenido, lossy)`: `lossy` es `true` si la decodificación de
/// texto fue LOSSY (`had_errors` — bytes inválidos → `�`), para que el
/// frontend lo señale en modo preview igual que el raw viewer ya marca su
/// propio `had_errors` (#101, `PluginPreview::lossy` en el wire). Un binario
/// (sin encoding de texto) nunca es lossy: sus bytes viajan crudos.
pub(crate) fn decode_for_preview(bytes: Vec<u8>) -> (Vec<u8>, bool) {
    // `< CAP` = el fichero cabía entero (si == CAP pudo quedar truncado: se
    // trata como incompleto, dirección segura — a lo sumo se omite el último
    // char multibyte, jamás se corrompe con `�`).
    let complete = (bytes.len() as u64) < PREVIEW_MAX_BYTES;
    match norte_encoding::detect(&bytes) {
        norte_encoding::Detection::Text { encoding, .. } => {
            let decoded = norte_encoding::decode(&bytes, encoding, complete);
            (decoded.text.into_bytes(), decoded.had_errors)
        }
        norte_encoding::Detection::Binary => (bytes, false),
    }
}

/// Adivina el mimetype por EXTENSIÓN (heurística ligera, sin dep de sniffing).
/// Un archivo sin extensión reconocible → `application/octet-stream` (ningún
/// previewer `text/*` lo tomará). NO lee el contenido. `pub(crate)` para el
/// handler del daemon.
pub(crate) fn guess_mimetype(path: &norte_proto::VPath) -> &'static str {
    let ext = path
        .file_name()
        .map(norte_proto::Segment::as_bytes)
        .and_then(|n| std::str::from_utf8(n).ok())
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()));
    match ext.as_deref() {
        Some("txt" | "rs" | "toml" | "log" | "csv" | "ini" | "conf") => "text/plain",
        // Its own type, so a Markdown previewer can claim it EXACTLY while a
        // `text/*` highlighter keeps everything else (D3).
        Some("md" | "markdown") => "text/markdown",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        _ => "application/octet-stream",
    }
}

/// Convierte las líneas del `render-styled` del runtime de plugins
/// (`Vec<Vec<norte_plugin_host::previewer_iface::Span>>`) al tipo de WIRE
/// (`Vec<Vec<norte_proto::methods::SpanWire>>`, G3a, ADR 0037). Comparte esta
/// única conversión el brazo EMBEBIDO de `Backend::plugin_preview_styled` y
/// el handler `plugin.preview_styled` del daemon (`daemon::server`), para no
/// duplicarla.
///
/// `role` viaja SIN VALIDAR (límite de responsabilidad, enmienda de ADR
/// 0037 decisión 3 en el propio ADR: `norte-core` headless NO depende de
/// `norte-theme`, dueño del conjunto cerrado `Role` — validar aquí exigiría
/// esa dependencia estructural solo para esta superficie). El texto NO se
/// enmascara aquí tampoco: `norte-core` es headless (regla 7, sin display),
/// el enmascarado por span es responsabilidad del FRONTEND (mismo criterio
/// que `PluginPreview::output`, que tampoco se enmascara en el core). Los
/// topes de tamaño (líneas/spans/bytes) YA se aplicaron en
/// `render_styled_preview` (`norte-plugin-host::runtime::cap_styled_text`,
/// POST-retorno del guest) — esta función solo re-forma el tipo, no vuelve a
/// acotar.
pub(crate) fn to_wire_lines(
    lines: Vec<Vec<norte_plugin_host::previewer_iface::Span>>,
) -> Vec<Vec<norte_proto::methods::SpanWire>> {
    lines
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|s| norte_proto::methods::SpanWire {
                    text: s.text,
                    role: s.role,
                    fg: s.fg.map(|(r, g, b)| [r, g, b]),
                })
                .collect()
        })
        .collect()
}

/// Convierte las rutas VISIBLES de una página a las entradas CRUDAS que
/// cruzan al WIT `decorator::decorate`/`columns::column-values` (ADR 0037
/// decisión 2): el BASENAME en bytes crudos (regla 1), NUNCA la ruta
/// completa. Decisión de privacidad, no solo de forma: un decorator/columns
/// ve el nombre de cada entrada visible, no dónde vive en el árbol — el
/// mismo criterio que el guest real (`examples-wasm/decorator-demo`, T2) ya
/// asume en su contrato (`decorator_wit_e2e_positional_roundtrip_wasm_real`
/// pasa basenames como `b"module.rs"`, no paths). POSICIONAL 1:1 con
/// `paths` — una entrada SIN nombre de fichero (path raíz) entrega un
/// basename vacío, nunca se omite, para no romper el contrato posicional.
pub(crate) fn paths_to_basenames(paths: &[norte_proto::VPath]) -> Vec<Vec<u8>> {
    paths
        .iter()
        .map(|p| {
            p.file_name()
                .map(|s| norte_proto::Segment::as_bytes(s).to_vec())
                .unwrap_or_default()
        })
        .collect()
}

/// Convierte el LOTE bruto que devuelve un guest DECORATOR
/// (`DecoratorInstance::decorate`) al tipo de wire
/// (`Vec<norte_proto::methods::DecorationWire>`), VALIDANDO el contrato
/// posicional 1:1 (ADR 0037 tabla de decisión 1) antes de re-formar: si
/// `out.len() != expected_len` el guest violó el contrato (bug del plugin, o
/// runtime que se saltó `cap_total_bytes` de otra forma) — `None` fail-closed
/// (el caller DESCARTA las decoraciones de ESE plugin entero, con aviso; el
/// resto de la página se pinta igual, mismo criterio de fallback que un
/// previewer que no aplica). `role` viaja SIN VALIDAR (mismo límite de
/// responsabilidad que [`to_wire_lines`] — el frontend, no `norte-core`
/// headless, conoce `norte_theme::Role`).
pub(crate) fn decorations_to_wire_checked(
    out: Vec<norte_plugin_host::decorator_iface::Decoration>,
    expected_len: usize,
) -> Option<Vec<norte_proto::methods::DecorationWire>> {
    if out.len() != expected_len {
        return None;
    }
    Some(
        out.into_iter()
            .map(|d| norte_proto::methods::DecorationWire {
                badge: d.badge,
                role: d.role,
            })
            .collect(),
    )
}

/// Valida el contrato posicional 1:1 del LOTE bruto que devuelve un guest
/// COLUMNS (`ColumnsInstance::column_values`): `None` fail-closed si
/// `out.len() != expected_len` (ver [`decorations_to_wire_checked`], mismo
/// criterio). Ya tiene la forma de wire (`Vec<Option<String>>`) — esta
/// función solo GUARDA el contrato, no re-forma.
pub(crate) fn column_values_checked(
    out: Vec<Option<String>>,
    expected_len: usize,
) -> Option<Vec<Option<String>>> {
    (out.len() == expected_len).then_some(out)
}

/// Convierte UNA entrada de [`PluginRegistry::config_keys`] a su forma de
/// wire (0.28.0, G3c, `plugin.get_config`): `kind`/`default`/`min`/`max`/
/// `values`/`description` salen del ESQUEMA (`spec`), `value` del efectivo
/// ya resuelto (parámetro separado, no del esquema). `default` se codifica
/// con el MISMO criterio canónico que
/// `norte_plugin_host::resolve_settings` (`bool` → `"true"`/`"false"`,
/// `int` → decimal) para que `default`/`value` sean directamente
/// comparables por un frontend.
pub(crate) fn config_key_to_wire(
    key: String,
    spec: &norte_plugin_host::ConfigKeySpec,
    value: String,
) -> norte_proto::methods::PluginConfigKeyWire {
    use norte_plugin_host::ConfigKeySpec;
    use norte_proto::methods::PluginConfigKeyWire;
    match spec {
        ConfigKeySpec::String {
            default,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "string".into(),
            default: default.clone(),
            min: None,
            max: None,
            values: Vec::new(),
            description: description.clone(),
            value,
        },
        ConfigKeySpec::Bool {
            default,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "bool".into(),
            default: default.to_string(),
            min: None,
            max: None,
            values: Vec::new(),
            description: description.clone(),
            value,
        },
        ConfigKeySpec::Int {
            default,
            min,
            max,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "int".into(),
            default: default.to_string(),
            min: *min,
            max: *max,
            values: Vec::new(),
            description: description.clone(),
            value,
        },
        ConfigKeySpec::Enum {
            default,
            values,
            description,
        } => PluginConfigKeyWire {
            key,
            kind: "enum".into(),
            default: default.clone(),
            min: None,
            max: None,
            values: values.clone(),
            description: description.clone(),
            value,
        },
    }
}

/// ¿El glob `pat` (`text/*` o exacto `application/json`) casa `mime`?
fn mimetype_matches(pat: &str, mime: &str) -> bool {
    match pat.strip_suffix("/*") {
        Some(prefix) => mime.split('/').next() == Some(prefix),
        None => pat == mime,
    }
}

/// Resultado de [`PluginRegistry::resolve_previewer`]: `(id, name,
/// wasm_path, capabilities, settings)` — factorizado a un alias (en vez de un
/// tuple de 5 elementos in-line) porque clippy `type_complexity` lo pide;
/// `settings` es P2 Task 4a, ver el rustdoc del método.
pub type ResolvedPreviewer = (
    String,
    String,
    PathBuf,
    norte_plugin_host::Capabilities,
    BTreeMap<String, String>,
);

/// Resultado de un elemento de [`PluginRegistry::resolve_decorators`] o de
/// [`PluginRegistry::resolve_columns`]: misma forma `(id, name, wasm_path,
/// capabilities, settings)` que [`ResolvedPreviewer`] — mismo alias en vez de
/// repetir el tuple de 5 elementos (clippy `type_complexity`).
pub type ResolvedDecorator = ResolvedPreviewer;

/// Resultado de [`PluginRegistry::resolve_provider`]: el provider plugin
/// consentido que sirve un scheme, con lo que hace falta para instanciarlo
/// SIN volver a fiarse del disco.
///
/// Un struct y no la tupla de los otros resolvers porque lleva dos cosas más
/// que ellos no necesitan: el digest del binario que el humano aprobó
/// (quien instancia compara los bytes que lee contra él) y el puerto por
/// defecto de la contribución (a qué se concede red).
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    /// Id del plugin.
    pub id: String,
    /// Nombre legible (texto de tercero).
    pub name: String,
    /// Ruta canónica de `plugin.wasm`, verificada dentro del directorio.
    pub wasm: PathBuf,
    /// Digest del `plugin.wasm` tal como lo ancló el catálogo al descubrir:
    /// lo que la aprobación cubre (#241). Quien instancie DEBE leer los
    /// bytes, hashearlos y comparar — una ruta no es una promesa.
    pub wasm_digest: String,
    /// Capabilities del manifiesto (el sandbox las hace cumplir).
    pub capabilities: norte_plugin_host::Capabilities,
    /// Valores de `[config]` resueltos, para `set_settings`.
    pub settings: BTreeMap<String, String>,
    /// `default-port` de la contribución que declara el scheme, si lo trae.
    pub default_port: Option<u16>,
}

/// El trabajo de leer el `help.md` de UN plugin ya resuelto, listo para
/// ejecutarse fuera del reactor (H3e). Se obtiene con
/// [`PluginRegistry::help_job`] y se consume con [`HelpJob::read`].
///
/// Es OPACO: lleva dentro el `dir` que el catálogo guardó al descubrir, y no lo
/// expone. Ese es todo el punto — el llamador consigue algo que puede mover a un
/// `spawn_blocking` sin haber recibido nunca una ruta que pudiera re-derivar del
/// id que vino por el wire, así que la guarda de escape se queda entera dentro
/// del registro en vez de convertirse en una obligación del que llama.
#[derive(Debug, Clone)]
pub(crate) struct HelpJob {
    dir: PathBuf,
}

impl HelpJob {
    /// Verifica y LEE, acotado, el `help.md` del plugin. Sin página legible
    /// (ausente, ilegible o escapada del directorio) devuelve la página en
    /// blanco, indistinguible de un `help.md` vacío: la ayuda es cosmética y no
    /// tiene por qué distinguir esos casos — quien los distingue es
    /// `norte doctor`.
    ///
    /// La guarda de escape ([`norte_plugin_host::verified_child`]) se aplica
    /// AQUÍ, no al construir el trabajo: son tres syscalls y este método corre
    /// en `spawn_blocking`, mientras que construirlo es memoria pura y ocurre
    /// bajo el lock del registro.
    ///
    /// EL TOPE SE APLICA AL LEER, no al decodificar. La guarda comprueba que hay
    /// un fichero regular y NADA sobre su tamaño, así que un plugin puede enviar
    /// `help.md` como un fichero DISPERSO de 100 GiB —unos pocos bytes en un
    /// tarball— y una sola llamada a `plugin.help` intentaría reservar 100 GiB:
    /// abortar por fallo de reserva, o el OOM killer llevándose el daemon con su
    /// journal y toda task en vuelo. Como el método está ABIERTO a un agente y
    /// el plugin no necesita ni aprobación ni activación, sería la primera
    /// lectura sin tope disparable por un agente en el daemon. Se leen como
    /// mucho `max_bytes + 1` bytes: el byte de más es lo que deja a
    /// [`norte_help::cut_and_decode_untrusted`] ver que sobraba y marcar
    /// `truncated` honestamente, en vez de servir un fichero cortado como si
    /// estuviera completo.
    ///
    /// El texto que devuelve NO está enmascarado: lleva verbatim los peligros de
    /// terminal que el plugin escribiera (ESC, controles C0, anulaciones bidi).
    /// Se parsea con `norte_help::parse_untrusted`, que enmascara al construir el
    /// modelo; nunca se pinta ni se loguea en crudo.
    ///
    /// I/O SÍNCRONA: el llamador async lo mete en `spawn_blocking` (regla 2).
    #[must_use]
    pub(crate) fn read(self) -> norte_proto::methods::PluginHelpResult {
        use std::io::Read as _;

        let tope = u64::try_from(norte_help::Limits::untrusted().max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let bytes = norte_plugin_host::verified_child(&self.dir, "help.md")
            .and_then(|p| {
                let f = std::fs::File::open(p).ok()?;
                let mut buf = Vec::new();
                // Un fallo a mitad de lectura degrada a página en blanco, igual
                // que un `help.md` que no se puede abrir: servir lo leído hasta
                // el error lo presentaría como completo.
                f.take(tope).read_to_end(&mut buf).ok()?;
                Some(buf)
            })
            .unwrap_or_default();
        let s = norte_help::cut_and_decode_untrusted(&bytes);
        norte_proto::methods::PluginHelpResult {
            markdown: s.markdown,
            truncated: s.truncated,
            lossy: s.lossy,
        }
    }
}

/// Registro de plugins: catálogo descubierto + estado persistido fusionado.
#[derive(Debug)]
pub struct PluginRegistry {
    config_dir: PathBuf,
    state: BTreeMap<String, PluginState>,
    catalog: Catalog,
}

impl PluginRegistry {
    /// Nombre del fichero de estado dentro de `config_dir`.
    const STATE_FILE: &'static str = "plugins-state.toml";

    /// Descubre el catálogo en `config_dir/plugins/<id>/plugin.toml` y le fusiona
    /// el estado de `config_dir/plugins-state.toml`.
    ///
    /// Un `config_dir/plugins` inexistente = catálogo vacío (no es error). Un
    /// `plugins-state.toml` ausente = estado vacío.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidData`] si `plugins-state.toml` existe pero no es
    /// TOML válido; cualquier otro error de I/O al leerlo se propaga tal cual.
    pub fn discover(config_dir: &Path) -> io::Result<Self> {
        let catalog = Catalog::load_dir(&config_dir.join("plugins"));
        let state = Self::read_state(&config_dir.join(Self::STATE_FILE))?;
        Ok(Self {
            config_dir: config_dir.to_path_buf(),
            state,
            catalog,
        })
    }

    /// Un registro VACÍO anclado en `config_dir`, sin tocar el FS: catálogo sin
    /// plugins y estado sin fusionar. Lo usa el daemon como degradación si el
    /// descubrimiento falla (p. ej. `plugins-state.toml` corrupto): un fichero
    /// de estado roto no debe impedir arrancar. Persistir sobre él re-crea el
    /// estado desde cero bajo `config_dir`.
    #[must_use]
    pub fn empty(config_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            state: BTreeMap::new(),
            catalog: Catalog::default(),
        }
    }

    /// El catálogo descubierto fusionado con el estado persistido, en la forma
    /// del protocolo.
    #[must_use]
    pub fn list(&self) -> PluginListResult {
        let plugins = self
            .catalog
            .plugins
            .iter()
            .map(|e| {
                let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
                PluginInfo {
                    id: e.manifest.id.clone(),
                    name: e.manifest.name.clone(),
                    publisher: e.manifest.publisher.clone(),
                    version: e.manifest.version.clone(),
                    category: e.manifest.category.as_str().to_string(),
                    // Las insignias del manifiesto MÁS el scheme que un
                    // provider reclama (`provider:webdav`): es lo que aprobar
                    // concede —ponerse delante de `webdav://`— y hasta aquí
                    // el humano aprobaba un provider sin ver para qué scheme.
                    capabilities: e
                        .manifest
                        .capabilities
                        .badges()
                        .into_iter()
                        .chain(
                            e.manifest
                                .contributions
                                .provider
                                .iter()
                                .map(|c| format!("provider:{}", c.scheme)),
                        )
                        .collect(),
                    // Aprobación EFECTIVA (issue #69): `approved` en el fichero
                    // pero con el digest de capabilities CASANDO el del manifiesto
                    // actual. Si las capabilities cambiaron en disco tras aprobar,
                    // la UI ve `approved = false` y vuelve a pedir consentimiento.
                    approved: Self::approval_is_current(&st, e),
                    enabled: st.enabled,
                    // (P1) manifest `description` is cosmetic/untrusted, same
                    // as `name`; `commands` mirrors `Contributions.command` in
                    // MANIFEST ORDER (not sorted — matches how the digest
                    // treats contribution order as significant, spec §6).
                    description: e.manifest.description.clone(),
                    commands: e
                        .manifest
                        .contributions
                        .command
                        .iter()
                        .map(|c| PluginCommandInfo {
                            id: c.id.clone(),
                            title: c.title.clone(),
                        })
                        .collect(),
                    // (G3c, 0.28.0) columns mirrors `Contributions.columns`
                    // the SAME way `commands` mirrors `Contributions.command`
                    // above: manifest order, discovery-only (NOT gated on
                    // approved/enabled — a plugin's contributed columns are
                    // metadata a human inspects BEFORE approving, same as
                    // `commands`/`capabilities` already are).
                    columns: e
                        .manifest
                        .contributions
                        .columns
                        .iter()
                        .map(|c| PluginColumnInfo {
                            id: c.id.clone(),
                            header: c.header.clone(),
                        })
                        .collect(),
                    // El ancla que el humano está MIRANDO (#282): es lo que
                    // devuelve al confirmar, y lo que el daemon compara con la
                    // suya antes de conceder. Cubre `category` y
                    // `contributions` —cuándo y cómo se dispara— además de las
                    // capabilities, o sea justo lo que la lista pintada NO
                    // dice.
                    manifest_digest: Some(norte_plugin_host::PluginEntry::approval_anchor(e)),
                    // (H3e, 0.34.0) NO gateado por approved/enabled — la
                    // documentación de un plugin es justo lo que un humano lee
                    // ANTES de aprobarlo, mismo criterio que
                    // `capabilities`/`commands`/`columns`.
                    //
                    // La bandera del WIRE es la ESTRICTA de las dos: el
                    // `is_present` del catálogo es un `is_file` que SIGUE
                    // enlaces (presencia, no permiso — así lo dice su propio
                    // comentario), mientras que `is_servable` ya pasó la
                    // MISMA guarda que aplicará el lector. Si divergen, el par
                    // (`has_help: true`, `markdown: ""`) es exactamente el
                    // oráculo "esa ruta existe y es un fichero regular", y las
                    // dos mitades las lee un agente por `plugin.list` +
                    // `plugin.help`, ninguno de los dos gateado por policy. Y
                    // aun sin el agente, la barra lateral pintaría un nodo que
                    // se abre en blanco.
                    //
                    // Se LEE, no se calcula: `list()` corre en el reactor async
                    // y bajo el lock global de plugins (`handle_plugin_list` lo
                    // llama síncrono desde `dispatch`), así que aplicar la
                    // guarda aquí serían tres syscalls por plugin bloqueando a
                    // todas las demás conexiones sobre un directorio que puede
                    // estar en autofs o NFS — y `plugin.list` está ABIERTO a un
                    // agente. El veredicto se calcula al DESCUBRIR, donde la
                    // I/O ya vive fuera del reactor.
                    has_help: e.help.is_servable(),
                }
            })
            .collect();
        let errors = self
            .catalog
            .errors
            .iter()
            .map(|e| {
                // Solo el NOMBRE del directorio del plugin, nunca la ruta
                // absoluta: revelaría el home del usuario (`~/.config/norte/...`)
                // a un agente que llame a `plugin.list`. El basename basta para
                // que un humano identifique el plugin roto.
                // `file_name()` es `None` para un path acabado en `..`; caer
                // ahí a `as_os_str()` mandaría la ruta ABSOLUTA, que es justo
                // lo que el rustdoc del campo promete que nunca pasa (revela
                // el home del usuario a un agente que llame a `plugin.list`).
                let base = e.dir.file_name().unwrap_or_else(|| "?".as_ref());
                PluginLoadError {
                    dir: base.to_string_lossy().into_owned(),
                    // Y los BYTES al lado (#265): el `to_string_lossy` de
                    // arriba pone `U+FFFD`, que NO es un peligro de terminal,
                    // así que ninguna heurística del receptor puede recuperar
                    // que hubo conversión. Con los bytes la hace él y la
                    // marca, que es la regla de siempre.
                    dir_bytes: bytes_de(base),
                    reason: e.error.to_string(),
                }
            })
            .collect();
        PluginListResult { plugins, errors }
    }

    /// Los directorios que NO cargaron, con su causa TIPADA (a diferencia de
    /// [`Self::list`], que la aplana a texto para el wire). Para quien
    /// diagnostica en local —`norte doctor`— y quiere distinguir un
    /// manifiesto roto de un binario compilado contra otro WIT (ADR 0094).
    #[must_use]
    pub fn load_errors(&self) -> &[norte_plugin_host::LoadError] {
        &self.catalog.errors
    }

    /// El directorio de configuración donde vive `plugins-state.toml`. Lo usa el
    /// daemon para persistir FUERA del lock (regla 2): captura el dir bajo el
    /// lock y escribe en `spawn_blocking`.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Valores EFECTIVOS de `[config]` (P2) para `id`: defaults del esquema
    /// del manifiesto con `config.toml` ya superpuesto y validado —
    /// resueltos al descubrir ([`norte_plugin_host::Catalog::load_dir`], que
    /// excluye a `errors` cualquier plugin cuyo `config.toml` no valide, así
    /// que lo que llega aquí SIEMPRE es válido). `None` si `id` no está en el
    /// catálogo — NUNCA por un `[config]` vacío/ausente, que da `Some` de un
    /// mapa vacío (mismo criterio que
    /// [`norte_plugin_host::Manifest::config`]).
    ///
    /// Host-side ONLY (P2 decisión 5): no cruza el wire directamente — lo
    /// consume `norte doctor` (que corre embebido) y, desde G3c,
    /// [`Self::config_keys`] (que SÍ cruza el wire vía
    /// `plugin.get_config`).
    #[must_use]
    pub fn settings_of(&self, id: &str) -> Option<&BTreeMap<String, String>> {
        self.catalog
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .map(|p| &p.settings)
    }

    /// El `help.md` de `id`, ACOTADO para el wire (H3e).
    ///
    /// `None` si `id` no está en el catálogo. Eso es lo que hace segura la
    /// llamada: `id` viene del WIRE y se usa como CLAVE DE BÚSQUEDA contra los
    /// plugins descubiertos, nunca compuesta en una ruta — la ruta sale del
    /// `dir` que el catálogo guardó al descubrir, así que un `../` en el id no
    /// llega a tocar el sistema de ficheros, solo falla el lookup.
    ///
    /// El fichero debe CANONICALIZAR DENTRO del directorio del plugin
    /// ([`norte_plugin_host::verified_child`], la misma guarda que
    /// `plugin.wasm`): un
    /// `help.md` que es un symlink a `~/.ssh/id_ed25519` o a `/etc/…` se lee
    /// como si no hubiera página. La razón es que esto cruza el wire y un
    /// AGENTE puede pedirlo: sin la guarda, `plugin.help` sería una lectura de
    /// fichero arbitrario POR FUERA del motor de policy y de sus scopes.
    ///
    /// Lo que hace segura la apertura NO es una aprobación: `help_of` NO está
    /// gateado por `approved`/`enabled` (la documentación es justo lo que se lee
    /// ANTES de aprobar), así que el directorio del plugin fue DESCUBIERTO, no
    /// consentido. Lo seguro es la conjunción de tres cosas: el contenido está
    /// ACOTADO (`HelpJob::read`), la ruta NO la controla quien llama
    /// (sale del catálogo, no del wire), y la guarda impide que apunte fuera del
    /// directorio donde el humano ya dejó caer el bundle.
    ///
    /// Un plugin conocido SIEMPRE devuelve `Some`, aunque su `help.md` falte,
    /// no se pueda leer o escape del directorio: en esos casos `markdown` es la
    /// cadena vacía. La ayuda es cosmética y no tiene por qué distinguirse de
    /// "página en blanco" — lo que sí distingue es `norte doctor`, que gatea por
    /// [`Self::announces_help`] (la bandera LAXA, sin la guarda) y toma de aquí
    /// el contenido, y así puede reportar el fichero ausente, ilegible o
    /// escapado; desde el lado del lector, un `help.md` que apunta fuera es
    /// indistinguible de un autor que no escribió nada, y eso merece un aviso.
    ///
    /// El texto que devuelve NO está enmascarado: lleva verbatim los peligros
    /// de terminal que el plugin escribiera (ESC, controles C0, anulaciones
    /// bidi). Se parsea con `norte_help::parse_untrusted`, que enmascara al
    /// construir el modelo; nunca se pinta ni se loguea en crudo.
    ///
    /// I/O SÍNCRONA: el llamador async va por `help_job` +
    /// `spawn_blocking` (regla 2), que además saca la verificación del lock.
    #[must_use]
    pub fn help_of(&self, id: &str) -> Option<norte_proto::methods::PluginHelpResult> {
        self.help_job(id).map(HelpJob::read)
    }

    /// El trabajo de leer el `help.md` de `id`, resuelto contra el catálogo pero
    /// SIN tocar todavía el disco (H3e). `None` si `id` no está descubierto.
    ///
    /// Es la mitad de [`Self::help_of`] que se puede hacer bajo un lock: aquí
    /// solo hay una búsqueda en memoria. La verificación (tres syscalls) y la
    /// lectura viven en [`HelpJob::read`], que el llamador async ejecuta en
    /// `spawn_blocking` con el lock ya soltado (regla 2).
    ///
    /// Devuelve un valor OPACO a propósito: el `dir` que lleva dentro no es
    /// accesible, así que quien lo recibe no puede re-derivar una ruta a partir
    /// del id del wire ni saltarse la guarda. La garantía se queda entera dentro
    /// del registro.
    #[must_use]
    pub(crate) fn help_job(&self, id: &str) -> Option<HelpJob> {
        let entry = self.catalog.plugins.iter().find(|e| e.manifest.id == id)?;
        Some(HelpJob {
            dir: entry.dir.clone(),
        })
    }

    /// `true` si `id` trae un fichero `help.md`, SIN aplicar la guarda de
    /// escape (H3e): la bandera LAXA, el `is_file` que sigue enlaces.
    ///
    /// Existe porque hay dos preguntas distintas y una sola no sirve para las
    /// dos. `PluginInfo::has_help`, que cruza el WIRE, es la ESTRICTA (la misma
    /// guarda que el lector: anunciar `true` y servir `""` sería un oráculo de
    /// rutas). Un DIAGNÓSTICO local necesita la laxa: "el autor puso un
    /// `help.md` y el host se niega a servirlo" es justo el hallazgo que hay que
    /// dar, y con la estricta ese caso desaparece sin dejar rastro — se vuelve
    /// indistinguible de un plugin que no se documentó.
    ///
    /// No la use nada que responda por el wire.
    #[must_use]
    pub fn announces_help(&self, id: &str) -> bool {
        self.catalog
            .plugins
            .iter()
            .any(|e| e.manifest.id == id && e.help.is_present())
    }

    /// Esquema `[config]` de `id` + valores EFECTIVOS, EMPAREJADOS en orden
    /// de clave del manifiesto (0.28.0, G3c): la fuente que alimenta
    /// `plugin.get_config` — cada `(key, spec, value)` se traduce 1:1 a un
    /// `PluginConfigKeyWire` en `norte-core/daemon/server.rs`. `None` si
    /// `id` no está en el catálogo (mismo criterio que
    /// [`Self::settings_of`]); un `[config]` vacío/ausente da `Some(vec![])`,
    /// nunca `None` — el catálogo SÍ conoce el plugin, solo no declara
    /// ninguna clave.
    ///
    /// Invariante: `entry.settings` (resuelto por
    /// [`norte_plugin_host::resolve_settings`] al descubrir) SIEMPRE
    /// contiene un valor para cada clave de `entry.manifest.config` — un
    /// `unwrap_or_default` cubriría una violación de ese invariante sin
    /// panicar (defensa en profundidad, nunca debería activarse en la
    /// práctica).
    #[must_use]
    pub fn config_keys(
        &self,
        id: &str,
    ) -> Option<Vec<(String, norte_plugin_host::ConfigKeySpec, String)>> {
        let entry = self.catalog.plugins.iter().find(|p| p.manifest.id == id)?;
        Some(
            entry
                .manifest
                .config
                .iter()
                .map(|(key, spec)| {
                    let value = entry.settings.get(key).cloned().unwrap_or_default();
                    (key.clone(), spec.clone(), value)
                })
                .collect(),
        )
    }

    /// Valida `value` contra el esquema `[config.<key>]` de `id` (la MISMA
    /// validación que `config.toml`, vía
    /// [`norte_plugin_host::encode_wire_value`]) y, si pasa, persiste +
    /// RE-RESUELVE `settings_of`/[`Self::config_keys`] EN MEMORIA para que
    /// una instanciación futura (o una `plugin.get_config` inmediatamente
    /// después) vea el valor nuevo (0.28.0, G3c). Nunca persiste si la
    /// validación falla (spec S2: "validated against the schema BEFORE
    /// writing").
    ///
    /// # Errors
    /// [`PluginConfigSetError::Unknown`] si `id` no está en el catálogo;
    /// [`PluginConfigSetError::UnknownKey`] si `key` no está declarada en
    /// `[config]`; [`PluginConfigSetError::Invalid`] si el valor no valida
    /// contra el tipo/rango/enum de la clave; [`PluginConfigSetError::Io`]
    /// si falla la escritura o la re-resolución tras escribir.
    pub fn set_config(
        &mut self,
        id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), PluginConfigSetError> {
        let idx = self
            .catalog
            .plugins
            .iter()
            .position(|p| p.manifest.id == id)
            .ok_or_else(|| PluginConfigSetError::Unknown(id.to_string()))?;
        let spec = self.catalog.plugins[idx]
            .manifest
            .config
            .get(key)
            .cloned()
            .ok_or_else(|| PluginConfigSetError::UnknownKey(key.to_string()))?;
        norte_plugin_host::encode_wire_value(key, &spec, value)
            .map_err(PluginConfigSetError::Invalid)?;
        norte_plugin_host::persist_plugin_setting_typed(&self.config_dir, id, key, &spec, value)
            .map_err(PluginConfigSetError::Io)?;
        let manifest = self.catalog.plugins[idx].manifest.clone();
        let dir = self.catalog.plugins[idx].dir.clone();
        let refreshed = norte_plugin_host::resolve_settings(&manifest, &dir).map_err(|e| {
            PluginConfigSetError::Io(io::Error::other(format!(
                "re-resolver config tras escribir: {e}"
            )))
        })?;
        self.catalog.plugins[idx].settings = refreshed;
        Ok(())
    }

    /// Ruta esperada del binario de `id`: `<config_dir>/plugins/<id>/plugin.wasm`.
    /// Para un caller que solo necesita comprobar PRESENCIA sin cargar el
    /// runtime WASM (p. ej. `norte doctor`, H2) — evita que ese caller
    /// duplique el layout con su propio `config_dir.join("plugins")...`.
    /// NO es el mismo camino que `Self::verified_wasm` (que además
    /// canonicaliza y verifica que el binario no escape del directorio del
    /// plugin vía symlink, issue #69 — una defensa que este cálculo puro de
    /// ruta no aplica) ni consulta el catálogo: por convención
    /// (`PluginEntry::dir`'s propio rustdoc) el directorio de un plugin
    /// descubierto es `plugins/<id>/`, pero esta función no lo verifica, solo
    /// lo asume.
    #[must_use]
    pub fn wasm_path(&self, id: &str) -> PathBuf {
        self.config_dir.join("plugins").join(id).join("plugin.wasm")
    }

    /// Copia del estado aprobado/activado, para persistir fuera del lock (el
    /// daemon lo mueve a `spawn_blocking` junto a [`Self::config_dir`], regla 2).
    #[must_use]
    pub fn state_snapshot(&self) -> BTreeMap<String, PluginState> {
        self.state.clone()
    }

    /// Muta EN MEMORIA el estado `approved` de un plugin descubierto, SIN I/O.
    ///
    /// Devuelve `true` si el plugin existe en el catálogo (y se aplicó), o
    /// `false` si el id es desconocido — en cuyo caso no se toca nada (no se
    /// ensucia el estado con plugins fantasma). La persistencia es
    /// responsabilidad del llamante (daemon: `persist_state` en
    /// `spawn_blocking`; embebido: [`Self::set_approval`]).
    pub fn set_approval_in_memory(&mut self, id: &str, approved: bool) -> bool {
        // Se ancla el digest de las capabilities que el humano está viendo AHORA
        // (issue #69): si el `plugin.toml` cambia después, el digest dejará de
        // casar y `resolve_*` re-pedirá consentimiento. Requiere que el id exista
        // en el catálogo (de lo contrario no hay manifiesto que digestar).
        let Some(digest) = self.manifest_digest(id) else {
            return false;
        };
        let st = self.state.entry(id.to_string()).or_default();
        st.approved = approved;
        // Al aprobar se guarda el digest visto; al revocar se limpia (una futura
        // re-aprobación volverá a anclarlo).
        st.approved_digest = approved.then_some(digest);
        true
    }

    /// Muta EN MEMORIA el estado `enabled`. Semántica idéntica a
    /// [`Self::set_approval_in_memory`].
    pub fn set_enabled_in_memory(&mut self, id: &str, enabled: bool) -> bool {
        if !self.is_known(id) {
            return false;
        }
        self.state.entry(id.to_string()).or_default().enabled = enabled;
        true
    }

    /// Fija el estado `approved` de un plugin descubierto y lo persiste, todo en
    /// el MISMO hilo. Es la API para el uso EMBEBIDO, que ya corre dentro de un
    /// `spawn_blocking` (backend del frontend). El daemon NO usa esto: separa la
    /// mutación ([`Self::set_approval_in_memory`]) de la persistencia
    /// (`persist_state`) para no bloquear el reactor (regla 2).
    ///
    /// Devuelve `Ok(true)` si el plugin existe (y se aplicó+persistió), o
    /// `Ok(false)` si el id es desconocido — sin persistir nada.
    ///
    /// # Errors
    /// Errores de I/O al re-leer o escribir `plugins-state.toml`, o
    /// [`io::ErrorKind::InvalidData`] si el fichero existente es TOML corrupto.
    pub fn set_approval(&mut self, id: &str, approved: bool) -> io::Result<bool> {
        if !self.set_approval_in_memory(id, approved) {
            return Ok(false);
        }
        persist_state(&self.config_dir, &self.state)?;
        Ok(true)
    }

    /// Fija el estado `enabled` de un plugin descubierto y lo persiste (uso
    /// EMBEBIDO). Semántica de retorno idéntica a [`Self::set_approval`].
    ///
    /// # Errors
    /// Igual que [`Self::set_approval`].
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> io::Result<bool> {
        if !self.set_enabled_in_memory(id, enabled) {
            return Ok(false);
        }
        persist_state(&self.config_dir, &self.state)?;
        Ok(true)
    }

    /// Valida el consentimiento (fail-closed) y RESUELVE el `.wasm` +
    /// capabilities + `[config]` YA resuelto de un plugin, SIN ejecutarlo. Es
    /// BARATO (lectura del catálogo/estado en memoria + un `is_file`): pensado
    /// para correr bajo el `Mutex<PluginRegistry>` del daemon, que después
    /// ejecuta lo PESADO (`PluginRuntime::instantiate` + `run_command`, que
    /// compila el componente WASM) FUERA del lock, en un `spawn_blocking`
    /// (regla 2). El `.wasm` es `<dir>/plugin.wasm` por convención (ADR 0022
    /// D6); las capabilities son las DEL MANIFIESTO (el sandbox de M4-P2 las
    /// hace cumplir).
    ///
    /// `settings` (P2 Task 4a) son los valores de `[config]` YA resueltos
    /// (Task 2) — el caller debe pasarlos a `PluginInstance::set_settings`
    /// ANTES de invocar el comando para que el guest los vea vía `host-config`
    /// (Task 3); [`Self::run_command`] ya lo hace, y `handle_plugin_run_command`
    /// del daemon (que resuelve bajo lock y ejecuta fuera de él, sin poder
    /// reusar `run_command` directamente) también.
    ///
    /// # Errors
    /// [`PluginRunError`] `Unknown`/`NotApproved`/`Disabled`/`NoBinary` según el
    /// veredicto de consentimiento; nunca `Runtime` (no ejecuta nada).
    pub fn resolve_runnable(
        &self,
        id: &str,
    ) -> Result<
        (
            PathBuf,
            norte_plugin_host::Capabilities,
            BTreeMap<String, String>,
        ),
        PluginRunError,
    > {
        let entry = self
            .catalog
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| PluginRunError::Unknown(id.to_string()))?;
        let st = self.state.get(id).cloned().unwrap_or_default();
        // Fail-closed: sin aprobación vigente cuyo digest CASE las capabilities
        // actuales (issue #69), se trata como sin aprobar — aunque el flag
        // `approved` siga a `true` en disco (el manifiesto cambió tras aprobar).
        if !Self::approval_is_current(&st, entry) {
            return Err(PluginRunError::NotApproved(id.to_string()));
        }
        if !st.enabled {
            return Err(PluginRunError::Disabled(id.to_string()));
        }
        let wasm = Self::verified_wasm(&entry.dir)
            .ok_or_else(|| PluginRunError::NoBinary(id.to_string()))?;
        Ok((
            wasm,
            entry.manifest.capabilities.clone(),
            entry.settings.clone(),
        ))
    }

    /// Resuelve el previewer APROBADO y ACTIVADO que declara `mime`,
    /// devolviendo `(id, name, wasm_path, capabilities, settings)`; `None` si
    /// ninguno aplica. Fail-closed: un previewer no consentido jamás se
    /// elige. Barato: el caller lee los bytes del archivo y ejecuta fuera del
    /// lock.
    ///
    /// **Exacto antes que glob** (D3, ADR 0037 enmienda): un plugin que
    /// declara `text/markdown` gana a uno que declara `text/*` para un
    /// `.md`, esté donde esté en el orden del catálogo; entre iguales, el
    /// primero por orden de catálogo (`category, id`). Sin esto, quién
    /// pintaba un Markdown lo decidía el alfabeto de los ids.
    ///
    /// `settings` (P2 Task 4a) son los valores de `[config]` YA resueltos
    /// ([`Self::settings_of`]) — el caller debe pasarlos a
    /// `PluginInstance::set_settings` ANTES de `render_preview` para que el
    /// guest los vea vía `host-config` (Task 3), igual que
    /// [`Self::run_command`] ya hace para los comandos.
    #[must_use]
    pub fn resolve_previewer(&self, mime: &str) -> Option<ResolvedPreviewer> {
        // Dos pasadas: la exacta gana a la de comodín aunque venga después.
        let exact = self.previewer_matching(|pat| pat == mime);
        exact.or_else(|| self.previewer_matching(|pat| mimetype_matches(pat, mime)))
    }

    /// El primer previewer consentido, en orden de catálogo, con alguna
    /// declaración de mimetype que satisfaga `casa`.
    fn previewer_matching(&self, casa: impl Fn(&str) -> bool) -> Option<ResolvedPreviewer> {
        self.catalog.plugins.iter().find_map(|e| {
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            // Fail-closed con digest vigente (issue #69): un previewer cuyo
            // manifiesto cambió tras aprobar NO se elige hasta re-consentir.
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            let handles = e
                .manifest
                .contributions
                .previewer
                .iter()
                .flat_map(|c| c.mimetypes.iter())
                .any(|pat| casa(pat));
            if !handles {
                return None;
            }
            let wasm = Self::verified_wasm(&e.dir)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                e.settings.clone(),
            ))
        })
    }

    /// Resuelve TODOS los decorators APROBADOS y ACTIVADOS (ADR 0037
    /// decisión 2), a diferencia de [`Self::resolve_previewer`] (que elige
    /// el PRIMERO que casa): una página se decora con la superposición de
    /// TODOS los plugins `decorator` consentidos — un badge de
    /// "modificado por git" y otro de "bajo revisión" pueden convivir en la
    /// misma entrada. Filtra por `category == Decorator` (a diferencia de
    /// `resolve_previewer`, que no filtra por categoría porque
    /// previewer/command comparten el MISMO world `norte-plugin`; decorator
    /// tiene su PROPIO world `norte-decorator`, así que solo un plugin cuyo
    /// binario lo implementa debe entrar aquí). Orden: el del catálogo
    /// (`category, id` — determinista, ver [`Catalog::load_dir`]).
    #[must_use]
    pub fn resolve_decorators(&self) -> Vec<ResolvedDecorator> {
        self.catalog
            .plugins
            .iter()
            .filter_map(|e| {
                if e.manifest.category != norte_plugin_host::Category::Decorator {
                    return None;
                }
                let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
                if !Self::approval_is_current(&st, e) || !st.enabled {
                    return None;
                }
                let wasm = Self::verified_wasm(&e.dir)?;
                Some((
                    e.manifest.id.clone(),
                    e.manifest.name.clone(),
                    wasm,
                    e.manifest.capabilities.clone(),
                    e.settings.clone(),
                ))
            })
            .collect()
    }

    /// Resuelve el provider plugin APROBADO y ACTIVADO que declara `scheme`
    /// en `contributions.provider[].scheme`: el que sirve `scheme://`.
    ///
    /// Hasta aquí un provider se declaraba, se aprobaba y se activaba, y
    /// NADIE lo resolvía: el `ConnectionManager` casaba schemes a mano contra
    /// los providers del core y un guest FTP embebido. Esta es la mitad del
    /// registro que faltaba; la otra es que el manager pregunte.
    ///
    /// Primero que case, como [`Self::resolve_columns`]: dos plugins
    /// consentidos que reclamen el mismo scheme son una colisión de
    /// configuración, y el orden del catálogo (`category, id`) la hace al
    /// menos determinista. Filtra por `category == Provider`, mismo
    /// razonamiento de world dedicado que [`Self::resolve_decorators`]: solo
    /// un binario que implementa `norte-provider` debe instanciarse como tal.
    ///
    /// Los schemes del core ([`norte_plugin_host::CORE_SCHEMES`]) no se
    /// sirven NUNCA desde aquí, aunque una entrada del catálogo los declare:
    /// el manifiesto ya los rechaza al parsear, y esta es la segunda puerta,
    /// la que se puede probar sin pasar por la primera.
    ///
    /// Un plugin que declara el scheme pero no está consentido se anota en el
    /// log: la respuesta al usuario es `Unsupported` —la misma que un scheme
    /// que nadie sirve— y el panel de registro es donde se lee el porqué.
    #[must_use]
    pub fn resolve_provider(&self, scheme: &str) -> Option<ResolvedProvider> {
        if norte_plugin_host::CORE_SCHEMES.contains(&scheme) {
            return None;
        }
        self.catalog.plugins.iter().find_map(|e| {
            if e.manifest.category != norte_plugin_host::Category::Provider {
                return None;
            }
            let contrib = e
                .manifest
                .contributions
                .provider
                .iter()
                .find(|c| c.scheme == scheme)?;
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                tracing::warn!(
                    plugin = %e.manifest.id,
                    scheme,
                    "declara el scheme pero no está aprobado y activado"
                );
                return None;
            }
            let wasm = Self::verified_wasm(&e.dir)?;
            let wasm_digest = e.wasm_digest.clone()?;
            Some(ResolvedProvider {
                id: e.manifest.id.clone(),
                name: e.manifest.name.clone(),
                wasm,
                wasm_digest,
                capabilities: e.manifest.capabilities.clone(),
                settings: e.settings.clone(),
                default_port: contrib.default_port,
            })
        })
    }

    /// Resuelve el plugin `columns` APROBADO y ACTIVADO que declara la
    /// columna `column_id` en `contributions.columns[].id` (M4 declaró la
    /// contribución, ADR 0037 la respalda con WIT/host). A diferencia de
    /// `resolve_decorators`, aquí SÍ el primero que casa basta (una columna
    /// con ese id la aporta como mucho un plugin con sentido — dos plugins
    /// declarando el MISMO id de columna es una colisión de configuración
    /// del usuario, no algo que este método deba resolver mezclando
    /// valores). Filtra por `category == Columns`, mismo razonamiento de
    /// world dedicado que [`Self::resolve_decorators`].
    #[must_use]
    pub fn resolve_columns(&self, column_id: &str) -> Option<ResolvedDecorator> {
        self.resolve_columns_of(None, column_id)
    }

    /// Como [`Self::resolve_columns`], pero pudiendo exigir QUÉ plugin
    /// (0.35.0, #120).
    ///
    /// Con `plugin_id = Some(p)` solo se considera `p`: si no está aprobado,
    /// activado, o no declara `column_id`, la respuesta es `None` — JAMÁS otro
    /// plugin. Caer al primero que case sería el fallo original con un
    /// parámetro más: dos plugins consentidos que declaren `status` hacían que
    /// una columna configurada como `plugin:a/status` pintara los valores de
    /// `b`, y ninguna capa lo notaba porque cada una comprobaba lo suyo (el
    /// frontend, que el plugin configurado declare la columna; el host, que
    /// alguien la declare).
    ///
    /// Con `plugin_id = None` se conserva el comportamiento anterior —
    /// primero que case— porque es lo que un cliente 0.34 espera, y lo que ya
    /// se comía.
    #[must_use]
    pub fn resolve_columns_of(
        &self,
        plugin_id: Option<&str>,
        column_id: &str,
    ) -> Option<ResolvedDecorator> {
        self.catalog.plugins.iter().find_map(|e| {
            if e.manifest.category != norte_plugin_host::Category::Columns {
                return None;
            }
            if plugin_id.is_some_and(|want| want != e.manifest.id) {
                return None;
            }
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            if !Self::approval_is_current(&st, e) || !st.enabled {
                return None;
            }
            let declares = e
                .manifest
                .contributions
                .columns
                .iter()
                .any(|c| c.id == column_id);
            if !declares {
                return None;
            }
            let wasm = Self::verified_wasm(&e.dir)?;
            Some((
                e.manifest.id.clone(),
                e.manifest.name.clone(),
                wasm,
                e.manifest.capabilities.clone(),
                e.settings.clone(),
            ))
        })
    }

    /// Ejecuta un comando de un plugin APROBADO y ACTIVADO (fail-closed: un
    /// plugin no consentido JAMÁS se ejecuta). Delega la validación en
    /// [`Self::resolve_runnable`] y ejecuta a continuación. SÍNCRONO (compila e
    /// instancia el componente): el caller lo corre en `spawn_blocking` (regla
    /// 2). El daemon prefiere separar resolución (bajo lock) y ejecución (fuera
    /// del lock) llamando a [`Self::resolve_runnable`] directamente — su
    /// `handle_plugin_run_command` entrega `settings` de la MISMA forma, solo
    /// que en dos pasos en vez de una llamada a este método.
    ///
    /// Entrega al guest los valores de `[config]` YA resueltos que devuelve
    /// [`Self::resolve_runnable`] (P2 Task 2) vía `host-config` (P2 Task 3)
    /// ANTES de invocar el comando — un plugin sin `[config]` recibe el mapa
    /// vacío.
    ///
    /// # Errors
    /// [`PluginRunError`] si el plugin no existe, no está aprobado, está
    /// desactivado, no tiene binario, o el runtime falla.
    pub fn run_command(
        &self,
        runtime: &norte_plugin_host::PluginRuntime,
        id: &str,
        command: &str,
        arg: &str,
    ) -> Result<String, PluginRunError> {
        let (wasm, caps, settings) = self.resolve_runnable(id)?;
        let mut inst = runtime.instantiate(&wasm, caps)?;
        inst.set_settings(settings);
        Ok(inst.run_command(command, arg)?)
    }

    /// `true` si `id` corresponde a un plugin realmente descubierto.
    fn is_known(&self, id: &str) -> bool {
        self.catalog.plugins.iter().any(|e| e.manifest.id == id)
    }

    /// El ancla de aprobación de `id` tal como está AHORA en el catálogo, o
    /// `None` si el id no está descubierto (issue #69).
    ///
    /// Cubre el MANIFIESTO —capabilities, `category` y `contributions`, o sea
    /// qué pide y cuándo se dispara— **y el binario** (#241).
    ///
    /// Es lo que ancla una aprobación al darla, lo que el daemon compara para
    /// contestar «¿sigue siendo el que enseñaste?» (#282), y lo que
    /// `plugin.list` pone en `PluginInfo::manifest_digest`.
    #[must_use]
    pub fn manifest_digest(&self, id: &str) -> Option<String> {
        self.catalog
            .plugins
            .iter()
            .find(|e| e.manifest.id == id)
            .map(norte_plugin_host::PluginEntry::approval_anchor)
    }

    /// `true` si la aprobación es VIGENTE (issue #69): el humano aprobó Y el
    /// ancla guardada casa la de AHORA — el manifiesto (capabilities,
    /// `category`, `contributions`: qué pide y cuándo se dispara) **y el
    /// binario** (#241). Un `approved_digest` ausente (aprobación heredada sin
    /// ancla) NUNCA casa → re-consentimiento.
    fn approval_is_current(st: &PluginState, entry: &norte_plugin_host::PluginEntry) -> bool {
        st.approved && st.approved_digest.as_deref() == Some(entry.approval_anchor().as_str())
    }

    /// Resuelve `<dir>/plugin.wasm` y verifica, canonicalizando, que el binario
    /// real cae DENTRO del directorio del plugin (issue #69, defensa en
    /// profundidad contra un `plugin.wasm` que sea un symlink a `/etc/...` o a
    /// otro plugin). `None` si no existe, no es fichero o escapa del dir. Nota:
    /// quien puede escribir el symlink ya puede reemplazar el binario entero
    /// (misma frontera de confianza), por eso es defensa en profundidad, no una
    /// barrera fuerte. Devuelve la ruta CANÓNICA (ya resuelta) para no re-seguir
    /// enlaces al abrirla.
    /// La guarda vive en `norte-plugin-host` (ver
    /// [`norte_plugin_host::verified_child`], que documenta lo que NO cubre):
    /// el catálogo la necesita al descubrir y este crate al leer o ejecutar, y
    /// una segunda copia de un guard de seguridad es peor que la dependencia.
    fn verified_wasm(dir: &Path) -> Option<PathBuf> {
        norte_plugin_host::verified_child(dir, "plugin.wasm")
    }

    /// Lee el estado persistido. Ausente = vacío; corrupto = `InvalidData`.
    fn read_state(path: &Path) -> io::Result<BTreeMap<String, PluginState>> {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
        };
        let doc = src
            .parse::<DocumentMut>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut map = BTreeMap::new();
        if let Some(plugins) = doc.get("plugins").and_then(Item::as_table_like) {
            for (key, item) in plugins.iter() {
                let Some(tbl) = item.as_table_like() else {
                    continue;
                };
                let flag = |name: &str| tbl.get(name).and_then(Item::as_bool).unwrap_or(false);
                let digest = tbl.get("digest").and_then(Item::as_str).map(str::to_owned);
                map.insert(
                    key.to_string(),
                    PluginState {
                        approved: flag("approved"),
                        enabled: flag("enabled"),
                        approved_digest: digest,
                    },
                );
            }
        }
        Ok(map)
    }
}

/// Re-emite `config_dir/plugins-state.toml` preservando el resto del fichero,
/// con una entrada por cada plugin con estado. La clave con puntos se
/// entrecomilla.
///
/// Es una función LIBRE (no un método) para que el daemon pueda persistir en un
/// `spawn_blocking` a partir de un snapshot del estado, sin sostener el
/// `Mutex<PluginRegistry>` a través del `.await` (regla 2).
///
/// El write es ATÓMICO: se escribe a un temporal en el MISMO directorio y luego
/// `rename` sobre el destino. Un crash a mitad no corrompe el store durable de
/// una decisión de seguridad (consentimiento de capabilities).
///
/// # Errors
/// Errores de I/O al re-leer, escribir el temporal o renombrar; o
/// [`io::ErrorKind::InvalidData`] si el fichero existente es TOML corrupto.
pub(crate) fn persist_state(
    config_dir: &Path,
    state: &BTreeMap<String, PluginState>,
) -> io::Result<()> {
    let path = config_dir.join(PluginRegistry::STATE_FILE);
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .parse::<DocumentMut>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => DocumentMut::new(),
        Err(e) => return Err(e),
    };
    let root = doc.as_table_mut();
    if !root.get("plugins").is_some_and(Item::is_table_like) {
        root.insert("plugins", Item::Table(Table::new()));
    }
    // `insert` con una clave con puntos guarda la clave LITERAL; toml_edit la
    // entrecomilla al render (no la interpreta como tablas anidadas).
    let plugins = root["plugins"]
        .as_table_mut()
        .expect("plugins es una tabla: se acaba de garantizar arriba");
    for (id, st) in state {
        let mut inline = InlineTable::new();
        inline.insert("approved", Value::from(st.approved));
        inline.insert("enabled", Value::from(st.enabled));
        // El digest de capabilities anclado a la aprobación (issue #69) persiste
        // junto al flag; sin él una re-discover no podría revalidar el
        // consentimiento y forzaría re-aprobar en cada arranque.
        if let Some(digest) = &st.approved_digest {
            inline.insert("digest", Value::from(digest.clone()));
        }
        plugins.insert(id, Item::Value(Value::InlineTable(inline)));
    }
    // Write atómico: temporal en el mismo dir (mismo filesystem → rename atómico)
    // + rename sobre el destino. El sufijo con el pid evita pisar el temporal de
    // otro proceso que persista a la vez.
    let tmp = config_dir.join(format!(
        "{}.tmp.{}",
        PluginRegistry::STATE_FILE,
        std::process::id()
    ));
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// #282: el ancla que un humano LEE tiene que cambiar cuando cambia lo
    /// que se le enseñó, o `expected_digest` no protege de nada.
    ///
    /// Es la premisa del campo, no su uso: el uso está en el daemon
    /// (`handle_plugin_set_approval`) y en el `Backend` embebido, que es donde
    /// de verdad hay ventana — redescubre el catálogo en CADA llamada.
    #[test]
    fn el_ancla_de_un_manifiesto_cambia_cuando_cambia_lo_que_declara() {
        const ANTES: &str = r#"
[plugin]
id = "org.norte.anchor"
name = "Anchor"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;
        // Lo que cambia NO son las capabilities: es `contributions`, o sea
        // CUÁNDO y CÓMO se dispara. Es justo lo que la lista pintada no dice y
        // el ancla sí cubre — por eso la comparación de capabilities que hace
        // el cliente no basta.
        const DESPUES: &str = r#"
[plugin]
id = "org.norte.anchor"
name = "Anchor"
publisher = "norte"
version = "0.1.0"
category = "command"
[contributions]
command = [{ id = "run", title = "Run" }]
"#;
        let cfg = TempDir::new().expect("tempdir");
        let dir = cfg.path().join("plugins").join("org.norte.anchor");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("plugin.toml"), ANTES).expect("write");
        std::fs::write(dir.join("plugin.wasm"), b"\0asm\x01\0\0\0").expect("write wasm");

        let reg = PluginRegistry::discover(cfg.path()).expect("discover");
        let antes = reg
            .manifest_digest("org.norte.anchor")
            .expect("el catálogo trae el ancla sin necesidad de un wasm real");

        std::fs::write(dir.join("plugin.toml"), DESPUES).expect("rewrite");
        let reg2 = PluginRegistry::discover(cfg.path()).expect("rediscover");
        let despues = reg2
            .manifest_digest("org.norte.anchor")
            .expect("sigue descubierto");
        assert_ne!(
            antes, despues,
            "el ancla no se movió: `expected_digest` no protegería de un \
             manifiesto cambiado bajo los pies"
        );
    }

    /// #29/§6.2: `decode_for_preview` entrega TEXTO decodificado al previewer;
    /// UTF-8 válido no es lossy (#101).
    #[test]
    fn decode_for_preview_texto_utf8_pasa_igual() {
        assert_eq!(
            decode_for_preview(b"hola mundo".to_vec()),
            (b"hola mundo".to_vec(), false)
        );
    }

    #[test]
    fn decode_for_preview_utf16le_bom_se_decodifica_a_utf8() {
        // BOM UTF-16LE (FF FE) + "hi" → detect Text, decode a UTF-8 "hi".
        let utf16 = vec![0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        assert_eq!(decode_for_preview(utf16), (b"hi".to_vec(), false));
    }

    #[test]
    fn decode_for_preview_binario_pasa_los_bytes_crudos() {
        // Cabecera PNG (controles + NUL): detect Binary → bytes tal cual,
        // jamás lossy (#101).
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00".to_vec();
        assert_eq!(decode_for_preview(png.clone()), (png, false));
    }

    /// #101: bytes detectados como texto pero con una secuencia INVÁLIDA para
    /// ese encoding → decodificación LOSSY (`�`) marcada `lossy = true`. La
    /// aguja vive en el corpus canónico del testkit (`utf8_bom_invalid`), no
    /// inline, por la regla de CLAUDE.md sobre regresiones de encoding.
    #[test]
    fn decode_for_preview_texto_invalido_es_lossy() {
        let fx = norte_testkit::corpus::lossy_content_fixtures()
            .into_iter()
            .find(|f| f.id == "utf8_bom_invalid")
            .expect("corpus lossy trae utf8_bom_invalid");
        let (out, lossy) = decode_for_preview(fx.bytes.clone());
        assert!(lossy, "byte inválido debe marcar lossy");
        assert_eq!(
            String::from_utf8_lossy(&out),
            fx.decoded,
            "la salida es el decode canónico con `�`"
        );
    }

    /// G3a (ADR 0037): `to_wire_lines` re-forma el tipo del runtime al de
    /// wire 1:1, SIN validar `role` (esa validación vive en el frontend, ver
    /// su rustdoc) ni volver a acotar tamaños (ya acotados por
    /// `render_styled_preview`).
    #[test]
    fn to_wire_lines_reforma_1_a_1_sin_validar_role() {
        use norte_plugin_host::previewer_iface::Span;
        let lines = vec![
            vec![
                Span {
                    text: "42".to_owned(),
                    role: Some("number".to_owned()), // no es un Role válido: pasa igual
                    fg: None,
                },
                Span {
                    text: " TODO".to_owned(),
                    role: Some("keyword".to_owned()),
                    fg: Some((255, 200, 0)),
                },
            ],
            vec![Span {
                text: "plano".to_owned(),
                role: None,
                fg: None,
            }],
        ];
        let wire = to_wire_lines(lines);
        assert_eq!(wire.len(), 2, "2 líneas de entrada → 2 líneas de salida");
        assert_eq!(wire[0].len(), 2, "spans conservados 1:1");
        assert_eq!(wire[0][0].text, "42");
        assert_eq!(
            wire[0][0].role.as_deref(),
            Some("number"),
            "role viaja SIN VALIDAR (no es un Role válido y aun así pasa)"
        );
        assert_eq!(wire[0][0].fg, None);
        assert_eq!(wire[0][1].fg, Some([255, 200, 0]), "tupla → array [u8;3]");
        assert_eq!(wire[1][0].text, "plano");
        assert_eq!(wire[1][0].role, None);
    }

    /// Manifiesto válido mínimo (copiado del doctest de `norte-plugin-host`).
    const DEMO_MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
"#;

    /// Crea `config_dir/plugins/<id>/plugin.toml` con `src`.
    fn write_plugin(config_dir: &Path, id: &str, src: &str) {
        let dir = config_dir.join("plugins").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), src).unwrap();
    }

    /// Un provider plugin, con binario: `resolve_provider` exige el `.wasm`
    /// verificado como cualquier otro resolver.
    const PROVIDER_MANIFEST: &str = r#"
[plugin]
id = "org.norte.memplug"
name = "Mem plug"
publisher = "norte"
version = "0.1.0"
category = "provider"
[[contributions.provider]]
scheme = "memplug"
"#;

    fn write_provider(config_dir: &Path, id: &str, src: &str) {
        write_plugin(config_dir, id, src);
        std::fs::write(
            config_dir.join("plugins").join(id).join("plugin.wasm"),
            b"\0asm",
        )
        .unwrap();
    }

    /// Un `[[contributions.provider]]` se declaraba, se aprobaba y se
    /// activaba, y NADIE lo resolvía: `connect.rs` casaba schemes a mano. Esta
    /// es la mitad del registro: dado un scheme, el plugin consentido que lo
    /// declara — o nada.
    #[test]
    fn resolve_provider_elige_el_plugin_consentido_que_declara_el_scheme() {
        let tmp = TempDir::new().unwrap();
        write_provider(tmp.path(), "org.norte.memplug", PROVIDER_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();

        // Sin consentir: nada, aunque el scheme case (fail-closed).
        assert!(reg.resolve_provider("memplug").is_none());
        reg.set_approval("org.norte.memplug", true).unwrap();
        assert!(
            reg.resolve_provider("memplug").is_none(),
            "aprobado pero apagado"
        );
        reg.set_enabled("org.norte.memplug", true).unwrap();

        let r = reg
            .resolve_provider("memplug")
            .expect("consentido y activado");
        assert_eq!(r.id, "org.norte.memplug");
        assert_eq!(r.name, "Mem plug");
        assert!(r.wasm.ends_with("plugin.wasm"));
        assert_eq!(
            r.wasm_digest,
            norte_plugin_host::wasm_digest_of(b"\0asm"),
            "el digest que se devuelve es el del binario anclado"
        );
        assert_eq!(r.default_port, None);
        // Otro scheme no lo sirve: el plugin sirve lo que DECLARA.
        assert!(reg.resolve_provider("webdav").is_none());
        // Y lo que aprobar concede se ENSEÑA: el scheme va en las insignias.
        let info = reg.list().plugins.into_iter().next().unwrap();
        assert!(
            info.capabilities.iter().any(|c| c == "provider:memplug"),
            "{:?}",
            info.capabilities
        );

        // Sin binario no hay nada que instanciar, consentido o no.
        std::fs::remove_file(tmp.path().join("plugins/org.norte.memplug/plugin.wasm")).unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.resolve_provider("memplug").is_none());
    }

    /// La segunda puerta: aunque una entrada del catálogo declare un scheme
    /// del core (el manifiesto lo rechaza, así que hay que colarla a mano),
    /// el registro no lo sirve. Es lo que hace del guard del manager una
    /// optimización y no la única defensa.
    #[test]
    fn resolve_provider_nunca_sirve_un_scheme_del_core() {
        let tmp = TempDir::new().unwrap();
        write_provider(tmp.path(), "org.norte.memplug", PROVIDER_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        reg.set_approval("org.norte.memplug", true).unwrap();
        reg.set_enabled("org.norte.memplug", true).unwrap();
        // Se reclama `sftp` por detrás del parser.
        reg.catalog.plugins[0].manifest.contributions.provider[0].scheme = "sftp".to_owned();
        // El ancla cambia con las contribuciones, así que se re-aprueba en
        // memoria para que lo único que quede en pie sea la puerta.
        reg.set_approval_in_memory("org.norte.memplug", true);
        assert!(reg.resolve_provider("sftp").is_none());
    }

    /// Un plugin de otra categoría con una contribución `provider` colada no
    /// entra: `provider` tiene su propio world, y solo un binario que lo
    /// implementa debe instanciarse como tal (mismo criterio que
    /// `resolve_decorators`).
    #[test]
    fn resolve_provider_ignora_otras_categorias() {
        let tmp = TempDir::new().unwrap();
        write_provider(
            tmp.path(),
            "org.norte.sneaky",
            r#"
[plugin]
id = "org.norte.sneaky"
name = "Sneaky"
publisher = "norte"
version = "0.1.0"
category = "command"
[[contributions.provider]]
scheme = "memplug"
"#,
        );
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        reg.set_approval("org.norte.sneaky", true).unwrap();
        reg.set_enabled("org.norte.sneaky", true).unwrap();
        assert!(reg.resolve_provider("memplug").is_none());
    }

    #[test]
    fn plugins_discover_lista_un_plugin_sin_estado() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1);
        let p = &list.plugins[0];
        assert_eq!(p.id, "org.norte.demo");
        assert_eq!(p.category, "command");
        assert!(!p.approved);
        assert!(!p.enabled);
        assert!(p.capabilities.iter().any(|c| c == "fs-read"));
        assert!(list.errors.is_empty());
        // (P1) DEMO_MANIFEST no declara description ni comandos.
        assert_eq!(p.description, None, "sin description en el manifiesto");
        assert!(p.commands.is_empty(), "sin contributions.command");
    }

    /// (P1) manifiesto con `description` + un `contributions.command`: ambos
    /// deben llegar íntegros a `PluginInfo` por `list()`.
    #[test]
    fn plugins_discover_propaga_description_y_commands() {
        const WITH_DESC_AND_COMMANDS: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
description = "Saluda desde la paleta de comandos."
[contributions]
command = [
    { id = "greet", title = "Greet" },
    { id = "wave", title = "Wave" },
]
[capabilities]
fs-read = "scoped"
"#;
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", WITH_DESC_AND_COMMANDS);

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1);
        let p = &list.plugins[0];
        assert_eq!(
            p.description.as_deref(),
            Some("Saluda desde la paleta de comandos.")
        );
        assert_eq!(p.commands.len(), 2, "los dos comandos declarados");
        // Orden de manifiesto preservado (no reordenado).
        assert_eq!(p.commands[0].id, "greet");
        assert_eq!(p.commands[0].title, "Greet");
        assert_eq!(p.commands[1].id, "wave");
        assert_eq!(p.commands[1].title, "Wave");
    }

    /// `wasm_path` es un cálculo puro de ruta (single source of truth del
    /// layout `plugins/<id>/plugin.wasm`, review H2): no requiere que el
    /// binario exista.
    #[test]
    fn wasm_path_sigue_el_layout_plugins_id() {
        let tmp = TempDir::new().unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert_eq!(
            reg.wasm_path("org.norte.demo"),
            tmp.path()
                .join("plugins")
                .join("org.norte.demo")
                .join("plugin.wasm")
        );
    }

    /// Manifiesto con `[config]` (P2 Task 2), para `settings_of`.
    const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfg"
name = "Cfg"
publisher = "norte"
version = "0.1.0"
category = "command"
[config.retries]
type = "int"
default = 3
min = 0
max = 10
"#;

    #[test]
    fn settings_of_sin_config_toml_devuelve_los_defaults() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cfg", CONFIG_MANIFEST);

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let settings = reg
            .settings_of("org.norte.cfg")
            .unwrap_or_else(|| panic!("se esperaba un plugin descubierto"));
        assert_eq!(settings.get("retries").map(String::as_str), Some("3"));
    }

    #[test]
    fn settings_of_con_override_refleja_el_valor_de_config_toml() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cfg", CONFIG_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.cfg")
                .join("config.toml"),
            "retries = 8\n",
        )
        .unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let settings = reg.settings_of("org.norte.cfg").unwrap();
        assert_eq!(settings.get("retries").map(String::as_str), Some("8"));
    }

    #[test]
    fn settings_of_id_desconocido_es_none() {
        let tmp = TempDir::new().unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.settings_of("org.norte.fantasma").is_none());
    }

    #[test]
    fn plugins_set_estado_se_refleja_y_persiste() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval("org.norte.demo", true).unwrap());
        assert!(reg.set_enabled("org.norte.demo", true).unwrap());

        let p = &reg.list().plugins[0];
        assert!(p.approved);
        assert!(p.enabled);

        // Una NUEVA discover del mismo dir lo recuerda (persistió).
        let reg2 = PluginRegistry::discover(tmp.path()).unwrap();
        let p2 = &reg2.list().plugins[0];
        assert!(p2.approved, "approved debe persistir");
        assert!(p2.enabled, "enabled debe persistir");
        assert_eq!(
            p2.id, "org.norte.demo",
            "el id-con-puntos debe volver intacto"
        );
    }

    #[test]
    fn plugins_set_de_id_inexistente_no_persiste_basura() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(!reg.set_approval("org.norte.fantasma", true).unwrap());
        assert!(!reg.set_enabled("org.norte.fantasma", true).unwrap());

        // No se creó el fichero de estado (nada que persistir).
        assert!(
            !tmp.path().join("plugins-state.toml").exists(),
            "un id desconocido no debe crear plugins-state.toml"
        );
    }

    /// #241: cambiar el BINARIO invalida la aprobación, aunque el manifiesto
    /// no se toque.
    ///
    /// El ancla del issue #69 cubría el `plugin.toml` —qué pide y cuándo se
    /// dispara— y dejaba la otra puerta del bundle abierta: quien pudiera
    /// escribir el `.wasm` sin tocar el `.toml` se quedaba con las
    /// capacidades que un humano aprobó para OTRO código.
    #[test]
    fn cambiar_el_binario_invalida_la_aprobacion() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let wasm = tmp
            .path()
            .join("plugins")
            .join("org.norte.demo")
            .join("plugin.wasm");
        std::fs::write(&wasm, b"\0asm-uno").unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval("org.norte.demo", true).unwrap());
        assert!(
            reg.list().plugins[0].approved,
            "aprobado con este binario delante"
        );

        // El manifiesto NO se toca; solo el binario.
        std::fs::write(&wasm, b"\0asm-otro").unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            !reg.list().plugins[0].approved,
            "otro binario es otra pregunta: hay que volver a consentir"
        );

        // Y devolver el binario de antes devuelve la aprobación: el ancla es
        // el CONTENIDO, no un contador de cambios.
        std::fs::write(&wasm, b"\0asm-uno").unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.list().plugins[0].approved);
    }

    #[test]
    fn plugins_manifiesto_roto_aparece_en_errors_sin_tumbar_discover() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        write_plugin(tmp.path(), "roto", "esto no es toml [ valido =");

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1, "el válido sigue cargando");
        assert_eq!(list.errors.len(), 1, "el roto se reporta, no desaparece");
        assert!(list.errors[0].dir.contains("roto"));
    }

    #[test]
    fn plugins_state_id_con_puntos_round_trip() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        // Persistimos estado para un id con PUNTOS.
        {
            let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
            assert!(reg.set_approval("org.norte.demo", true).unwrap());
        }

        // El fichero debe llevar la clave ENTRECOMILLADA, no anidada.
        let raw = std::fs::read_to_string(tmp.path().join("plugins-state.toml")).unwrap();
        assert!(
            raw.contains("\"org.norte.demo\""),
            "la clave debe ir entrecomillada, no como [org.norte.demo]: {raw}"
        );

        // Y una discover fresca recupera el MISMO id con su estado.
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let st = reg.state.get("org.norte.demo").cloned().unwrap();
        assert!(st.approved && !st.enabled);
        // El ancla guardada al aprobar (issue #69, #241) también sobrevive al
        // round-trip y casa la de AHORA — que es manifiesto Y binario.
        let entrada = reg
            .catalog
            .plugins
            .iter()
            .find(|e| e.manifest.id == "org.norte.demo")
            .expect("descubierto");
        assert_eq!(
            st.approved_digest.as_deref(),
            Some(entrada.approval_anchor().as_str()),
            "el ancla debe persistir y casar el bundle"
        );
    }

    /// Manifiesto `command` mínimo, sin capabilities especiales, para los tests
    /// de ejecución fail-closed.
    const CMD_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cmd"
name = "Cmd"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

    #[test]
    fn plugins_run_command_sin_aprobar_es_not_approved() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotApproved(ref id) if id == "org.norte.cmd"),
            "un plugin sin aprobar JAMÁS se ejecuta: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_aprobado_sin_activar_es_disabled() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::Disabled(ref id) if id == "org.norte.cmd"),
            "aprobado pero desactivado no se ejecuta: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_activado_sin_wasm_es_no_binary() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(&err, PluginRunError::NoBinary(id) if id == "org.norte.cmd"),
            "sin plugin.wasm el runtime no arranca: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_id_inexistente_es_unknown() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.fantasma", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::Unknown(ref id) if id == "org.norte.fantasma"),
            "un id desconocido es Unknown: {err:?}"
        );
    }

    #[test]
    fn plugins_state_corrupto_es_invalid_data() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::write(
            tmp.path().join("plugins-state.toml"),
            "esto no es [ toml valido =",
        )
        .unwrap();

        let err = PluginRegistry::discover(tmp.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// Manifiesto de un previewer que declara `text/*`.
    const PREV_MANIFEST: &str = r#"
[plugin]
id = "org.norte.prev"
name = "Prev"
publisher = "norte"
version = "0.1.0"
category = "previewer"
[[contributions.previewer]]
mimetypes = ["text/*"]
"#;

    fn vpath(s: &str) -> norte_proto::VPath {
        norte_proto::VPath::parse(s).unwrap()
    }

    #[test]
    fn plugins_guess_mimetype_por_extension() {
        assert_eq!(guess_mimetype(&vpath("file:///a.txt")), "text/plain");
        assert_eq!(guess_mimetype(&vpath("file:///a.json")), "application/json");
        assert_eq!(guess_mimetype(&vpath("file:///README.md")), "text/markdown");
        assert_eq!(
            guess_mimetype(&vpath("file:///a.MARKDOWN")),
            "text/markdown"
        );
        assert_eq!(
            guess_mimetype(&vpath("file:///a")),
            "application/octet-stream"
        );
        assert_eq!(
            guess_mimetype(&vpath("file:///a.UNKNOWN")),
            "application/octet-stream"
        );
    }

    #[test]
    fn plugins_mimetype_matches_glob_y_exacto() {
        assert!(mimetype_matches("text/*", "text/plain"));
        assert!(!mimetype_matches("text/*", "application/json"));
        assert!(mimetype_matches("application/json", "application/json"));
        // No casa parcial: prefijo textual sin la barra no es glob.
        assert!(!mimetype_matches("application/json", "application/json5"));
        assert!(!mimetype_matches("text/plain", "text/plai"));
    }

    #[test]
    fn plugins_resolve_previewer_fail_closed_y_por_mimetype() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);
        // `plugin.wasm` VACÍO: `is_file()` no valida contenido, solo presencia.
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.prev")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();

        // Descubierto pero SIN aprobar/activar → fail-closed.
        assert!(
            reg.resolve_previewer("text/plain").is_none(),
            "un previewer no consentido jamás se elige"
        );

        // Aprobado + activado → resuelve para el mimetype que casa el glob.
        assert!(reg.set_approval_in_memory("org.norte.prev", true));
        assert!(reg.set_enabled_in_memory("org.norte.prev", true));

        let got = reg.resolve_previewer("text/plain");
        assert!(got.is_some(), "text/plain casa text/*");
        let (id, name, wasm, _caps, settings) = got.unwrap();
        assert_eq!(id, "org.norte.prev");
        assert_eq!(name, "Prev");
        assert!(wasm.ends_with("plugin.wasm"));
        assert!(
            settings.is_empty(),
            "PREV_MANIFEST no declara [config]: mapa vacío"
        );

        // Un mimetype que no casa el glob declarado → None.
        assert!(
            reg.resolve_previewer("application/json").is_none(),
            "application/json no casa text/*"
        );
    }

    /// D3: un previewer que declara el mimetype EXACTO gana a uno que declara
    /// el comodín, aunque el catálogo lo ordene después. Sin esto, quién
    /// pintaba un `.md` lo decidía el alfabeto de los ids: `org.norte.md`
    /// ganaba a `org.norte.syntect`, y `org.zzz.md` perdía.
    #[test]
    fn plugins_resolve_previewer_prefiere_exacto_sobre_glob() {
        let tmp = TempDir::new().unwrap();
        // El comodín va PRIMERO en orden de catálogo (id menor).
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);
        write_plugin(
            tmp.path(),
            "org.zzz.md",
            r#"
[plugin]
id = "org.zzz.md"
name = "MD"
publisher = "zzz"
version = "0.1.0"
category = "previewer"
[contributions]
previewer = [{ mimetypes = ["text/markdown"] }]
"#,
        );
        for id in ["org.norte.prev", "org.zzz.md"] {
            std::fs::write(tmp.path().join("plugins").join(id).join("plugin.wasm"), b"").unwrap();
        }
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        for id in ["org.norte.prev", "org.zzz.md"] {
            assert!(reg.set_approval_in_memory(id, true));
            assert!(reg.set_enabled_in_memory(id, true));
        }
        let (id, ..) = reg.resolve_previewer("text/markdown").unwrap();
        assert_eq!(id, "org.zzz.md", "exacto gana a text/* aunque vaya después");
        let (id, ..) = reg.resolve_previewer("text/plain").unwrap();
        assert_eq!(id, "org.norte.prev", "y el comodín sigue con el resto");
    }

    #[test]
    fn plugins_resolve_previewer_sin_wasm_es_none() {
        let tmp = TempDir::new().unwrap();
        // Sin escribir plugin.wasm: aunque esté consentido, no hay binario.
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.prev", true));
        assert!(reg.set_enabled_in_memory("org.norte.prev", true));

        assert!(
            reg.resolve_previewer("text/plain").is_none(),
            "sin plugin.wasm no hay nada que ejecutar"
        );
    }

    /// Sobrescribe el `plugin.toml` de `<config>/plugins/<id>/` con `src`.
    fn rewrite_manifest(config_dir: &Path, id: &str, src: &str) {
        std::fs::write(config_dir.join("plugins").join(id).join("plugin.toml"), src).unwrap();
    }

    /// Manifiesto `command` SIN capabilities peligrosas (para el test TOCTOU).
    const TOCTOU_BEFORE: &str = r#"
[plugin]
id = "org.norte.toctou"
name = "TOCTOU"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

    /// El MISMO plugin, pero con capabilities AMPLIADAS (fs-read + net) que el
    /// humano nunca aprobó.
    const TOCTOU_AFTER: &str = r#"
[plugin]
id = "org.norte.toctou"
name = "TOCTOU"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
net = { hosts = ["evil.example"] }
"#;

    #[test]
    fn plugins_capabilities_cambiadas_tras_aprobar_re_piden_consentimiento() {
        // Issue #69: el humano aprueba unas capabilities; luego el plugin.toml
        // cambia en disco a otras más amplias y ocurre un nuevo discover. La
        // aprobación (flag true en disco) NO debe valer para las capabilities
        // NUEVAS: el digest anclado ya no casa → NotApproved (re-consentimiento).
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.toctou", TOCTOU_BEFORE);
        {
            let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
            assert!(reg.set_approval("org.norte.toctou", true).unwrap());
            assert!(reg.set_enabled("org.norte.toctou", true).unwrap());
        }

        // El atacante reescribe el manifiesto con capabilities ampliadas.
        rewrite_manifest(tmp.path(), "org.norte.toctou", TOCTOU_AFTER);

        // Nueva discover: lee el estado (approved=true + digest VIEJO) y el
        // manifiesto NUEVO.
        let reg = PluginRegistry::discover(tmp.path()).unwrap();

        // list() muestra la aprobación como NO vigente (la UI re-pide consentir).
        let info = &reg.list().plugins[0];
        assert!(
            !info.approved,
            "capabilities cambiadas ⇒ aprobación efectiva=false"
        );
        assert!(
            info.capabilities.iter().any(|c| c == "net"),
            "y muestra las capabilities NUEVAS para que el humano las vea"
        );

        // Y resolve_runnable rechaza fail-closed con NotApproved.
        let err = reg.resolve_runnable("org.norte.toctou").unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotApproved(ref id) if id == "org.norte.toctou"),
            "el digest anclado ya no casa: {err:?}"
        );

        // Re-aprobar re-ancla el digest a las capabilities NUEVAS y vuelve a
        // resolver (el humano consintió lo que ahora hay).
        let mut reg = reg;
        assert!(reg.set_approval("org.norte.toctou", true).unwrap());
        assert!(reg.list().plugins[0].approved);
    }

    #[test]
    fn plugins_aprobacion_heredada_sin_digest_re_pide_consentimiento() {
        // Estado persistido de ANTES de la defensa (issue #69): approved=true sin
        // `digest`. Fail-closed: se trata como no vigente hasta re-aprobar.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        std::fs::write(
            tmp.path().join("plugins-state.toml"),
            "[plugins]\n\"org.norte.cmd\" = { approved = true, enabled = true }\n",
        )
        .unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            !reg.list().plugins[0].approved,
            "aprobación sin digest anclado no es vigente"
        );
        let err = reg.resolve_runnable("org.norte.cmd").unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotApproved(_)),
            "fail-closed sin digest: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn plugins_wasm_symlink_fuera_del_dir_es_no_binary() {
        // Issue #69 (defensa en profundidad): `plugin.wasm` es un symlink que
        // apunta FUERA del directorio del plugin. Se rechaza (NoBinary), no se
        // ejecuta un binario ajeno.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        // Un binario "de fuera" (contenido irrelevante: el symlink se rechaza
        // antes de intentar compilarlo).
        let outside = tmp.path().join("ajeno.wasm");
        std::fs::write(&outside, b"binario ajeno").unwrap();
        let link = tmp
            .path()
            .join("plugins")
            .join("org.norte.cmd")
            .join("plugin.wasm");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));

        let err = reg.resolve_runnable("org.norte.cmd").unwrap_err();
        assert!(
            matches!(err, PluginRunError::NoBinary(ref id) if id == "org.norte.cmd"),
            "un plugin.wasm que escapa del dir se rechaza: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn plugins_wasm_symlink_dentro_del_dir_se_acepta() {
        // Un symlink que resuelve DENTRO del dir del plugin es legítimo (p. ej.
        // un build que enlaza al artefacto real junto a él).
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let plugin_dir = tmp.path().join("plugins").join("org.norte.cmd");
        let real = plugin_dir.join("real.wasm");
        std::fs::write(&real, b"artefacto").unwrap();
        std::os::unix::fs::symlink(&real, plugin_dir.join("plugin.wasm")).unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));

        // resolve_runnable no debe fallar por NoBinary (llega a devolver la ruta).
        let resolved = reg.resolve_runnable("org.norte.cmd");
        assert!(
            resolved.is_ok(),
            "un symlink dentro del dir es válido: {resolved:?}"
        );
    }

    // -------------------------------------------------------------------
    // G3b (ADR 0037): `resolve_decorators`/`resolve_columns` + los helpers
    // de validación posicional.

    /// Manifiesto `decorator` mínimo.
    const DECOR_MANIFEST: &str = r#"
[plugin]
id = "org.norte.decor"
name = "Decor"
publisher = "norte"
version = "0.1.0"
category = "decorator"
[[contributions.decorator]]
"#;

    /// Un segundo decorator, para probar que `resolve_decorators` devuelve
    /// TODOS los consentidos (no el primero, a diferencia de
    /// `resolve_previewer`).
    const DECOR_MANIFEST_2: &str = r#"
[plugin]
id = "org.norte.decor2"
name = "Decor2"
publisher = "norte"
version = "0.1.0"
category = "decorator"
[[contributions.decorator]]
"#;

    /// Manifiesto `columns` que declara una columna `size-human`.
    const COLUMNS_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cols"
name = "Cols"
publisher = "norte"
version = "0.1.0"
category = "columns"
[[contributions.columns]]
id = "size-human"
header = "Size"
"#;

    #[test]
    fn resolve_decorators_fail_closed_sin_consentir() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.decor", DECOR_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.decor")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            reg.resolve_decorators().is_empty(),
            "un decorator no consentido jamás se resuelve"
        );
    }

    #[test]
    fn resolve_decorators_devuelve_todos_los_consentidos_no_solo_el_primero() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.decor", DECOR_MANIFEST);
        write_plugin(tmp.path(), "org.norte.decor2", DECOR_MANIFEST_2);
        for id in ["org.norte.decor", "org.norte.decor2"] {
            std::fs::write(tmp.path().join("plugins").join(id).join("plugin.wasm"), b"").unwrap();
        }
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.decor", true));
        assert!(reg.set_enabled_in_memory("org.norte.decor", true));
        assert!(reg.set_approval_in_memory("org.norte.decor2", true));
        assert!(reg.set_enabled_in_memory("org.norte.decor2", true));

        let resolved = reg.resolve_decorators();
        assert_eq!(
            resolved.len(),
            2,
            "AMBOS decorators consentidos: {resolved:?}"
        );
        let ids: Vec<&str> = resolved.iter().map(|(id, ..)| id.as_str()).collect();
        assert!(ids.contains(&"org.norte.decor"));
        assert!(ids.contains(&"org.norte.decor2"));
    }

    #[test]
    fn resolve_decorators_ignora_categoria_distinta_aunque_declare_contrib() {
        // Un plugin `command` no entra por `resolve_decorators` aunque, por
        // hipótesis, alguien copiara `[[contributions.decorator]]` en su
        // manifiesto: el world dedicado (`norte-decorator`) exige que la
        // categoría PRIMARIA sea `decorator` (a diferencia de
        // previewer/command, que comparten world).
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.cmd")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));
        assert!(reg.resolve_decorators().is_empty());
    }

    #[test]
    fn resolve_columns_fail_closed_y_por_id() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cols", COLUMNS_MANIFEST);
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.cols")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();

        assert!(
            reg.resolve_columns("size-human").is_none(),
            "sin consentir, ninguna columna se resuelve"
        );

        assert!(reg.set_approval_in_memory("org.norte.cols", true));
        assert!(reg.set_enabled_in_memory("org.norte.cols", true));

        let (id, name, wasm, _caps, _settings) = reg
            .resolve_columns("size-human")
            .expect("la columna declarada resuelve");
        assert_eq!(id, "org.norte.cols");
        assert_eq!(name, "Cols");
        assert!(wasm.ends_with("plugin.wasm"));

        assert!(
            reg.resolve_columns("no-declarada").is_none(),
            "un id de columna no declarado no resuelve"
        );
    }

    #[test]
    fn paths_to_basenames_extrae_el_nombre_no_la_ruta_completa() {
        let paths = vec![
            norte_proto::VPath::parse("file:///a/b/module.rs").unwrap(),
            norte_proto::VPath::parse("file:///a/README.md").unwrap(),
        ];
        let names = paths_to_basenames(&paths);
        assert_eq!(names, vec![b"module.rs".to_vec(), b"README.md".to_vec()]);
    }

    #[test]
    fn decorations_to_wire_checked_longitud_correcta_pasa() {
        use norte_plugin_host::decorator_iface::Decoration;
        let out = vec![
            Decoration {
                badge: Some("M".to_string()),
                role: Some("warning".to_string()),
            },
            Decoration {
                badge: None,
                role: None,
            },
        ];
        let wire = decorations_to_wire_checked(out, 2).expect("longitud casa: Some");
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0].badge.as_deref(), Some("M"));
        assert_eq!(wire[1].badge, None);
    }

    #[test]
    fn decorations_to_wire_checked_longitud_distinta_es_none_fail_closed() {
        use norte_plugin_host::decorator_iface::Decoration;
        let out = vec![Decoration {
            badge: Some("M".to_string()),
            role: None,
        }];
        assert!(
            decorations_to_wire_checked(out, 2).is_none(),
            "un guest que rompe el contrato posicional se descarta entero"
        );
    }

    /// El token muere con la llamada: el `Drop` de la sesión ES el mecanismo
    /// de expiración, y por eso no hay TTL que ajustar ni barrido que olvidar.
    #[test]
    fn el_token_muere_con_la_llamada() {
        let dir = tempfile::tempdir().unwrap();
        let vpath = crate::policy::local_root_vpath(dir.path()).expect("vpath del tempdir");
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let token = {
            let sesion = mint.mint_for(&vpath, None, false).expect("acuña");
            let token = sesion.token().to_owned();
            assert!(
                mint.resolve(&token).is_ok(),
                "vivo mientras dura la llamada"
            );
            assert_eq!(mint.live_tokens(), 1);
            token
        };
        assert!(
            mint.resolve(&token).is_err(),
            "un token de la página anterior está muerto"
        );
        assert_eq!(mint.live_tokens(), 0, "y no queda nada retenido");
    }

    /// Dos tokens distintos no se cruzan, y ninguno es adivinable.
    #[test]
    fn dos_ubicaciones_no_comparten_token() {
        use norte_plugin_host::LocationHost as _;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("solo-en-a"), b"x").unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let sa = mint
            .mint_for(
                &crate::policy::local_root_vpath(a.path()).unwrap(),
                None,
                false,
            )
            .expect("acuña a");
        let sb = mint
            .mint_for(
                &crate::policy::local_root_vpath(b.path()).unwrap(),
                None,
                false,
            )
            .expect("acuña b");
        assert_ne!(sa.token(), sb.token());
        assert_eq!(sa.token().len(), 64, "32 bytes en hex");
        assert!(mint.read(sa.token(), b"solo-en-a").is_ok());
        assert!(
            mint.read(sb.token(), b"solo-en-a").is_err(),
            "el token de B no alcanza el árbol de A"
        );
    }

    /// Sin ruta local no hay token: el guest lee de un descriptor de
    /// directorio, y un `sftp://` no tiene ninguno.
    #[test]
    fn una_ubicacion_que_no_es_file_no_acuna_token() {
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let remote = norte_proto::VPath::parse("sftp://host/dir").unwrap();
        assert!(
            mint.mint_for(&remote, None, false).is_none(),
            "sin ruta local no hay token"
        );
        let archivo = norte_proto::VPath::parse("zip+file:///a.zip/!/dentro").unwrap();
        assert!(
            mint.mint_for(&archivo, None, false).is_none(),
            "dentro de un archivo tampoco"
        );
    }

    /// ADR 0052: una raíz protegida no se rodea porque quien pregunta sea un
    /// plugin en vez de un agente.
    #[test]
    fn el_directorio_de_estado_sigue_sin_leerse_por_aqui() {
        let estado = tempfile::tempdir().unwrap();
        let raiz = crate::policy::local_root_vpath(estado.path()).unwrap();
        std::fs::create_dir(estado.path().join("dentro")).unwrap();
        let mint =
            LocationMint::with_protected(vec![raiz.clone()], norte_vfs_local::Bounds::default());
        assert!(
            mint.mint_for(&raiz, None, false).is_none(),
            "la raíz protegida, no"
        );
        let hijo = crate::policy::local_root_vpath(&estado.path().join("dentro")).unwrap();
        assert!(
            mint.mint_for(&hijo, None, false).is_none(),
            "ni nada bajo ella"
        );
    }

    /// El marcador de raíz de proyecto: con `.git` declarado, lo que se abre
    /// es el ANCESTRO que lo contiene, y el prefijo dice qué mira el usuario.
    /// Sin esto la columna solo funcionaría con el panel justo en la raíz del
    /// repositorio, porque un token NO puede subir.
    #[test]
    fn el_marcador_abre_el_ancestro_y_dice_el_prefijo() {
        use norte_plugin_host::LocationHost as _;
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/index"), b"DIRC").unwrap();
        std::fs::create_dir_all(repo.path().join("src/deep")).unwrap();
        let dir = crate::policy::local_root_vpath(&repo.path().join("src/deep")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let sesion = mint.mint_for(&dir, Some(".git"), true).expect("acuña");
        assert_eq!(sesion.as_ref().prefix, b"src/deep");
        assert_eq!(
            mint.read(sesion.token(), b".git/index").unwrap(),
            b"DIRC",
            "la raíz abierta es el repositorio, no el directorio visible"
        );
    }

    /// #241: un marcador que es un SYMLINK no cuenta, ni colgando.
    ///
    /// `ln -s /nada /tmp/.git` — y crear un nombre en `/tmp` puede cualquiera,
    /// el sticky bit solo impide borrar los ajenos — hacía que todo panel bajo
    /// `/tmp` le entregase al plugin el `/tmp` entero. Un `.git` legítimo es un
    /// directorio o el fichero `gitdir:` de un worktree; enlace, nunca.
    #[test]
    fn un_marcador_que_es_symlink_no_abre_el_ancestro() {
        let raiz = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/no-existe", raiz.path().join(".git")).unwrap();
        std::fs::create_dir(raiz.path().join("sub")).unwrap();
        let dir = crate::policy::local_root_vpath(&raiz.path().join("sub")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        // Se llama a la SUBIDA directamente, y no solo a `mint_for`, para que
        // un fallo nombre el ancestro culpable. Con el prefijo a secas, este
        // test decía «se subió» y no a dónde, que es la mitad del dato
        // (#308): dos días de intermitencia sin saber qué directorio tenía el
        // marcador.
        let (raiz_hallada, prefix, _) =
            mint.climb_to_marker(&dir, &raiz.path().join("sub"), ".git");
        assert!(
            prefix.is_empty(),
            "no se subió: la raíz es el directorio visible, no el ancestro. \
             Subió hasta {} (partiendo de {})",
            raiz_hallada.display(),
            raiz.path().join("sub").display()
        );
        let sesion = mint.mint_for(&dir, Some(".git"), true).expect("acuña");
        assert!(sesion.as_ref().prefix.is_empty());
    }

    /// Un marcador en un directorio QUE ESCRIBE CUALQUIERA no abre nada (#308).
    ///
    /// #241 cerró el caso del symlink y dejó abierto el que menos trabajo da:
    /// `mkdir /tmp/.git`. El sticky bit de `/tmp` impide BORRAR nombres
    /// ajenos, no impide CREAR el tuyo, y un `.git` que es un directorio de
    /// verdad pasaba la comprobación —está escrita para rechazar enlaces, y un
    /// directorio no es un enlace—. A partir de ahí, cualquier panel bajo
    /// `/tmp` le entregaba al plugin `/tmp` ENTERO: los ficheros temporales de
    /// todos los usuarios de la máquina.
    ///
    /// Se descubrió porque este test es intermitente en máquinas donde alguien
    /// ha dejado un `/tmp/.git`. No era un test frágil: era el test viendo el
    /// agujero cada vez que la condición existía.
    #[test]
    fn un_marcador_en_un_directorio_que_escribe_cualquiera_no_abre_nada() {
        use std::os::unix::fs::PermissionsExt as _;

        let raiz = tempfile::tempdir().unwrap();
        // Un `.git` de VERDAD, no un enlace: es lo que la comprobación anterior
        // aceptaba.
        std::fs::create_dir(raiz.path().join(".git")).unwrap();
        std::fs::create_dir(raiz.path().join("sub")).unwrap();
        // 1777, como `/tmp`: escribible por todos, con sticky bit.
        std::fs::set_permissions(raiz.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let (hallada, prefix, _) = mint.climb_to_marker(
            &crate::policy::local_root_vpath(&raiz.path().join("sub")).unwrap(),
            &raiz.path().join("sub"),
            ".git",
        );
        assert!(
            prefix.is_empty(),
            "un directorio que escribe cualquiera no es la raíz de un proyecto: \
             subió hasta {}",
            hallada.display()
        );
    }

    /// Y un repositorio NORMAL sigue abriéndose: el arreglo no puede costar el
    /// caso de uso entero.
    #[test]
    fn un_repositorio_con_permisos_normales_sigue_abriendo() {
        let raiz = tempfile::tempdir().unwrap();
        std::fs::create_dir(raiz.path().join(".git")).unwrap();
        std::fs::create_dir(raiz.path().join("sub")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let (_, prefix, _) = mint.climb_to_marker(
            &crate::policy::local_root_vpath(&raiz.path().join("sub")).unwrap(),
            &raiz.path().join("sub"),
            ".git",
        );
        assert_eq!(prefix, b"sub", "un repo de verdad sí abre su raíz");
    }

    /// Y el fichero `gitdir:` de un worktree SÍ cuenta: es un `.git` de verdad,
    /// y exigir un directorio habría roto los worktrees y los submódulos.
    #[test]
    fn un_marcador_que_es_fichero_si_abre_el_ancestro() {
        let raiz = tempfile::tempdir().unwrap();
        std::fs::write(raiz.path().join(".git"), b"gitdir: /otro/sitio").unwrap();
        std::fs::create_dir(raiz.path().join("sub")).unwrap();
        let dir = crate::policy::local_root_vpath(&raiz.path().join("sub")).unwrap();

        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let sesion = mint.mint_for(&dir, Some(".git"), true).expect("acuña");
        assert_eq!(sesion.as_ref().prefix, b"sub");
    }

    /// #241: la subida NO pasa de `$HOME`.
    ///
    /// `touch $HOME/.git` —un archivo mal extraído, un instalador descuidado,
    /// cualquier proceso del usuario— convertía cada directorio suyo que no
    /// fuera un repositorio en una raíz que abarcaba la casa entera:
    /// `MAX_CLIMB` es 64 y no había nada más. El marcador EN `$HOME` sigue
    /// valiendo; lo que no se hace es pasar de ahí.
    #[test]
    fn la_subida_se_para_en_home() {
        let casa = tempfile::tempdir().unwrap();
        // Un `.git` por ENCIMA de la casa: el caso que hay que no alcanzar.
        std::fs::write(casa.path().join(".git"), b"gitdir: /x").unwrap();
        let hijo = casa.path().join("proyectos/uno");
        std::fs::create_dir_all(&hijo).unwrap();
        let dir = crate::policy::local_root_vpath(&hijo).unwrap();

        // Con la casa EN el ancestro que lleva el marcador: se abre ese, que es
        // el caso legítimo — el techo es no pasar de la casa, no ignorar lo
        // que hay en ella.
        let mint = LocationMint::with_protected_and_home(
            Vec::new(),
            norte_vfs_local::Bounds::default(),
            Some(casa.path().to_path_buf()),
        );
        let sesion = mint.mint_for(&dir, Some(".git"), true).expect("acuña");
        assert_eq!(sesion.as_ref().prefix, b"proyectos/uno");

        // Y con la casa en el hijo, la subida se para ahí: el `.git` de encima
        // ya no cuenta.
        let mint = LocationMint::with_protected_and_home(
            Vec::new(),
            norte_vfs_local::Bounds::default(),
            Some(hijo.clone()),
        );
        let sesion = mint.mint_for(&dir, Some(".git"), true).expect("acuña");
        assert!(
            sesion.as_ref().prefix.is_empty(),
            "no se subió por encima de la casa"
        );
    }

    /// Sin `climb` no se sube: un agente acotado a su scope no gana un ancestro
    /// porque el plugin declare un marcador.
    #[test]
    fn sin_climb_la_raiz_es_el_directorio_visible() {
        use norte_plugin_host::LocationHost as _;
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::create_dir(repo.path().join("src")).unwrap();
        let dir = crate::policy::local_root_vpath(&repo.path().join("src")).unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let sesion = mint.mint_for(&dir, Some(".git"), false).expect("acuña");
        assert!(sesion.as_ref().prefix.is_empty());
        assert!(
            mint.read(sesion.token(), b".git/index").is_err(),
            "sin subir, el repositorio queda fuera"
        );
    }

    /// Un marcador que no aparece por encima no hace subir a ningún sitio: la
    /// raíz sigue siendo el directorio visible.
    #[test]
    fn un_marcador_ausente_no_sube_por_si_acaso() {
        let dir_t = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir_t.path().join("sub")).unwrap();
        let dir = crate::policy::local_root_vpath(&dir_t.path().join("sub")).unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        let sesion = mint
            .mint_for(&dir, Some(".no-existe"), true)
            .expect("acuña");
        assert!(sesion.as_ref().prefix.is_empty());
    }

    /// La subida se para en una raíz protegida: el directorio de estado no se
    /// convierte en la raíz de nadie (ADR 0052).
    #[test]
    fn la_subida_se_para_en_una_raiz_protegida() {
        let estado = tempfile::tempdir().unwrap();
        // El marcador está en el PADRE protegido; el directorio visible cuelga
        // de él.
        std::fs::create_dir(estado.path().join(".git")).unwrap();
        std::fs::create_dir(estado.path().join("dentro")).unwrap();
        let raiz = crate::policy::local_root_vpath(estado.path()).unwrap();
        let dir = crate::policy::local_root_vpath(&estado.path().join("dentro")).unwrap();
        let mint =
            LocationMint::with_protected(vec![raiz.clone()], norte_vfs_local::Bounds::default());
        assert!(
            mint.mint_for(&dir, Some(".git"), true).is_none(),
            "ni el directorio visible se sirve, porque ya está bajo la raíz protegida"
        );
    }

    /// Sin la capability aprobada no se acuña NADA: ni se abre el directorio.
    /// Es el mismo gate que hace cumplir el host, un paso antes.
    #[test]
    fn sin_capability_no_se_acuna_ni_se_abre_el_directorio() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"x").unwrap();
        let vpath = crate::policy::local_root_vpath(dir.path()).unwrap();
        let mint = LocationMint::with_protected(Vec::new(), norte_vfs_local::Bounds::default());
        // `run_column_values` solo llama a `mint_for` cuando la capability
        // está concedida; aquí se pinea la mitad observable: con capabilities
        // por defecto, `granted()` es falso.
        assert!(!norte_plugin_host::Capabilities::default()
            .location
            .granted());
        drop(mint.mint_for(&vpath, None, false));
        assert_eq!(mint.live_tokens(), 0);
    }

    #[test]
    fn column_values_checked_longitud_correcta_y_distinta() {
        assert_eq!(
            column_values_checked(vec![Some("1".into()), None], 2),
            Some(vec![Some("1".to_string()), None])
        );
        assert_eq!(column_values_checked(vec![Some("1".into())], 2), None);
    }

    // -------------------------------------------------------------------
    // H3e: `has_help` al descubrir + `help_of` bajo demanda.

    #[test]
    fn el_tope_del_wire_y_el_del_host_son_el_mismo_numero() {
        // `PLUGIN_HELP_MAX_BYTES` es NORMATIVO: el contrato invita a un
        // receptor a dimensionar contra él. El host recorta por
        // `Limits::untrusted()`. Son dos crates que no se conocen, así que sin
        // este ancla podrían separarse en silencio y el wire prometería un tope
        // que nadie aplica. `norte-core` depende de los dos: es el único sitio
        // donde la igualdad se puede afirmar.
        assert_eq!(
            norte_proto::methods::PLUGIN_HELP_MAX_BYTES,
            norte_help::Limits::untrusted().max_bytes
        );
    }

    #[test]
    fn help_of_devuelve_el_markdown_acotado_del_plugin() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::write(
            tmp.path().join("plugins/org.norte.demo/help.md"),
            "+++\nid = \"org.norte.demo\"\ntitle = \"Demo\"\n+++\ncuerpo",
        )
        .unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.list().plugins[0].has_help, "list lo anuncia");
        let help = reg.help_of("org.norte.demo").expect("hay página");
        assert!(help.markdown.contains("cuerpo"));
        assert!(!help.truncated && !help.lossy);
    }

    #[test]
    fn help_of_de_un_id_desconocido_es_none() {
        // Fail-closed: el id viene del WIRE. Se resuelve contra el catálogo y
        // jamás se compone en una ruta — un `../` no llega a tocar el FS.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.help_of("../../etc/passwd").is_none());
        assert!(reg.help_of("otro.plugin").is_none());
    }

    #[test]
    fn help_of_acota_un_help_md_enorme_y_lo_declara() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let gordo = "a".repeat(norte_help::Limits::untrusted().max_bytes + 4096);
        std::fs::write(tmp.path().join("plugins/org.norte.demo/help.md"), &gordo).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let help = reg.help_of("org.norte.demo").expect("hay página");
        assert!(help.truncated, "un fichero por encima del tope se declara");
        assert!(help.markdown.len() <= norte_help::Limits::untrusted().max_bytes);
        assert!(help.markdown.len() < gordo.len(), "y de verdad se cortó");
    }

    #[test]
    fn un_help_md_gigante_no_se_carga_entero_en_memoria() {
        // Un `help.md` DISPERSO de 100 GiB son unos pocos bytes en un tarball.
        // Leerlo entero para acotarlo DESPUÉS aborta por fallo de reserva, o
        // invita al OOM killer a llevarse el daemon con su journal y toda task
        // en vuelo. Y `plugin.help` está ABIERTO a un agente sobre un plugin
        // que no necesita ni aprobación ni activación: sería la primera lectura
        // SIN TOPE disparable por un agente en el daemon. El tope se aplica al
        // LEER, no al decodificar.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let f = std::fs::File::create(tmp.path().join("plugins/org.norte.demo/help.md")).unwrap();
        // Disperso: ni un byte escrito, así que el fixture cabe en cualquier CI.
        f.set_len(100 * 1024 * 1024 * 1024).unwrap();
        drop(f);

        let inicio = std::time::Instant::now();
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let help = reg.help_of("org.norte.demo").expect("hay página");
        assert!(
            inicio.elapsed() < std::time::Duration::from_secs(10),
            "la lectura acotada no depende del tamaño del fichero"
        );
        assert!(
            !help.markdown.is_empty(),
            "la página se sirve CORTADA, no se pierde: sin tope, la reserva de \
             100 GiB falla y el fichero se degrada a «no hay página» (y donde \
             la reserva sí entra, se la lleva el OOM killer)"
        );
        assert!(
            help.markdown.len() <= norte_help::Limits::untrusted().max_bytes,
            "lo que cruza el wire sigue acotado"
        );
        assert!(
            help.truncated,
            "y el recorte se declara: leer max_bytes+1 es lo que deja a \
             `cut_and_decode_untrusted` ver que sobraba"
        );
    }

    #[cfg(unix)]
    #[test]
    fn un_help_md_que_apunta_fuera_del_directorio_no_se_lee() {
        // El `help.md` cruza el wire y un AGENTE puede pedirlo: un symlink que
        // sale del directorio del plugin convertiría `plugin.help` en una
        // lectura de fichero arbitrario POR FUERA del motor de policy (misma
        // forma que el `plugin.wasm` del issue #69). Se lee como página vacía.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let fuera = tmp.path().join("ajeno.md");
        std::fs::write(&fuera, "secreto-de-otro-sitio").unwrap();
        let link = tmp.path().join("plugins/org.norte.demo/help.md");
        std::os::unix::fs::symlink(&fuera, &link).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(
            !reg.list().plugins[0].has_help,
            "la bandera del wire usa la MISMA guarda que el lector: anunciar \
             `true` y servir `\"\"` es el oráculo «esa ruta existe y es un \
             fichero regular», y las dos mitades las lee un agente"
        );
        assert!(
            reg.announces_help("org.norte.demo"),
            "y la bandera LAXA sigue diciendo que el autor puso el fichero: sin \
             ella, «lo puso y el host se niega a servirlo» sería indistinguible \
             de «no se documentó», y `norte doctor` no tendría qué reportar"
        );
        let help = reg.help_of("org.norte.demo").expect("el plugin existe");
        assert_eq!(help.markdown, "", "no hay página, y no es un error");
        assert!(
            !help.markdown.contains("secreto-de-otro-sitio"),
            "el contenido de fuera del dir JAMÁS cruza el wire"
        );
    }

    #[cfg(unix)]
    #[test]
    fn un_help_md_enlazado_dentro_del_directorio_si_se_lee() {
        // La guarda es "no escapar del dir", NO "nada de symlinks": un plugin
        // que organiza su propio directorio con enlaces no hace nada malo.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        let dir = tmp.path().join("plugins/org.norte.demo");
        std::fs::write(
            dir.join("README.md"),
            "+++\nid = \"org.norte.demo\"\ntitle = \"Demo\"\n+++\ncuerpo",
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.join("README.md"), dir.join("help.md")).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.list().plugins[0].has_help, "is_file sigue el enlace");
        let help = reg.help_of("org.norte.demo").expect("hay página");
        assert!(help.markdown.contains("cuerpo"));
    }

    #[test]
    fn un_help_md_ilegible_es_pagina_vacia_no_error() {
        // La ayuda es cosmética: un `help.md` que no se puede leer nunca
        // tumba el plugin ni la llamada.
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::create_dir_all(tmp.path().join("plugins/org.norte.demo/help.md")).unwrap();

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let help = reg.help_of("org.norte.demo").expect("el plugin existe");
        assert_eq!(help.markdown, "");
    }
}

/// Qué hizo [`install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Id del plugin instalado (leído del manifiesto, no del nombre del
    /// directorio de origen).
    pub id: String,
    /// Nombre legible declarado en el manifiesto.
    pub name: String,
    /// `true` si había ya un plugin con ese id y se reemplazó (solo con
    /// `force`). Cuando es `true`, su consentimiento se ha RETIRADO.
    pub replaced: bool,
}

/// Por qué no se pudo instalar.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// El origen no tiene `plugin.toml`, o no se puede leer.
    #[error("no hay `plugin.toml` legible en {0}")]
    NoManifest(PathBuf),
    /// El manifiesto no valida (id, capabilities, hooks…).
    #[error("`plugin.toml` inválido: {0}")]
    Manifest(#[from] norte_plugin_host::ManifestError),
    /// El origen no tiene `plugin.wasm`.
    #[error("no hay `plugin.wasm` en {0}")]
    NoWasm(PathBuf),
    /// El `plugin.wasm` supera el tope de artefacto del runtime: ni el
    /// catálogo lo leería ni el runtime lo instanciaría, así que no se copia.
    #[error("`plugin.wasm` mide {len} bytes y el tope es {cap}")]
    WasmTooLarge {
        /// Bytes del fichero.
        len: u64,
        /// El tope.
        cap: u64,
    },
    /// Ya hay un plugin instalado con ese id y no se pidió reemplazarlo.
    #[error(
        "`{0}` ya está instalado; reemplazarlo RETIRA su consentimiento — repite con `--force` si es lo que quieres"
    )]
    AlreadyInstalled(String),
    /// Error de I/O copiando.
    #[error("instalando: {0}")]
    Io(#[from] io::Error),
}

/// Instala el plugin de `src` (un directorio con `plugin.toml` + `plugin.wasm`)
/// bajo `config_dir/plugins/<id>/`.
///
/// El id sale del MANIFIESTO, nunca del nombre del directorio de origen: es lo
/// que el descubridor va a usar, y dejar que un directorio llamado de otra
/// forma decidiera dónde aterriza sería una vía para pisar a un tercero.
///
/// **Instalar no es consentir.** El plugin queda descubierto y sin aprobar; lo
/// aprueba y lo activa un humano en el gestor. Un instalador que consintiera
/// por su cuenta convertiría «traigo este fichero» en «le doy sus
/// capabilities», que es la decisión entera.
///
/// **Reemplazar RETIRA el consentimiento**, y esta es la parte que no es
/// cosmética: el digest de aprobación cubre el MANIFIESTO —capabilities,
/// categoría, contribuciones— y no el `.wasm`. Sin retirarlo, instalar encima
/// de un plugin ya aprobado dejaría un binario nuevo corriendo bajo el permiso
/// que un humano le dio a otro. Por eso reemplazar exige `force` y, cuando
/// ocurre, el estado del id se borra.
///
/// # Errors
/// [`InstallError`] si falta el manifiesto o el `.wasm`, si el manifiesto no
/// valida, si el id ya está instalado sin `force`, o por I/O.
pub fn install(config_dir: &Path, src: &Path, force: bool) -> Result<InstallReport, InstallError> {
    let manifest_path = src.join("plugin.toml");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|_| InstallError::NoManifest(manifest_path.clone()))?;
    let manifest = norte_plugin_host::Manifest::from_toml(&raw)?;

    let wasm_src = src.join("plugin.wasm");
    if !wasm_src.is_file() {
        return Err(InstallError::NoWasm(wasm_src));
    }
    // El tope del artefacto se aplica en la puerta: un binario que el
    // catálogo no va a leer (y el runtime no va a instanciar) no se copia a
    // la config para que cada descubrimiento lo liste como roto.
    let len = std::fs::metadata(&wasm_src)?.len();
    if len > norte_plugin_host::MAX_ARTIFACT_BYTES {
        return Err(InstallError::WasmTooLarge {
            len,
            cap: norte_plugin_host::MAX_ARTIFACT_BYTES,
        });
    }

    let dest = config_dir.join("plugins").join(&manifest.id);
    let replaced = dest.exists();
    if replaced && !force {
        return Err(InstallError::AlreadyInstalled(manifest.id.clone()));
    }

    std::fs::create_dir_all(&dest)?;
    std::fs::write(dest.join("plugin.toml"), &raw)?;
    std::fs::copy(&wasm_src, dest.join("plugin.wasm"))?;
    // La ayuda viaja con el plugin si la trae (H3e); su ausencia no es error.
    let help_src = src.join("help.md");
    if help_src.is_file() {
        std::fs::copy(&help_src, dest.join("help.md"))?;
    }

    if replaced {
        // Consentimiento retirado: el `.wasm` es otro y el digest del
        // manifiesto no lo habría notado.
        //
        // Se SOBRESCRIBE la entrada a "sin aprobar" en vez de borrarla del
        // mapa: `persist_state` fusiona sobre el documento existente, así que
        // quitar la clave del mapa la dejaría intacta en el fichero — el
        // plugin seguiría aprobado y nada lo diría. Escribir la entrada apagada
        // es además lo que un humano querría leer en `plugins-state.toml`:
        // "esto estuvo aprobado y ya no", no un hueco.
        let mut state = PluginRegistry::read_state(&config_dir.join(PluginRegistry::STATE_FILE))?;
        state.insert(manifest.id.clone(), PluginState::default());
        persist_state(config_dir, &state)?;
    }

    Ok(InstallReport {
        id: manifest.id,
        name: manifest.name,
        replaced,
    })
}

/// Los schemes del core, que ningún provider plugin sirve (ADR 0093). Se
/// re-exporta para quien no depende de `norte-plugin-host` (la CLI).
pub use norte_plugin_host::CORE_SCHEMES;
/// Los errores de carga TIPADOS del catálogo, para quien diagnostica sin
/// depender de `norte-plugin-host` (`norte doctor`, ADR 0094).
pub use norte_plugin_host::{LoadError, ManifestError};

/// Los schemes que declaran los provider plugins INSTALADOS bajo
/// `config_dir`, consentidos o no, ordenados y sin repetir.
///
/// Es para quien tiene que decidir si un argumento es una URL antes de que
/// nadie conecte (la CLI): enrutar `webdav://x` como URL no concede nada, y
/// la conexión sigue siendo fail-closed en [`PluginRegistry::resolve_provider`].
/// Un catálogo ilegible es una lista vacía: la CLI no puede hacer nada mejor
/// que tratar el argumento como fichero.
///
/// Lee SOLO los manifiestos: el catálogo entero hashea cada `plugin.wasm` y
/// resuelve cada `[config]`, y esto se pregunta para enrutar un argumento.
#[must_use]
pub fn installed_provider_schemes(config_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(config_dir.join("plugins")) else {
        return Vec::new();
    };
    let mut schemes: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|d| std::fs::read_to_string(d.path().join("plugin.toml")).ok())
        .filter_map(|src| norte_plugin_host::Manifest::from_toml(&src).ok())
        .filter(|m| m.category == norte_plugin_host::Category::Provider)
        .flat_map(|m| m.contributions.provider.into_iter().map(|c| c.scheme))
        .collect();
    schemes.sort();
    schemes.dedup();
    schemes
}

/// Qué hizo [`uninstall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallReport {
    /// Id desinstalado.
    pub id: String,
    /// `true` si el plugin tenía consentimiento (aprobado en el estado): el
    /// informe lo dice porque es lo que acaba de dejar de existir.
    pub was_approved: bool,
}

/// Por qué no se pudo desinstalar.
#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    /// El id no es un id de plugin (reverse-DNS). Se rechaza ANTES de tocar el
    /// disco: el id se convierte en una ruta bajo `plugins/`, y un `..` sería
    /// un borrado fuera de ella.
    #[error("id de plugin inválido: se espera reverse-DNS (p. ej. `org.foo.bar`)")]
    InvalidId,
    /// No hay ningún plugin instalado con ese id.
    #[error("`{0}` no está instalado")]
    NotInstalled(String),
    /// Error de I/O borrando o escribiendo el estado.
    #[error("desinstalando: {0}")]
    Io(#[from] io::Error),
}

/// Desinstala el plugin `id`: borra `config_dir/plugins/<id>/` y deja su
/// entrada de `plugins-state.toml` APAGADA.
///
/// Apagada y no borrada, por la misma razón que [`install`] con `force`:
/// `persist_state` fusiona sobre el documento existente, así que quitar la
/// clave del mapa la dejaría intacta en el fichero — y un plugin con el mismo
/// id que se instalase después heredaría un consentimiento que nadie le dio.
///
/// El estado se LEE antes de borrar nada: si `plugins-state.toml` está
/// corrupto, se falla con el directorio intacto. Al revés, un borrado seguido
/// de una lectura fallida dejaría la aprobación viva en el fichero y un
/// segundo `uninstall` contestando «no está instalado» para siempre. El
/// borrado va antes de ESCRIBIR el estado por la razón contraria: si falla a
/// medias, lo que queda es un plugin roto que el descubridor lista en
/// `errors`, no un plugin entero con el consentimiento retirado en silencio.
///
/// Un daemon en marcha sigue con su registro en memoria hasta que vuelve a
/// descubrir; conectar por scheme redescubre siempre, ejecutar un comando
/// falla por falta de binario.
///
/// # Errors
/// [`UninstallError`] si el id no es un id, si no está instalado, o por I/O.
pub fn uninstall(config_dir: &Path, id: &str) -> Result<UninstallReport, UninstallError> {
    if !norte_plugin_host::is_valid_plugin_id(id) {
        return Err(UninstallError::InvalidId);
    }
    let dir = config_dir.join("plugins").join(id);
    if !dir.is_dir() {
        return Err(UninstallError::NotInstalled(id.to_owned()));
    }
    let state_path = config_dir.join(PluginRegistry::STATE_FILE);
    let mut state = PluginRegistry::read_state(&state_path)?;
    let was_approved = state.get(id).is_some_and(|st| st.approved);

    std::fs::remove_dir_all(&dir)?;

    state.insert(id.to_owned(), PluginState::default());
    persist_state(config_dir, &state)?;

    Ok(UninstallReport {
        id: id.to_owned(),
        was_approved,
    })
}

// ---------- ubicación para plugins de columnas (ADR 0057) ----------

/// Acuña los tokens OPACOS con los que un guest de columnas lee bajo el
/// directorio que se está listando, y los resuelve mientras la llamada vive.
///
/// El guest jamás recibe la ruta. Recibe una cadena aleatoria que solo
/// significa algo dentro de este proceso y solo mientras dura la llamada que
/// la acuñó: cuando la [`LocationSession`] se suelta, el token deja de
/// resolver. Un token de la página anterior está muerto, y un guest que lo
/// guarde no gana nada con él.
#[derive(Debug)]
pub(crate) struct LocationMint {
    bounds: norte_vfs_local::Bounds,
    /// Raíces que NO se abren aunque las pida un plugin (ADR 0052: el
    /// directorio de estado del daemon no se rodea porque el que pregunta sea
    /// un plugin en vez de un agente).
    protected: Vec<norte_proto::VPath>,
    /// El techo de la subida al marcador de raíz (#241): por encima de la casa
    /// no hay proyectos, hay sistema. `None` = sin `$HOME`, y entonces manda
    /// `MAX_CLIMB` sola.
    home: Option<std::path::PathBuf>,
    live: std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<norte_vfs_local::ConfinedRoot>>,
    >,
}

/// El HOME del usuario, si el entorno lo dice. Techo de la subida (#241).
fn home_del_entorno() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
}

/// Si `p` lo puede escribir CUALQUIER usuario de la máquina.
///
/// Un marcador de raíz de proyecto dentro de un directorio así no significa
/// nada: lo pone quien quiera. #241 cerró el caso del enlace —`ln -s /nada
/// /tmp/.git`— y dejó abierto el que menos trabajo cuesta, `mkdir /tmp/.git`,
/// porque la comprobación estaba escrita para rechazar ENLACES y un
/// directorio de verdad no lo es. El sticky bit de `/tmp` impide borrar
/// nombres ajenos; no impide crear el tuyo. Con el marcador plantado, todo
/// panel bajo `/tmp` le entregaba al plugin `/tmp` entero: los temporales de
/// todos los usuarios.
///
/// Se mira el bit `o+w` del DIRECTORIO que tiene el marcador, no el del
/// marcador: lo que decide quién puede plantarlo es el permiso del contenedor.
/// Un repositorio normal es 0755 y no se ve afectado.
///
/// Un `stat` que falla dice `true` —fail-closed—: si no se puede saber quién
/// escribe ahí, no se entrega esa raíz.
#[cfg(unix)]
fn lo_escribe_cualquiera(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    // Escrito en negativo a propósito: es «true salvo que se DEMUESTRE que no».
    // Un `is_ok_and(… != 0)` diría `false` cuando el stat falla, o sea abriría
    // la raíz precisamente cuando no se sabe de quién es.
    !std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o002 == 0)
}

/// En sistemas sin bits POSIX esta comprobación no aplica: Windows tiene su
/// propia historia de ACL y el confinamiento allí lo lleva #217.
#[cfg(not(unix))]
fn lo_escribe_cualquiera(_p: &std::path::Path) -> bool {
    false
}

/// `(dev, ino)` de una ruta, o `None` si no se pudo mirar.
///
/// `None` no relaja nada por su cuenta: quien lo recibe abre sin verificar,
/// que es lo que se hacía antes de #241 — y una ruta que no se puede `stat`ear
/// tampoco se va a poder abrir dos líneas después.
fn node_id_de(p: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

impl LocationMint {
    /// Un acuñador con las raíces protegidas de este proceso.
    pub(crate) fn new(bounds: norte_vfs_local::Bounds) -> std::sync::Arc<Self> {
        Self::with_protected(crate::policy::protected_roots(), bounds)
    }

    /// Como [`Self::new`], diciendo qué raíces están protegidas (tests).
    pub(crate) fn with_protected(
        protected: Vec<norte_proto::VPath>,
        bounds: norte_vfs_local::Bounds,
    ) -> std::sync::Arc<Self> {
        Self::with_protected_and_home(protected, bounds, home_del_entorno())
    }

    /// Como [`Self::with_protected`] diciendo también dónde está la casa
    /// (tests): `$HOME` es el techo de la subida (#241) y un test no puede
    /// tocarlo —`std::env::set_var` es `unsafe` en la edición 2024 y la regla
    /// 5 lo prohíbe fuera de `norte-vfs-local`—, así que el techo se INYECTA.
    /// Leerlo una vez al construir, y no en cada subida, es además lo correcto:
    /// la casa no cambia a media vida del proceso.
    pub(crate) fn with_protected_and_home(
        protected: Vec<norte_proto::VPath>,
        bounds: norte_vfs_local::Bounds,
        home: Option<std::path::PathBuf>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            bounds,
            protected,
            home,
            live: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Tope de niveles que sube la búsqueda del marcador de raíz. Un proyecto
    /// anidado 64 directorios por debajo de su raíz no es un proyecto.
    const MAX_CLIMB: usize = 64;

    /// Acuña un token para `dir`, o `None` si esa ubicación no se sirve: no es
    /// un `file://` local, cae bajo una raíz protegida, o no se pudo abrir.
    /// `None` NO es un error de la petición — la columna se queda vacía y el
    /// panel sigue.
    ///
    /// `marker` es el marcador de raíz de proyecto que declara el manifiesto
    /// (`.git`): si viene, lo que se abre es el ANCESTRO más cercano que lo
    /// contenga, y el `prefix` de la sesión dice qué parte de esa raíz está
    /// mirando el usuario. Sin marcador —o si no aparece por encima— la raíz
    /// es `dir` y el prefijo va vacío.
    ///
    /// `climb` lo decide el llamante: solo se sube para el actor HUMANO. Un
    /// agente o un plugin están acotados a su scope, y subir por encima de él
    /// sería justo lo que el gate de lectura impide.
    ///
    /// BLOQUEANTE (abre directorios): va dentro de `spawn_blocking`.
    pub(crate) fn mint_for(
        self: &std::sync::Arc<Self>,
        dir: &norte_proto::VPath,
        marker: Option<&str>,
        climb: bool,
    ) -> Option<LocationSession> {
        if dir.scheme() != "file" || dir.authority().is_some() {
            return None;
        }
        if self.is_protected(dir) {
            tracing::debug!("columns: ubicación bajo una raíz protegida, sin token");
            return None;
        }
        let native = norte_vfs_local::vpath_to_native(dir).ok()?;
        let (root_native, prefix, esperado) = if let Some(marker) = marker.filter(|_| climb) {
            self.climb_to_marker(dir, &native, marker)
        } else {
            let id = node_id_de(&native);
            (native, Vec::new(), id)
        };
        // Las raíces protegidas viajan a la confinación (#238): que la raíz no
        // ESTÉ bajo una de ellas —lo que comprueba `is_protected` arriba— no
        // dice nada sobre si CONTIENE alguna, y contenerla es el caso normal
        // (`$XDG_CONFIG_HOME` contiene `norte/`). Sin esto, un panel abierto en
        // el directorio de configuración le servía al plugin el journal, los
        // secretos y el fichero de conexiones.
        let vetadas: Vec<std::path::PathBuf> = self
            .protected
            .iter()
            .filter_map(|p| norte_vfs_local::vpath_to_native(p).ok())
            .collect();
        // `open_verified` y no `open`: lo que se abre tiene que ser el nodo que
        // esta función miró para decidir que era la raíz (#241).
        let root = norte_vfs_local::ConfinedRoot::open_verified(
            &root_native,
            self.bounds,
            &vetadas,
            esperado,
        )
        .ok()?;
        let token = mint_token();
        self.live
            .lock()
            .expect("live lock sano")
            .insert(token.clone(), std::sync::Arc::new(root));
        Some(LocationSession {
            mint: std::sync::Arc::clone(self),
            token,
            prefix,
        })
    }

    fn is_protected(&self, path: &norte_proto::VPath) -> bool {
        self.protected
            .iter()
            .any(|root| crate::policy::is_under(root, path))
    }

    /// El ancestro más cercano que contiene una entrada llamada `marker`, y el
    /// camino desde él hasta `dir` en bytes. Si no hay ninguno, `dir` mismo con
    /// prefijo vacío — nunca se sube «por si acaso».
    ///
    /// La subida se corta **en `$HOME`** (#241) y a los [`Self::MAX_CLIMB`]
    /// niveles.
    ///
    /// El techo de `$HOME` es lo que impide que un `touch $HOME/.git` —un
    /// archivo mal extraído, un instalador descuidado, cualquier proceso del
    /// usuario— convierta cada directorio suyo que no sea un repositorio en
    /// una raíz que abarca la casa entera. El marcador EN `$HOME` sí vale: el
    /// techo es no pasar de ahí, no ignorar lo que hay ahí. Por encima de
    /// `$HOME` no hay proyectos, hay sistema.
    ///
    /// Antes había también un corte en la primera raíz protegida. Era código
    /// muerto y decía hacer algo: `is_protected(p)` significa «p está bajo una
    /// raíz protegida», y si lo está un ANCESTRO lo está también `dir`, con lo
    /// que [`Self::mint_for`] ya devolvió `None` antes de llegar aquí. Lo que
    /// de verdad hace falta —no entregar una raíz que CONTIENE una protegida—
    /// lo hace la confinación con sus `vetadas` (#238).
    fn climb_to_marker(
        &self,
        dir: &norte_proto::VPath,
        native: &std::path::Path,
        marker: &str,
    ) -> (std::path::PathBuf, Vec<u8>, Option<(u64, u64)>) {
        use std::os::unix::ffi::OsStrExt as _;
        let casa = self.home.as_deref();
        let mut prefix: Vec<Vec<u8>> = Vec::new();
        let mut actual_v = dir.clone();
        let mut actual_n = native.to_path_buf();
        for _ in 0..=Self::MAX_CLIMB {
            // El marcador NO puede ser un symlink (#241). Con
            // `symlink_metadata().is_ok()` a secas valía hasta uno colgando:
            // `ln -s /nada /tmp/.git` —y `/tmp` lo escribe cualquiera, que el
            // sticky bit impida BORRAR nombres ajenos no impide CREAR uno—
            // hacía que cualquier panel bajo `/tmp` le entregara al plugin
            // todo `/tmp`. Un `.git` de verdad es un directorio, o el fichero
            // `gitdir:` de un worktree; ninguno de los dos es un enlace, y
            // aceptar enlaces solo compra el ataque.
            if actual_n
                .join(marker)
                .symlink_metadata()
                .is_ok_and(|m| !m.file_type().is_symlink())
                && !lo_escribe_cualquiera(&actual_n)
            {
                // El nodo que se MIRÓ, para exigirlo al abrir: entre esta
                // decisión y el `open` la ruta se resuelve otra vez desde `/`,
                // siguiendo enlaces, y renombrar un componente por medio
                // cambiaba la raíz por la que quisiera quien pudo renombrarlo
                // (#241).
                let id = node_id_de(&actual_n);
                return (actual_n, prefix.join(&b'/'), id);
            }
            // El techo: se mira el marcador EN `$HOME` (arriba) y de ahí no se
            // pasa. Sin esto, `MAX_CLIMB` era el único límite y un `.git`
            // suelto en la casa se llevaba la casa entera (#241).
            if casa == Some(actual_n.as_path()) {
                break;
            }
            let Some(padre_v) = actual_v.parent() else {
                break;
            };
            let Some(padre_n) = actual_n.parent().map(std::path::Path::to_path_buf) else {
                break;
            };
            let nombre = actual_n
                .file_name()
                .map(|n| n.as_bytes().to_vec())
                .unwrap_or_default();
            prefix.insert(0, nombre);
            actual_v = padre_v;
            actual_n = padre_n;
        }
        (native.to_path_buf(), Vec::new(), node_id_de(native))
    }

    fn resolve(
        &self,
        token: &str,
    ) -> Result<std::sync::Arc<norte_vfs_local::ConfinedRoot>, String> {
        self.live
            .lock()
            .expect("live lock sano")
            .get(token)
            .cloned()
            .ok_or_else(|| "token desconocido".to_owned())
    }

    fn retire(&self, token: &str) {
        self.live.lock().expect("live lock sano").remove(token);
    }

    /// Cuántos tokens siguen vivos. Un número que no sea 0 entre páginas es un
    /// escape de sesión, así que los tests lo miran.
    #[cfg(test)]
    pub(crate) fn live_tokens(&self) -> usize {
        self.live.lock().expect("live lock sano").len()
    }
}

/// El token de UNA llamada. Al soltarse, el token deja de resolver: ese es
/// todo el mecanismo de expiración, y por eso no hay TTL que ajustar.
#[derive(Debug)]
pub(crate) struct LocationSession {
    pub(crate) mint: std::sync::Arc<LocationMint>,
    token: String,
    /// Qué parte de la raíz está mirando el usuario, en bytes y sin barra
    /// final. Vacío = la raíz ES el directorio visible.
    prefix: Vec<u8>,
}

impl LocationSession {
    /// El token que se le pasa al guest. Solo lo miran los tests: el camino
    /// real usa [`Self::as_ref`], que lleva token Y prefijo juntos.
    #[cfg(test)]
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// El par (token, prefijo) tal y como cruza al guest.
    pub(crate) fn as_ref(&self) -> norte_plugin_host::columns_iface::LocationRef {
        norte_plugin_host::columns_iface::LocationRef {
            token: self.token.clone(),
            prefix: self.prefix.clone(),
        }
    }
}

impl Drop for LocationSession {
    fn drop(&mut self) {
        self.mint.retire(&self.token);
    }
}

/// 32 bytes aleatorios en hex: ni adivinable ni derivable de la ruta.
fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    // Del CSPRNG del sistema: un token derivable de la ruta o de un contador
    // sería adivinable desde OTRO plugin del mismo proceso.
    getrandom::fill(&mut bytes).expect("el CSPRNG del sistema no falla");
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

impl norte_plugin_host::LocationHost for LocationMint {
    fn read(&self, token: &str, rel: &[u8]) -> Result<Vec<u8>, String> {
        self.resolve(token)?.read(rel).map_err(|e| e.to_string())
    }

    fn read_prefix(&self, token: &str, rel: &[u8], max: u64) -> Result<Vec<u8>, String> {
        self.resolve(token)?
            .read_prefix(rel, max)
            .map_err(|e| e.to_string())
    }

    fn stat(
        &self,
        token: &str,
        rel: &[u8],
    ) -> Result<norte_plugin_host::location_iface::Meta, String> {
        let meta = self.resolve(token)?.stat(rel).map_err(|e| e.to_string())?;
        Ok(meta_to_wire(&meta))
    }

    fn list_dir(
        &self,
        token: &str,
        rel: &[u8],
    ) -> Result<Vec<norte_plugin_host::location_iface::Dirent>, String> {
        let entries = self.resolve(token)?.list(rel).map_err(|e| e.to_string())?;
        Ok(entries
            .into_iter()
            .map(|e| norte_plugin_host::location_iface::Dirent {
                name: e.name,
                kind: kind_to_wire(e.kind),
            })
            .collect())
    }
}

fn kind_to_wire(
    kind: norte_vfs_local::LocationKind,
) -> norte_plugin_host::location_iface::EntryKind {
    use norte_plugin_host::location_iface::EntryKind as Wire;
    match kind {
        norte_vfs_local::LocationKind::File => Wire::File,
        norte_vfs_local::LocationKind::Dir => Wire::Dir,
        norte_vfs_local::LocationKind::Symlink => Wire::Symlink,
        norte_vfs_local::LocationKind::Other => Wire::Other,
    }
}

fn meta_to_wire(meta: &norte_vfs_local::LocationMeta) -> norte_plugin_host::location_iface::Meta {
    norte_plugin_host::location_iface::Meta {
        kind: kind_to_wire(meta.kind),
        size: meta.size,
        mtime_sec: meta.mtime_sec,
        mtime_nsec: meta.mtime_nsec,
        ctime_sec: meta.ctime_sec,
        ctime_nsec: meta.ctime_nsec,
        ino: meta.ino,
        dev: meta.dev,
        mode: meta.mode,
    }
}

/// Corre `column-values` de UN plugin ya resuelto, con ubicación si se le
/// aprobó. El ÚNICO sitio donde eso se hace: el daemon y el backend embebido
/// llaman aquí, porque una capacidad exigida en un camino y no en el otro es
/// el fallo que este repositorio ya se ha escrito tres veces (#165, #201,
/// #181).
///
/// Fail-closed en todos sus bordes — no instancia, trapea, rompe el contrato
/// posicional, o la ubicación no se puede abrir: la página sale con celdas
/// vacías, jamás un error que tumbe el listado.
///
/// BLOQUEANTE (instancia WASM y abre un directorio): va en `spawn_blocking`.
/// [`run_column_values`] para los e2e: mismo camino exacto que el daemon y el
/// backend embebido, expuesto porque el test que importa —el plugin oficial
/// instalado como el de un tercero— vive fuera de este crate. Reexportar la
/// función es preferible a que el test monte su propia versión del camino,
/// que es como dos caminos se separan.
/// **Solo con la feature `testing`** (#241): en la biblioteca publicada esto
/// era un camino de acuñado SIN política —toma `location_dir` y `climb` tal
/// cual, y el `climb` solo es del actor humano—, disponible para cualquiera
/// que dependa de este crate.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
#[must_use]
pub fn run_column_values_for_test(
    runtime: &norte_plugin_host::PluginRuntime,
    resolved: ResolvedDecorator,
    column_id: &str,
    location_dir: Option<&norte_proto::VPath>,
    climb: bool,
    entries: &[Vec<u8>],
    expected_len: usize,
) -> Vec<Option<String>> {
    run_column_values(
        runtime,
        resolved,
        column_id,
        location_dir,
        climb,
        entries,
        expected_len,
    )
}

pub(crate) fn run_column_values(
    runtime: &norte_plugin_host::PluginRuntime,
    resolved: ResolvedDecorator,
    column_id: &str,
    location_dir: Option<&norte_proto::VPath>,
    climb: bool,
    entries: &[Vec<u8>],
    expected_len: usize,
) -> Vec<Option<String>> {
    let (id, _name, wasm, caps, settings) = resolved;
    // La sesión vive hasta el final de esta función y ni un instante más: al
    // soltarse, el token deja de resolver.
    let sesion = if caps.location.granted() {
        let mint = LocationMint::new(norte_vfs_local::Bounds::default());
        location_dir.and_then(|dir| mint.mint_for(dir, caps.location_root_marker.as_deref(), climb))
    } else {
        None
    };
    let host: Option<std::sync::Arc<dyn norte_plugin_host::LocationHost>> =
        sesion.as_ref().map(|s| {
            std::sync::Arc::clone(&s.mint) as std::sync::Arc<dyn norte_plugin_host::LocationHost>
        });
    let Ok(mut inst) = runtime.instantiate_columns_with_location(&wasm, caps, host) else {
        tracing::warn!(plugin = %id, "columns: fallo al instanciar, celdas vacías");
        return vec![None; expected_len];
    };
    inst.set_settings(settings);
    let refe = sesion.as_ref().map(LocationSession::as_ref);
    let Ok(raw) = inst.column_values(column_id, refe.as_ref(), entries) else {
        tracing::warn!(plugin = %id, "columns: fallo al ejecutar, celdas vacías");
        return vec![None; expected_len];
    };
    column_values_checked(raw, expected_len).unwrap_or_else(|| {
        tracing::warn!(
            plugin = %id,
            "columns: longitud no casa el contrato posicional, celdas vacías"
        );
        vec![None; expected_len]
    })
}

/// Cuántas instancias vivas se retienen a la vez. Ocho porque una página son
/// dos paneles y unas pocas columnas: por encima de eso lo que se retiene es
/// memoria de directorios que ya nadie mira.
const POOL_MAX: usize = 8;

/// Cuánto sobrevive una instancia sin usarse. Un minuto es "el lector sigue
/// paginando por aquí"; más allá, quien vuelve prefiere no estar pagando la
/// memoria del guest de un directorio que dejó atrás.
const POOL_TTL: std::time::Duration = std::time::Duration::from_mins(1);

/// Una instancia de columnas VIVA, con lo que hace falta para saber si sigue
/// sirviendo.
struct EnPool {
    /// `(id del plugin, wasm, ubicación en wire)`. La ubicación entra en la
    /// clave porque es lo que el guest cachea dentro: un `.git/index` parseado
    /// no vale para otro proyecto.
    clave: (String, std::path::PathBuf, String),
    /// Los permisos con los que se instanció. Si el catálogo resuelve otros
    /// —un consentimiento retirado, un manifiesto reinstalado— la instancia se
    /// TIRA: reutilizarla sería correr con permisos que ya nadie concede.
    caps: norte_plugin_host::Capabilities,
    /// Quién resuelve los tokens de esta instancia. Sobrevive a la llamada; lo
    /// que no sobrevive es la SESIÓN, que se acuña y se suelta en cada una.
    mint: std::sync::Arc<LocationMint>,
    inst: norte_plugin_host::ColumnsInstance,
    ultima: std::time::Instant,
}

/// Instancias de columnas reutilizadas entre páginas (#224).
///
/// El coste medido de una página de veinte filas sobre un índice git de dos
/// mil entradas era **167 ms**, con el componente WASM instanciado y el
/// `.git/index` parseado desde cero en cada llamada. Nada de eso es trabajo
/// que cambie entre la página 1 y la página 2 del mismo directorio.
///
/// **Lo que el pool compra no es solo reloj.** La frescura es deliberadamente
/// problema del guest (el host no puede saber de qué depende su respuesta), y
/// un guest que no sobrevive a la llamada no puede cachear NADA: sin pool, esa
/// caché no está sin usar, está prohibida.
///
/// Vive aquí, al lado de `run_column_values` —privada, así que sin enlace—,
/// porque hacen falta los dos
/// caminos: el daemon lo cuelga de su estado compartido y el backend embebido
/// del suyo. Uno sí y el otro no recrearía justo la asimetría de #165/#201/#181.
///
/// **Lo que el pool NO retiene es un token vivo.** La sesión de ubicación se
/// acuña al empezar cada llamada y se suelta al acabarla —su `Drop` la retira
/// del acuñador—, así que entre página y página la instancia guardada tiene un
/// `LocationHost` que no resuelve nada.
#[derive(Default)]
pub struct ColumnPool {
    /// Más reciente al final. Ocho como mucho, así que un `Vec` con búsqueda
    /// lineal es más rápido —y mucho más fácil de leer— que un mapa con orden
    /// de uso al lado.
    vivas: std::sync::Mutex<Vec<EnPool>>,
    /// Cuántas llamadas encontraron su instancia ya viva.
    ///
    /// Es lo que hace TESTEABLE el pool sin cronómetro: que la segunda página
    /// tarde menos es el síntoma, y un síntoma medido en milisegundos se pone
    /// rojo el día que la máquina va cargada. Que la instancia se reutilizó es
    /// el hecho, y es determinista.
    reutilizadas: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for ColumnPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A mano y no derivado porque una `ColumnsInstance` no tiene `Debug`
        // útil (su `Store` de wasmtime no lo tiene), así que lo que se imprime
        // es CUÁNTAS hay, no cuáles.
        let n = self.vivas.lock().map_or(0, |v| v.len());
        f.debug_struct("ColumnPool")
            .field("vivas", &n)
            .field("reutilizadas", &self.reutilizadas)
            .finish()
    }
}

impl ColumnPool {
    /// Los valores de la columna, reutilizando la instancia de esta
    /// `(plugin, ubicación)` si sigue viva y con los mismos permisos.
    ///
    /// Cuántas llamadas encontraron su instancia viva. Ver
    /// [`Self::reutilizadas`].
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    #[must_use]
    pub fn reutilizadas(&self) -> u64 {
        self.reutilizadas.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// [`Self::column_values`] para los e2e, por el mismo motivo y con la misma
    /// advertencia que [`run_column_values_for_test`]: el test que importa vive
    /// fuera de este crate, y montar ahí una versión propia del camino es como
    /// dos caminos se separan.
    #[cfg(any(test, feature = "testing"))]
    #[doc(hidden)]
    #[must_use]
    #[allow(clippy::too_many_arguments)] // la MISMA lista que `run_column_values`, y a propósito
    pub fn column_values_for_test(
        &self,
        runtime: &norte_plugin_host::PluginRuntime,
        resolved: ResolvedDecorator,
        column_id: &str,
        location_dir: Option<&norte_proto::VPath>,
        climb: bool,
        entries: &[Vec<u8>],
        expected_len: usize,
    ) -> Vec<Option<String>> {
        self.column_values(
            runtime,
            resolved,
            column_id,
            location_dir,
            climb,
            entries,
            expected_len,
        )
    }

    /// Mismo contrato que `run_column_values` hasta en la degradación: lo
    /// que no se puede hacer sale como celdas vacías, jamás como un error que
    /// tumbe el listado. Y BLOQUEANTE igual: va en `spawn_blocking`.
    #[allow(clippy::too_many_arguments)] // la MISMA lista que `run_column_values`, y a propósito
    pub(crate) fn column_values(
        &self,
        runtime: &norte_plugin_host::PluginRuntime,
        resolved: ResolvedDecorator,
        column_id: &str,
        location_dir: Option<&norte_proto::VPath>,
        climb: bool,
        entries: &[Vec<u8>],
        expected_len: usize,
    ) -> Vec<Option<String>> {
        let (id, name, wasm, caps, settings) = resolved;
        let clave = (
            id.clone(),
            wasm.clone(),
            location_dir
                .map(norte_proto::VPath::to_wire)
                .unwrap_or_default(),
        );
        let Ok(mut vivas) = self.vivas.lock() else {
            // El mutex envenenado no es motivo para dejar sin columnas a nadie:
            // se cae al camino sin pool, que es el de siempre.
            tracing::warn!("columns: pool envenenado, se instancia sin reutilizar");
            return run_column_values(
                runtime,
                (id, name, wasm, caps, settings),
                column_id,
                location_dir,
                climb,
                entries,
                expected_len,
            );
        };
        let ahora = std::time::Instant::now();
        vivas.retain(|e| ahora.duration_since(e.ultima) < POOL_TTL);
        let hallada = vivas
            .iter()
            .position(|e| e.clave == clave && e.caps == caps)
            .map(|i| vivas.remove(i));
        // La instancia sale del pool mientras se usa: el mutex se suelta antes
        // de entrar al guest, que es la llamada larga, y dos páginas del mismo
        // directorio a la vez instancian por separado en vez de serializarse.
        drop(vivas);

        let mut entrada = if let Some(e) = hallada {
            self.reutilizadas
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            e
        } else {
            let mint = LocationMint::new(norte_vfs_local::Bounds::default());
            let host: Option<std::sync::Arc<dyn norte_plugin_host::LocationHost>> =
                if caps.location.granted() {
                    Some(std::sync::Arc::clone(&mint)
                        as std::sync::Arc<dyn norte_plugin_host::LocationHost>)
                } else {
                    None
                };
            let Ok(inst) = runtime.instantiate_columns_with_location(&wasm, caps.clone(), host)
            else {
                tracing::warn!(plugin = %id, "columns: fallo al instanciar, celdas vacías");
                return vec![None; expected_len];
            };
            EnPool {
                clave,
                caps,
                mint,
                inst,
                ultima: ahora,
            }
        };

        // La sesión se acuña AQUÍ y muere al final de esta función, la use una
        // instancia nueva o una reutilizada: lo que se retiene entre páginas es
        // el guest y su memoria, nunca el permiso de leer.
        let sesion = if entrada.caps.location.granted() {
            location_dir.and_then(|dir| {
                entrada
                    .mint
                    .mint_for(dir, entrada.caps.location_root_marker.as_deref(), climb)
            })
        } else {
            None
        };
        entrada.inst.set_settings(settings);
        let refe = sesion.as_ref().map(LocationSession::as_ref);
        let salida = entrada
            .inst
            .column_values(column_id, refe.as_ref(), entries);
        drop(sesion);

        let raw = match salida {
            Ok(raw) => raw,
            Err(e) => {
                // Una instancia que falló NO vuelve al pool: un guest que
                // atrapó puede haber dejado su memoria lineal a medias, y
                // reutilizarla es servir esa mitad en la página siguiente.
                tracing::warn!(plugin = %id, error = %e, "columns: fallo al ejecutar, celdas vacías");
                return vec![None; expected_len];
            }
        };
        entrada.ultima = std::time::Instant::now();
        if let Ok(mut vivas) = self.vivas.lock() {
            vivas.push(entrada);
            // Por arriba caen las MÁS VIEJAS, que es lo que hace de esto una
            // LRU: cada uso vuelve a poner la suya al final.
            if vivas.len() > POOL_MAX {
                let sobran = vivas.len() - POOL_MAX;
                vivas.drain(..sobran);
            }
        }
        column_values_checked(raw, expected_len).unwrap_or_else(|| {
            tracing::warn!(
                plugin = %id,
                "columns: longitud no casa el contrato posicional, celdas vacías"
            );
            vec![None; expected_len]
        })
    }
}
