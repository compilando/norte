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
        Some("txt" | "md" | "rs" | "toml" | "log" | "csv" | "ini" | "conf") => "text/plain",
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
                    capabilities: e
                        .manifest
                        .capabilities
                        .badges()
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    // Aprobación EFECTIVA (issue #69): `approved` en el fichero
                    // pero con el digest de capabilities CASANDO el del manifiesto
                    // actual. Si las capabilities cambiaron en disco tras aprobar,
                    // la UI ve `approved = false` y vuelve a pedir consentimiento.
                    approved: Self::approval_is_current(&st, &e.manifest),
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
            .map(|e| PluginLoadError {
                // Solo el NOMBRE del directorio del plugin, nunca la ruta
                // absoluta: revelaría el home del usuario (`~/.config/norte/...`)
                // a un agente que llame a `plugin.list`. El basename basta para
                // que un humano identifique el plugin roto.
                dir: e
                    .dir
                    .file_name()
                    .map_or_else(|| e.dir.to_string_lossy(), |n| n.to_string_lossy())
                    .into_owned(),
                reason: e.error.to_string(),
            })
            .collect();
        PluginListResult { plugins, errors }
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
        let Some(digest) = self.current_digest(id) else {
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
        if !Self::approval_is_current(&st, &entry.manifest) {
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

    /// Resuelve el PRIMER previewer APROBADO y ACTIVADO cuyo mimetype declarado
    /// case `mime`, devolviendo `(id, name, wasm_path, capabilities, settings)`;
    /// `None` si ninguno aplica. Fail-closed: un previewer no consentido jamás
    /// se elige. Barato: el caller lee los bytes del archivo y ejecuta fuera del
    /// lock.
    ///
    /// `settings` (P2 Task 4a) son los valores de `[config]` YA resueltos
    /// ([`Self::settings_of`]) — el caller debe pasarlos a
    /// `PluginInstance::set_settings` ANTES de `render_preview` para que el
    /// guest los vea vía `host-config` (Task 3), igual que
    /// [`Self::run_command`] ya hace para los comandos.
    #[must_use]
    pub fn resolve_previewer(&self, mime: &str) -> Option<ResolvedPreviewer> {
        self.catalog.plugins.iter().find_map(|e| {
            let st = self.state.get(&e.manifest.id).cloned().unwrap_or_default();
            // Fail-closed con digest vigente (issue #69): un previewer cuyo
            // manifiesto cambió tras aprobar NO se elige hasta re-consentir.
            if !Self::approval_is_current(&st, &e.manifest) || !st.enabled {
                return None;
            }
            let handles = e
                .manifest
                .contributions
                .previewer
                .iter()
                .flat_map(|c| c.mimetypes.iter())
                .any(|pat| mimetype_matches(pat, mime));
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
                if !Self::approval_is_current(&st, &e.manifest) || !st.enabled {
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
            if !Self::approval_is_current(&st, &e.manifest) || !st.enabled {
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

    /// Digest actual del MANIFIESTO de `id` en el catálogo (capabilities +
    /// category + contributions), o `None` si el id no está descubierto (issue
    /// #69).
    fn current_digest(&self, id: &str) -> Option<String> {
        self.catalog
            .plugins
            .iter()
            .find(|e| e.manifest.id == id)
            .map(|e| e.manifest.approval_digest())
    }

    /// `true` si la aprobación es VIGENTE (issue #69): el humano aprobó Y el
    /// digest anclado casa el del manifiesto actual (no solo sus capabilities,
    /// también `category`/`contributions` — cuándo/cómo se dispara). Un
    /// `approved_digest` ausente (aprobación heredada sin ancla) NUNCA casa →
    /// re-consentimiento.
    fn approval_is_current(st: &PluginState, manifest: &norte_plugin_host::Manifest) -> bool {
        st.approved && st.approved_digest.as_deref() == Some(manifest.approval_digest().as_str())
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
        // El digest del manifiesto anclado al aprobar (issue #69) también
        // sobrevive al round-trip y casa el manifiesto actual.
        let manifest = norte_plugin_host::Manifest::from_toml(DEMO_MANIFEST).unwrap();
        assert_eq!(
            st.approved_digest.as_deref(),
            Some(manifest.approval_digest().as_str()),
            "el digest del manifiesto debe persistir y casar el manifiesto"
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
