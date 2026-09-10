//! Manifiesto `plugin.toml` (ADR 0022 D3): identidad + contribuciones por
//! interfaz (estilo `contributes` de `VSCode`) + capabilities + `[config]`
//! (P2: settings tipadas declaradas por el plugin).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::capability::Capabilities;

/// Categoría PRIMARIA del plugin = la interfaz WIT por la que se ordena en el
/// gestor (spec §7.1). Un plugin puede contribuir a varias, pero declara una
/// principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// Genera previews para mimetypes.
    Previewer,
    /// Provider VFS de terceros.
    Provider,
    /// Comando invocable desde palette/keybinding.
    Command,
    /// Columnas custom en el listado.
    Columns,
    /// Observa las mutaciones que el journal ya registró (H1, ADR 0100,
    /// interfaz WIT `hook` del paquete `norte:hook`, world `norte-hook`).
    /// Solo `after-*`: un hook no veta ni muta, y su único efecto es una
    /// frase para el humano. Los eventos que escucha van en
    /// `[[contributions.hook]]`, del vocabulario [`HOOK_EVENTS`].
    Hook,
    /// Decora entradas visibles con un badge/rol tipo "git status" (ADR
    /// 0037 decisión 2, interfaz WIT `decorator`, world `norte-decorator`).
    Decorator,
    /// Propone pares de renombrado para un lote (C3, ADR 0095, interfaz WIT
    /// `renamer` del paquete `norte:renamer`, world `norte-renamer`). El
    /// core los ejecuta por el mismo camino que el plan de la IA.
    Renamer,
}

impl Category {
    /// Nombre estable (para agrupar en la UI y trazas).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Previewer => "previewer",
            Category::Provider => "provider",
            Category::Command => "command",
            Category::Columns => "columns",
            Category::Hook => "hook",
            Category::Decorator => "decorator",
            Category::Renamer => "renamer",
        }
    }

    /// Byte canónico y estable para el digest de aprobación (issue #69). NO se
    /// usa el discriminante del enum (podría reordenarse) sino un valor fijo.
    /// `Decorator` = 5 (ADR 0037): un valor NUEVO al final, nunca reutiliza ni
    /// reordena los existentes — los digests de manifiestos previos a esta
    /// categoría no se ven afectados por su sola existencia.
    fn digest_tag(self) -> u8 {
        match self {
            Category::Previewer => 0,
            Category::Provider => 1,
            Category::Command => 2,
            Category::Columns => 3,
            Category::Hook => 4,
            Category::Decorator => 5,
            // Nuevo al final (ADR 0095), como `Decorator` en su día.
            Category::Renamer => 6,
        }
    }
}

/// Un previewer declarado: los mimetypes que sabe pintar.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewerContrib {
    /// Patrones de mimetype (`text/*`, `application/json`).
    pub mimetypes: Vec<String>,
}

/// Un comando declarado.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContrib {
    /// Id estable (namespaced por el plugin al registrarlo).
    pub id: String,
    /// Título para la palette.
    pub title: String,
}

/// Una columna declarada.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnContrib {
    /// Id estable.
    pub id: String,
    /// Cabecera visible.
    pub header: String,
}

/// Un provider declarado: el scheme que sirve (`webdav`, …).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContrib {
    /// Scheme del VFS (sin `://`).
    pub scheme: String,
    /// Puerto al que conecta el guest cuando la URL no lo dice (`default-port
    /// = 8443`). Un provider plugin recibe red a `ip:puerto`, nunca a la IP
    /// entera, y el host no conoce el puerto por defecto de un scheme ajeno:
    /// sin este campo y sin puerto en la URL, la conexión se rehúsa. Entra en
    /// el digest de aprobación como el scheme.
    #[serde(default, rename = "default-port")]
    pub default_port: Option<u16>,
}

/// Un renamer declarado (C3, ADR 0095): un proponente de nombres con su id
/// y su título. Un plugin puede aportar varios («por fecha EXIF», «por
/// título ID3»); el id es lo que viaja a `renamer.plan` y el título lo que
/// la paleta enseña.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenamerContrib {
    /// Id estable dentro del plugin.
    pub id: String,
    /// Título legible. Texto del plugin — NO confiable.
    pub title: String,
}

/// Un hook declarado: el evento al que engancha.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookContrib {
    /// Evento, uno de [`HOOK_EVENTS`] (`after-renamed`, …). Cerrado y
    /// validado al parsear: un valor que no esté es
    /// [`ManifestError::HookUnknownEvent`], como una capability desconocida.
    pub on: String,
}

/// Cuántos sidecars puede declarar un hook. Dieciséis es «varios ficheros
/// de trabajo»; por encima es un plugin que quiere un directorio, y eso es
/// otra capacidad.
pub const SIDECAR_MAX_NAMES: usize = 16;

/// Tope de bytes del nombre de un sidecar: `NAME_MAX` en los sistemas de
/// ficheros corrientes.
const SIDECAR_NAME_MAX_BYTES: usize = 255;

/// `true` si `name` es UN nombre de fichero PORTABLE que el host aceptará
/// escribir junto a un evento: ASCII imprimible sin `/ \ : * ? " < > |`, no
/// vacío, cabe en `NAME_MAX`, ni `.` ni `..`, sin punto final y sin nombre
/// reservado de Windows (`CON`, `NUL`, `COM1`…). ASCII porque el nombre es
/// texto de un tercero que se pinta en la aprobación y se escribe en disco:
/// sin bidi, sin invisibles, sin homógrafos.
///
/// ```
/// use norte_plugin_host::is_valid_sidecar_name;
/// assert!(is_valid_sidecar_name(".norte-renames.log"));
/// assert!(!is_valid_sidecar_name("a/b"));
/// assert!(!is_valid_sidecar_name(".."));
/// assert!(!is_valid_sidecar_name("CON"));
/// assert!(!is_valid_sidecar_name("log\u{202e}"));
/// ```
#[must_use]
pub fn is_valid_sidecar_name(name: &str) -> bool {
    const FORBIDDEN: &[u8] = b"/\\:*?\"<>|";
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if name.is_empty() || name.len() > SIDECAR_NAME_MAX_BYTES || name == "." || name == ".." {
        return false;
    }
    if !name
        .bytes()
        .all(|b| (0x21..=0x7e).contains(&b) && !FORBIDDEN.contains(&b))
    {
        return false;
    }
    if name.ends_with('.') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    !RESERVED.contains(&stem.as_str())
}

/// El vocabulario CERRADO de `[[contributions.hook]].on` (ADR 0100): las
/// operaciones del journal, en pasado, porque un hook solo ve lo que ya
/// quedó registrado. No hay `before-*` — eso sería policy, no un plugin.
///
/// Está aquí y no en el core porque las tres piezas que tienen que estar de
/// acuerdo —quien lo valida (este crate), quien lo dispara (`norte-core`) y
/// la guía— parten de una lista; sin ella cada una guarda su copia.
pub const HOOK_EVENTS: &[&str] = &[
    "after-created",
    "after-removed",
    "after-trashed",
    "after-renamed",
    "after-mode-changed",
];

/// Un decorator declarado (ADR 0037 decisión 2): marcador VACÍO — a
/// diferencia de [`PreviewerContrib`]/[`ColumnContrib`], un decorator no
/// declara mimetypes ni ids: la interfaz WIT `decorator::decorate` se llama
/// para TODA entrada visible de la página (batched, sin filtro previo por
/// tipo). La entrada existe (en vez de que `category = "decorator"` baste
/// por sí sola) para dejar sitio simétrico a futuros campos (p. ej. un glob
/// de exclusión) sin otro cambio de forma del manifiesto; hoy es
/// deliberadamente `{}` — `deny_unknown_fields` para que un campo hostil
/// desconocido rechace el manifiesto en vez de ignorarse en silencio.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecoratorContrib {
    /// En qué HUECO de la fila se pinta lo que este decorador devuelve
    /// (ADR 0105): `badge` (por defecto) a la derecha del nombre, como un
    /// estado de git; `icon` a la izquierda, en una columna de ancho fijo.
    /// Los dos huecos coexisten: un icono y una insignia en la misma fila
    /// vienen de dos plugins distintos. Entra en el digest solo cuando no es
    /// el valor por defecto, para que ningún manifiesto anterior cambie de
    /// ancla.
    #[serde(default)]
    pub slot: DecoratorSlot,
}

/// El hueco de la fila que llena un decorador (ADR 0105).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecoratorSlot {
    /// A la derecha del nombre, texto corto: `M`, `++`.
    #[default]
    Badge,
    /// A la izquierda del nombre, un glifo por fila.
    Icon,
}

/// Lo que el plugin APORTA, por interfaz. Todo opcional: un plugin de una sola
/// interfaz solo rellena la suya.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    /// Previewers.
    #[serde(default)]
    pub previewer: Vec<PreviewerContrib>,
    /// Comandos.
    #[serde(default)]
    pub command: Vec<CommandContrib>,
    /// Columnas.
    #[serde(default)]
    pub columns: Vec<ColumnContrib>,
    /// Providers.
    #[serde(default)]
    pub provider: Vec<ProviderContrib>,
    /// Hooks.
    #[serde(default)]
    pub hook: Vec<HookContrib>,
    /// Decorators (ADR 0037 decisión 2). A diferencia de las demás
    /// secciones, esta NO entra en `Contributions::update_digest`
    /// (unconditional): sigue el patrón OPCIONAL de `[config]` — ver
    /// `update_decorator_digest` — para que un manifiesto sin
    /// `[[contributions.decorator]]` digeste EXACTAMENTE igual que antes de
    /// esta categoría (las aprobaciones humanas ya existentes no se
    /// resetean por la sola introducción del campo).
    #[serde(default)]
    pub decorator: Vec<DecoratorContrib>,
    /// Renamers (C3, ADR 0095). Aditivo y con default, como los demás: los
    /// manifiestos anteriores no lo traen y su digest no se mueve.
    #[serde(default)]
    pub renamer: Vec<RenamerContrib>,
}

impl Contributions {
    /// Alimenta un hasher con la forma CANÓNICA de las contribuciones, SIN
    /// finalizar (issue #69). Son los campos que deciden CUÁNDO/CÓMO se dispara
    /// el plugin (mimetypes del previewer, ids de comando, schemes de provider,
    /// eventos de hook): cambiarlos manteniendo las mismas capabilities NO debe
    /// conservar la aprobación (si no, un `command` reeditado a `previewer`
    /// pasaría a auto-ejecutarse en el viewer sobre ficheros que casen). El orden
    /// se preserva (no se ordena): un reorden dispara re-consentimiento —
    /// conservador y fail-closed. Cada sección va con su nº de entradas y cada
    /// cadena longitud-prefijada (sin ambigüedad entre secciones).
    fn update_digest(&self, h: &mut sha2::Sha256) {
        use crate::capability::update_str;
        use sha2::Digest;

        h.update((self.previewer.len() as u64).to_le_bytes());
        for c in &self.previewer {
            h.update((c.mimetypes.len() as u64).to_le_bytes());
            for m in &c.mimetypes {
                update_str(h, m);
            }
        }
        h.update((self.command.len() as u64).to_le_bytes());
        for c in &self.command {
            update_str(h, &c.id);
            update_str(h, &c.title);
        }
        h.update((self.columns.len() as u64).to_le_bytes());
        for c in &self.columns {
            update_str(h, &c.id);
            update_str(h, &c.header);
        }
        h.update((self.provider.len() as u64).to_le_bytes());
        for c in &self.provider {
            update_str(h, &c.scheme);
            // Presencia + valor, como cualquier opcional del digest: el
            // puerto decide a qué se concede red, así que cambiarlo tras
            // aprobar es cambiar lo aprobado.
            h.update([u8::from(c.default_port.is_some())]);
            h.update(c.default_port.unwrap_or_default().to_le_bytes());
        }
        h.update((self.hook.len() as u64).to_le_bytes());
        for c in &self.hook {
            update_str(h, &c.on);
        }
        // Los renamers, DETRÁS y solo si hay: un manifiesto sin ninguno
        // digesta exactamente lo que digestaba antes de que existieran, y
        // ninguna aprobación se resetea por su sola introducción. Con alguno,
        // el separador fijo y cada par id/título, como los comandos: cambiar
        // qué propone un plugin es cambiar lo aprobado.
        if !self.renamer.is_empty() {
            h.update(b"renamer:\n");
            h.update((self.renamer.len() as u64).to_le_bytes());
            for c in &self.renamer {
                update_str(h, &c.id);
                update_str(h, &c.title);
            }
        }
    }
}

/// Una clave `[config.<key>]` del manifiesto (P2), ya validada: el `type`
/// TOML fija la forma exacta (mirror del estilo de [`Capabilities`] — un
/// permiso ausente/campo no aplicable simplemente no existe en la variante).
/// `description` es cosmética para la UI del gestor y está deliberadamente
/// FUERA de [`Manifest::approval_digest`] (mismo criterio que
/// [`Manifest::description`]): editarla no reinvalida capabilities ya
/// aprobadas, porque no cambia qué valores puede tomar la clave.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigKeySpec {
    /// `type = "string"`.
    String {
        /// Valor por defecto (tope [`CONFIG_STRING_MAX_CHARS`] caracteres).
        default: String,
        /// Texto cosmético para la UI (tope [`CONFIG_DESCRIPTION_MAX_CHARS`]
        /// caracteres). NO entra en el digest de aprobación.
        description: Option<String>,
    },
    /// `type = "bool"`.
    Bool {
        /// Valor por defecto.
        default: bool,
        /// Ver [`ConfigKeySpec::String::description`].
        description: Option<String>,
    },
    /// `type = "int"`.
    Int {
        /// Valor por defecto; DEBE caer dentro de `[min, max]` cuando se
        /// declaran (validado al parsear, fail-loud).
        default: i64,
        /// Cota inferior inclusive (opcional).
        min: Option<i64>,
        /// Cota superior inclusive (opcional).
        max: Option<i64>,
        /// Ver [`ConfigKeySpec::String::description`].
        description: Option<String>,
    },
    /// `type = "enum"`.
    Enum {
        /// Valor por defecto; DEBE estar en `values` (validado al parsear,
        /// fail-loud).
        default: String,
        /// Valores permitidos (tope [`CONFIG_ENUM_MAX_VALUES`] entradas, cada
        /// una hasta [`CONFIG_STRING_MAX_CHARS`] caracteres).
        values: Vec<String>,
        /// Ver [`ConfigKeySpec::String::description`].
        description: Option<String>,
    },
}

impl ConfigKeySpec {
    /// Byte canónico y estable para el digest de aprobación (mismo criterio
    /// que [`Category::digest_tag`]/`Scope::digest_tag`): NO se usa el
    /// discriminante del enum (podría reordenarse) sino un valor fijo.
    fn digest_tag(&self) -> u8 {
        match self {
            ConfigKeySpec::String { .. } => 0,
            ConfigKeySpec::Bool { .. } => 1,
            ConfigKeySpec::Int { .. } => 2,
            ConfigKeySpec::Enum { .. } => 3,
        }
    }

    /// Alimenta un hasher con la forma CANÓNICA de esta clave, SIN finalizar
    /// (compone [`Manifest::approval_digest`]): tag de tipo + los campos que
    /// afectan comportamiento (`default`, `min`, `max`, `values`).
    /// `description` se EXCLUYE a propósito (cosmética, ver el doc del tipo).
    fn update_digest(&self, h: &mut sha2::Sha256) {
        use crate::capability::{update_opt_i64, update_str};
        use sha2::Digest;
        h.update([self.digest_tag()]);
        match self {
            ConfigKeySpec::String { default, .. } => update_str(h, default),
            ConfigKeySpec::Bool { default, .. } => h.update([u8::from(*default)]),
            ConfigKeySpec::Int {
                default, min, max, ..
            } => {
                h.update(default.to_le_bytes());
                update_opt_i64(h, *min);
                update_opt_i64(h, *max);
            }
            ConfigKeySpec::Enum {
                default, values, ..
            } => {
                update_str(h, default);
                // `values` es una LISTA ordenada (no un conjunto): el orden en
                // que el usuario las declara es el orden en que se muestran en
                // la UI (ADR-style: mismo criterio que `Contributions`, que
                // tampoco ordena). Cambiar el orden SÍ mueve el digest.
                h.update((values.len() as u64).to_le_bytes());
                for v in values {
                    update_str(h, v);
                }
            }
        }
    }
}

/// Forma cruda de una entrada `[config.<key>]` (antes de validar). El campo
/// `type` (`serde(tag = "type")`) selecciona la variante; TOML es
/// autodescriptivo así que el tag interno funciona sin ambigüedad.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum ConfigKeyRaw {
    /// `type = "string"`.
    String {
        default: String,
        #[serde(default)]
        description: Option<String>,
    },
    /// `type = "bool"`.
    Bool {
        default: bool,
        #[serde(default)]
        description: Option<String>,
    },
    /// `type = "int"`.
    Int {
        default: i64,
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
        #[serde(default)]
        description: Option<String>,
    },
    /// `type = "enum"`.
    Enum {
        default: String,
        values: Vec<String>,
        #[serde(default)]
        description: Option<String>,
    },
}

/// Tope de claves en `[config]` (P2 decisión 1).
pub const CONFIG_MAX_KEYS: usize = 32;
/// Tope de longitud de una clave `[config.<key>]`; el charset permitido es
/// `[a-z0-9-]{1,32}` (P2 decisión 1) — ni mayúsculas ni `_` ni no-ASCII, para
/// que la clave sea segura de interpolar en TOML de valores
/// (`config_dir/plugins/<id>/config.toml`), logs y la UI del gestor sin
/// escapado.
pub const CONFIG_KEY_MAX_CHARS: usize = 32;
/// Tope de `[config.<key>].description` (P2 decisión 1), mismo criterio que
/// [`Manifest::description`] (280 CARACTERES, no bytes).
pub const CONFIG_DESCRIPTION_MAX_CHARS: usize = 280;
/// Tope de un `default` de tipo `string`, o de cada entrada de `values`
/// (enum) (P2 decisión 1), en CARACTERES.
pub const CONFIG_STRING_MAX_CHARS: usize = 280;
/// Tope de entradas en `[config.<key>].values` (enum) (P2 decisión 1).
pub const CONFIG_ENUM_MAX_VALUES: usize = 16;

/// `true` si `key` respeta el charset `[a-z0-9-]{1,32}` (P2 decisión 1): solo
/// minúsculas ASCII, dígitos y guion, longitud `1..=32`. `pub(crate)`: la
/// reutiliza `config_values.rs` (security review P2 Task 4a) para decidir si
/// una clave DESCONOCIDA de `config.toml` es segura de interpolar en un
/// mensaje de error — el mismo charset acotado (ASCII, sin control/bidi, tope
/// de longitud) que ya garantiza toda clave DECLARADA en el esquema.
pub(crate) fn is_valid_config_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= CONFIG_KEY_MAX_CHARS
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Valida una entrada cruda `[config.<key>]` contra sus propios topes de tipo
/// y devuelve la forma ya validada. `key` NO se usa en los mensajes de error
/// (mismo criterio que `Id`/`DuplicateId`... salvo que aquí ni siquiera el id
/// de plugin, ya validado, se arriesga: la clave de config puede venir de
/// CUALQUIER TOML hostil antes de pasar el charset).
fn validate_config_entry(raw: ConfigKeyRaw) -> Result<ConfigKeySpec, ManifestError> {
    fn check_description(description: Option<&String>) -> Result<(), ManifestError> {
        if description.is_some_and(|d| d.chars().count() > CONFIG_DESCRIPTION_MAX_CHARS) {
            return Err(ManifestError::ConfigDescriptionTooLong);
        }
        Ok(())
    }

    match raw {
        ConfigKeyRaw::String {
            default,
            description,
        } => {
            check_description(description.as_ref())?;
            if default.chars().count() > CONFIG_STRING_MAX_CHARS {
                return Err(ManifestError::ConfigDefaultTooLong);
            }
            Ok(ConfigKeySpec::String {
                default,
                description,
            })
        }
        ConfigKeyRaw::Bool {
            default,
            description,
        } => {
            check_description(description.as_ref())?;
            Ok(ConfigKeySpec::Bool {
                default,
                description,
            })
        }
        ConfigKeyRaw::Int {
            default,
            min,
            max,
            description,
        } => {
            check_description(description.as_ref())?;
            if min.is_some_and(|m| default < m) || max.is_some_and(|m| default > m) {
                return Err(ManifestError::ConfigIntDefaultOutOfRange);
            }
            Ok(ConfigKeySpec::Int {
                default,
                min,
                max,
                description,
            })
        }
        ConfigKeyRaw::Enum {
            default,
            values,
            description,
        } => {
            check_description(description.as_ref())?;
            if values.len() > CONFIG_ENUM_MAX_VALUES {
                return Err(ManifestError::ConfigEnumTooManyValues);
            }
            if values
                .iter()
                .any(|v| v.chars().count() > CONFIG_STRING_MAX_CHARS)
            {
                return Err(ManifestError::ConfigEnumValueTooLong);
            }
            if !values.iter().any(|v| v == &default) {
                return Err(ManifestError::ConfigEnumDefaultNotInValues);
            }
            Ok(ConfigKeySpec::Enum {
                default,
                values,
                description,
            })
        }
    }
}

/// Alimenta un hasher con la forma CANÓNICA de `[config]` completo, SIN
/// finalizar (P2 decisión 2): la sección `config:` SOLO se añade si `config`
/// NO está vacío — un manifiesto sin `[config]` (o con una tabla presente
/// pero sin claves) digesta IGUAL que antes de P2, así que las aprobaciones
/// humanas ya existentes de plugins que no usan `[config]` NUNCA se
/// resetean. El `BTreeMap` ya itera en orden de clave (determinista, no
/// depende del orden en el fichero).
fn update_config_digest(config: &BTreeMap<String, ConfigKeySpec>, h: &mut sha2::Sha256) {
    use crate::capability::update_str;
    use sha2::Digest;
    if config.is_empty() {
        return;
    }
    // Domain separator FIJO (no interpolado, no ambiguo con contenido de
    // usuario): marca dónde empieza la sección opcional.
    h.update(b"config:\n");
    h.update((config.len() as u64).to_le_bytes());
    for (key, spec) in config {
        update_str(h, key);
        spec.update_digest(h);
    }
}

/// Alimenta un hasher con la forma CANÓNICA de `contributions.decorator`, SIN
/// finalizar (ADR 0037 decisión 2): mismo patrón OPCIONAL que
/// [`update_config_digest`] — la sección `decorator:` SOLO se añade si el
/// `Vec` NO está vacío, así que un manifiesto sin
/// `[[contributions.decorator]]` (la inmensa mayoría, incluidos TODOS los
/// manifiestos que existían antes de esta categoría) digesta EXACTAMENTE
/// igual que antes de este cambio — ninguna aprobación humana existente se
/// resetea por la sola introducción del campo. Un manifiesto que SÍ declara
/// al menos un decorator mueve el digest (fuerza consentimiento) porque
/// pasar a `category = "decorator"` cambia radicalmente cuándo/cómo se
/// dispara el plugin.
fn update_decorator_digest(decorator: &[DecoratorContrib], h: &mut sha2::Sha256) {
    use sha2::Digest;
    if decorator.is_empty() {
        return;
    }
    // Domain separator FIJO, igual criterio que `update_config_digest`.
    h.update(b"decorator:\n");
    h.update((decorator.len() as u64).to_le_bytes());
    // El hueco (ADR 0105) SOLO cuando no es el de siempre: un manifiesto
    // anterior a la columna de iconos digesta byte a byte igual que antes, y
    // pasar a `icon` mueve el ancla porque cambia dónde se pinta el plugin.
    // CON su posición: el core lee el hueco de la PRIMERA contribución, y
    // sin el índice reordenar dos bloques movería un plugin aprobado de la
    // insignia a la columna de iconos sin que el digest se enterase.
    for (i, d) in decorator.iter().enumerate() {
        if d.slot == DecoratorSlot::Icon {
            h.update(b"slot:icon@");
            h.update((i as u64).to_le_bytes());
        }
    }
}

/// Bloque `[plugin]` del manifiesto.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginSection {
    id: String,
    name: String,
    publisher: String,
    version: String,
    category: Category,
    /// Descripción cosmética (P1); ausente = `None`. Cap 280 chars en
    /// [`Manifest::from_toml`] — ver [`Manifest::description`].
    #[serde(default)]
    description: Option<String>,
}

/// Forma cruda del TOML (antes de validar).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRaw {
    plugin: PluginSection,
    #[serde(default)]
    contributions: Contributions,
    #[serde(default)]
    capabilities: Capabilities,
    /// `[config.<key>]` (P2); ausente = mapa vacío. `BTreeMap` para que el
    /// orden de iteración sea determinista independientemente del orden en
    /// el fichero (relevante para [`Manifest::approval_digest`]).
    #[serde(default)]
    config: BTreeMap<String, ConfigKeyRaw>,
}

/// El manifiesto ya validado de un plugin.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Id reverse-DNS único (`org.norte.syntax-preview`).
    pub id: String,
    /// Nombre legible.
    pub name: String,
    /// Publicador.
    pub publisher: String,
    /// Versión (`SemVer`, sin validar aquí).
    pub version: String,
    /// Categoría primaria (ordena el gestor).
    pub category: Category,
    /// Descripción cosmética (P1), tope 280 caracteres (fail-loud al parsear,
    /// como `id`). `None` si el manifiesto no la declara. Texto suministrado
    /// por el plugin — NO confiable, un frontend debe enmascararla antes de
    /// renderizarla. Deliberadamente FUERA de [`Manifest::approval_digest`]
    /// (mismo trato que `name`/`publisher`/`version`): editarla no reinvalida
    /// capabilities ya aprobadas por el humano, porque no cambia qué hace el
    /// plugin ni cuándo se dispara.
    pub description: Option<String>,
    /// Contribuciones por interfaz.
    pub contributions: Contributions,
    /// Capabilities declaradas.
    pub capabilities: Capabilities,
    /// Settings tipadas declaradas por el plugin (P2), ya validadas.
    /// `BTreeMap` para orden determinista por clave. Ausente `[config]` en el
    /// TOML ⇒ mapa vacío. SÍ entra en [`Manifest::approval_digest`] (afecta
    /// comportamiento: define qué valores puede tomar cada setting), salvo la
    /// `description` de cada clave (cosmética, igual que
    /// [`Manifest::description`]).
    pub config: BTreeMap<String, ConfigKeySpec>,
}

/// Error al cargar un manifiesto — o, más ampliamente, la razón por la que un
/// candidato a plugin queda excluido del catálogo (mismo tipo que
/// [`crate::LoadError::error`]): además del parseo/validación de
/// `plugin.toml` en sí, cubre condiciones de nivel catálogo como
/// `DuplicateId` y, desde P2, `ConfigValues` (un `config.toml` de VALORES
/// que no valida contra el esquema `[config]` — el plugin entero se excluye,
/// no solo la clave ofensora).
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// El TOML no parsea o tiene claves desconocidas.
    #[error("plugin.toml inválido: {0}")]
    Toml(#[from] toml::de::Error),
    /// `plugin.id` vacío o no reverse-DNS (sin `.`).
    #[error("plugin.id inválido: se espera reverse-DNS (p. ej. `org.foo.bar`)")]
    Id,
    /// `exec` distinto de `none`: PROHIBIDO (spec §7.1, invariante dura).
    #[error("capability `exec` prohibida: debe ser `none` (o ausente)")]
    ExecForbidden,
    /// `location-root-marker` que no es UN nombre: vacío, con separador, con
    /// NUL, `.`/`..`, o absurdamente largo. Un marcador con una barra dentro
    /// haría que el host buscase por una RUTA subiendo, que es otra capacidad.
    #[error("`location-root-marker` debe ser un nombre simple (sin `/`, sin NUL, no `.`/`..`)")]
    LocationMarker,
    /// `location-root-marker` declarado SIN `location = "read"`: pediría abrir
    /// un ancestro sin pedir la capacidad que lo lee. Se rechaza en vez de
    /// ignorarse, para que el autor se entere.
    #[error(
        "`location-root-marker` sin `location = \"read\"`: declara la capacidad o quita el marcador"
    )]
    LocationMarkerWithoutCap,
    /// `[[contributions.hook]].on` con un valor fuera de [`HOOK_EVENTS`]. El
    /// vocabulario es cerrado a propósito: un `before-copy` que se aceptara
    /// instalaría un hook que nunca dispara, y su autor se enteraría porque
    /// nunca pasa nada. Lleva el valor para que el error sea accionable.
    #[error(
        "evento de hook desconocido `{0}`: los que existen son after-created, after-removed, after-trashed, after-renamed y after-mode-changed"
    )]
    HookUnknownEvent(String),
    /// `category = "hook"` sin ningún `[[contributions.hook]]`: un plugin que
    /// dice observar y no escucha nada es inerte, y el gestor lo pintaría
    /// como uno normal.
    #[error(
        "`category = \"hook\"` sin ningún `[[contributions.hook]]`: declara qué eventos escucha"
    )]
    HookWithoutEvents,
    /// `[[contributions.hook]]` en un plugin de otra categoría: solo los de
    /// `category = "hook"` se despachan, así que esos eventos no sonarían
    /// nunca — el plugin inerte que el gestor pintaría como uno normal.
    #[error(
        "`[[contributions.hook]]` requiere `category = \"hook\"`: un plugin de otra categoría no recibe eventos"
    )]
    HookOnOtherCategory,
    /// `category = "hook"` con `net`: un hook recibe la ruta de cada mutación
    /// de la máquina, y con red sería un canal para sacarlas fuera. Hasta que
    /// un ADR diga qué badge lo dice, se rechaza (ADR 0100).
    #[error(
        "un `hook` no puede declarar `net`: recibe la ruta de cada mutación, y con red eso es un canal de salida (ADR 0100)"
    )]
    HookWithNet,
    /// `fs-write = "scoped"` (o cualquier cadena): un valor RESERVADO que
    /// ninguna puerta del host honraba y que desde ADR 0088 se rechaza en vez
    /// de aprobarse en vano. Lo que existe es `fs-write = { sidecar = [...] }`
    /// (ADR 0101).
    #[error(
        "`fs-write = \"{0}\"` no existe: la escritura de un plugin es `fs-write = {{ sidecar = [\"nombre\"] }}`, y solo para un `hook` (ADR 0101)"
    )]
    FsWriteReserved(String),
    /// `fs-write = { sidecar = [...] }` en un plugin que no es `hook`: solo un
    /// hook tiene un evento junto al que escribir, así que en otra categoría
    /// sería una capacidad aprobada que nadie usa (ADR 0088).
    #[error("`fs-write` con sidecars solo lo puede declarar un `hook` (ADR 0101)")]
    SidecarNotForCategory,
    /// Un nombre de sidecar que no es UN nombre de fichero portable: ASCII
    /// imprimible sin `/ \ : * ? " < > |`, ni `.`/`..`, ni nombre reservado de
    /// Windows, ni punto final; o repetido. ASCII a propósito: el nombre es
    /// lo que el humano lee en el badge de aprobación y lo que acaba en
    /// disco, y un carácter bidi o invisible ahí es una suplantación. Lleva
    /// el valor para que sea accionable.
    #[error(
        "nombre de sidecar inválido `{0}`: ASCII imprimible, sin `/ \\ : * ? \" < > |`, ni `.`/`..`, ni nombre reservado, ni repetido"
    )]
    SidecarName(String),
    /// `fs-write = {{ sidecar = [...] }}` vacío o con más de
    /// [`SIDECAR_MAX_NAMES`] nombres.
    #[error("`fs-write.sidecar` lleva {got} nombres: entre 1 y {SIDECAR_MAX_NAMES}")]
    SidecarListSize {
        /// Cuántos traía.
        got: usize,
    },
    /// `capabilities.ai` declarada cuando NADA la honra: no hay interfaz WIT
    /// de IA ni sitio en el host que la linke. Se parseaba, entraba en el
    /// digest y pintaba insignia, así que un humano aprobaba «acceso a IA» y
    /// concedía nada — la capability declarada que nadie honra (ADR 0088).
    /// Se rechaza al parsear y el campo se queda porque spec §7.1 lo nombra;
    /// los hooks tuvieron el mismo rechazo hasta ADR 0100.
    #[error(
        "la capability `ai` aún no está implementada: no hay interfaz WIT que la sirva, \
         así que declararla aprobaría un permiso que no concede nada"
    )]
    AiNotImplemented,
    /// Un `[[contributions.provider]]` reclama un scheme que no puede servir:
    /// uno del core ([`CORE_SCHEMES`]), un formato de archivo o un scheme con
    /// `+` (composición, ADR 0018), o algo que no es un scheme (charset de
    /// [`norte_proto::Scheme`]). Un plugin que sirviera `sftp://` se pondría
    /// delante de un provider con papelera, reanudación y TLS, y uno que
    /// sirviera `ftp://` recibiría las contraseñas FTP guardadas; el humano
    /// que aprueba no vería la diferencia.
    #[error(
        "contributions.provider[].scheme reservado o inválido: `file`, `sftp`, `ftp`, `s3` y \
         los formatos de archivo los sirve el core, y el scheme debe ser `[a-z][a-z0-9.-]*`"
    )]
    ReservedScheme,
    /// El `plugin.wasm` se compiló contra una versión de un paquete WIT que
    /// este host sirve a OTRA (ADR 0094). No es un error del manifiesto,
    /// pero es la causa por la que el catálogo no carga el plugin, y
    /// [`crate::LoadError`] lleva una de estas: se lista como roto con las
    /// dos versiones a la vista en vez de morir en wasmtime nombrando una
    /// interfaz. El estado (aprobación) no se toca; un binario recompilado
    /// es otro binario y se vuelve a aprobar (#241).
    #[error(
        "compilado contra `{package}@{built_against}`, este norte sirve `@{served}`: \
         recompila el plugin contra el WIT actual"
    )]
    WitMismatch {
        /// El paquete (`norte:plugin`).
        package: String,
        /// La versión que el binario referencia.
        built_against: String,
        /// La que este host sirve.
        served: String,
    },
    /// El `plugin.wasm` supera el tope de artefacto
    /// ([`crate::MAX_ARTIFACT_BYTES`]) y el catálogo NO lo lee: leerlo para
    /// hashearlo y leer sus imports materializaría en memoria lo que un
    /// tercero decidió, en cada descubrimiento, y un fallo ahí tumba el
    /// catálogo entero y no un plugin. El runtime aplica el mismo tope al
    /// instanciar; este es el mismo tope, una puerta antes.
    #[error("plugin.wasm mide {len} bytes y el tope es {cap}: no se lee")]
    ArtifactTooLarge {
        /// Bytes del fichero.
        len: u64,
        /// El tope.
        cap: u64,
    },
    /// Dos o más directorios declaran el MISMO `plugin.id` (issue #69): se
    /// rechazan TODOS (fail-closed). Un segundo directorio no puede reclamar el
    /// id de un plugin aprobado para colar su propio `plugin.wasm`.
    #[error(
        "id duplicado: `{0}` aparece en más de un directorio de plugins (rechazado por seguridad)"
    )]
    DuplicateId(String),
    /// `plugin.description` supera el tope de 280 caracteres (P1). Cosmética
    /// pero fail-loud, como `id`: evita manifiestos que abulten logs/UI o que
    /// intenten esconder texto fuera de la vista truncada del frontend.
    #[error("plugin.description excede el tope de 280 caracteres")]
    DescriptionTooLong,
    /// `contributions.command[].title` supera el tope de 120 caracteres (P1
    /// encoding audit M2). A diferencia de `description`, `title` SÍ entra en
    /// `approval_digest` (decide cuándo/cómo se dispara el comando en la
    /// palette) — el tope es solo de PARSEO: un manifiesto ya aprobado con un
    /// título corto no se ve afectado si el tope cambia en una versión
    /// futura del host, porque eso solo rechaza manifiestos NUEVOS, nunca
    /// reinterpreta uno viejo.
    #[error("contributions.command[].title excede el tope de 120 caracteres")]
    CommandTitleTooLong,
    /// `contributions.command[].id` supera el tope de 64 caracteres (P1
    /// encoding audit M2). Mismo criterio que `CommandTitleTooLong`: cap de
    /// PARSEO, no reinterpreta aprobaciones existentes.
    #[error("contributions.command[].id excede el tope de 64 caracteres")]
    CommandIdTooLong,
    /// `[contributions]` declara más de [`COMMAND_MAX_COUNT`] comandos (#281).
    #[error("contributions declara más comandos de los permitidos (tope: {COMMAND_MAX_COUNT})")]
    TooManyCommands,
    /// `[config]` declara más de [`CONFIG_MAX_KEYS`] claves (P2 decisión 1).
    #[error("[config] declara más claves de las permitidas (tope: {CONFIG_MAX_KEYS})")]
    ConfigTooManyKeys,
    /// Una clave `[config.<key>]` no respeta el charset `[a-z0-9-]{1,32}` (P2
    /// decisión 1). La clave literal NO se interpola en el mensaje (mismo
    /// criterio que `Id`): una clave hostil no debe llegar a logs/UI vía el
    /// texto del error.
    #[error("clave de [config] inválida: se espera el charset `[a-z0-9-]{{1,32}}`")]
    ConfigKeyCharset,
    /// `[config.<key>].description` supera el tope de
    /// [`CONFIG_DESCRIPTION_MAX_CHARS`] caracteres (mismo criterio que
    /// `plugin.description`).
    #[error(
        "[config.<key>].description excede el tope de {CONFIG_DESCRIPTION_MAX_CHARS} caracteres"
    )]
    ConfigDescriptionTooLong,
    /// `[config.<key>].default` de tipo `string` supera el tope de
    /// [`CONFIG_STRING_MAX_CHARS`] caracteres (P2 decisión 1).
    #[error(
        "[config.<key>].default (string) excede el tope de {CONFIG_STRING_MAX_CHARS} caracteres"
    )]
    ConfigDefaultTooLong,
    /// `[config.<key>].default` de tipo `int` cae fuera de `[min, max]`
    /// declarados (P2 decisión 1: los defaults DEBEN validar contra su propio
    /// tipo/rango al parsear).
    #[error("[config.<key>].default (int) cae fuera del rango [min, max] declarado")]
    ConfigIntDefaultOutOfRange,
    /// `[config.<key>].values` (enum) supera [`CONFIG_ENUM_MAX_VALUES`]
    /// entradas (P2 decisión 1).
    #[error("[config.<key>].values (enum) excede el tope de {CONFIG_ENUM_MAX_VALUES} entradas")]
    ConfigEnumTooManyValues,
    /// Una entrada de `[config.<key>].values` (enum) supera el tope de
    /// [`CONFIG_STRING_MAX_CHARS`] caracteres (P2 decisión 1).
    #[error(
        "[config.<key>].values (enum) contiene una entrada que excede el tope de {CONFIG_STRING_MAX_CHARS} caracteres"
    )]
    ConfigEnumValueTooLong,
    /// `[config.<key>].default` de tipo `enum` no está entre `values` (P2
    /// decisión 1: los defaults DEBEN validar contra su propio tipo/rango al
    /// parsear).
    #[error("[config.<key>].default (enum) no está entre los `values` declarados")]
    ConfigEnumDefaultNotInValues,
    /// El `config.toml` de VALORES (P2 decisión 3, distinto del manifiesto)
    /// no valida contra el esquema `[config]` — fail-closed a nivel de
    /// catálogo: el plugin ENTERO se excluye (mismo criterio que
    /// `DuplicateId`), nunca carga con valores a medias.
    #[error("config.toml inválido: {0}")]
    ConfigValues(#[from] crate::config_values::ConfigValueError),
}

/// Tope de `contributions.command[].title` (P1 encoding audit M2): mismo
/// espíritu que el tope de `description` — un plugin hostil no debe poder
/// abultar la palette con un título kilométrico. `title` SÍ entra en
/// `approval_digest` (ver doc de [`ManifestError::CommandTitleTooLong`]).
pub const COMMAND_TITLE_MAX_CHARS: usize = 120;

/// Tope de `contributions.command[].id` (P1 encoding audit M2).
pub const COMMAND_ID_MAX_CHARS: usize = 64;

/// Tope de CUÁNTOS comandos declara un manifiesto (#281), del mismo tamaño y
/// por el mismo motivo que [`CONFIG_MAX_KEYS`]: cada comando aprobado se
/// convierte en una fila de paleta en cada cliente
/// (`norte_frontend::palette::plugin_rows`), y el sitio donde eso se corta de
/// raíz es la validación del manifiesto, no cada paleta.
///
/// Como los otros topes de `[[command]]`, es un cap de PARSEO: rechaza
/// manifiestos NUEVOS, nunca reinterpreta una aprobación ya concedida.
pub const COMMAND_MAX_COUNT: usize = 32;

/// El alfabeto de un id de plugin, definido junto al tipo del wire que lo
/// transporta ([`norte_proto::methods::is_valid_plugin_id`]).
///
/// Se re-exporta con el nombre de siempre porque la pregunta es UNA y tiene
/// dos entradas: este crate la hace al parsear un `plugin.toml`, y todo el que
/// recibe un `PluginInfo` por el wire la hace otra vez. Dos implementaciones
/// del mismo alfabeto acabarían con una más laxa que la otra. `norte-core` lo
/// re-exporta a su vez para los frontends que no dependen de este crate.
pub use norte_proto::methods::is_valid_plugin_id;

/// Schemes que sirve el core y que un provider plugin NO puede reclamar.
///
/// `ftp` está, aunque su guest sea WASM: un plugin que lo reclamase recibiría
/// por `configure` la contraseña de cada conexión `ftp://` guardada, y la
/// pantalla de aprobación no enseñaba el scheme. El día que el guest embebido
/// se distribuya como plugin, `ftp` sale de aquí en ese mismo commit.
pub const CORE_SCHEMES: &[&str] = &["file", "sftp", "ftp", "s3"];

/// `true` si un `[[contributions.provider]]` puede declarar `scheme`: es un
/// scheme válido para un [`norte_proto::VPath`], no es de
/// [`CORE_SCHEMES`], no es un formato de archivo y no lleva `+`, el operador
/// de composición de ADR 0018 (`zip+sftp`).
///
/// ```
/// use norte_plugin_host::scheme_claimable;
/// assert!(scheme_claimable("webdav"));
/// assert!(!scheme_claimable("ftp"));
/// assert!(!scheme_claimable("sftp"));
/// assert!(!scheme_claimable("zip+sftp"));
/// assert!(!scheme_claimable("Web-DAV"));
/// ```
#[must_use]
pub fn scheme_claimable(scheme: &str) -> bool {
    norte_proto::Scheme::new(scheme).is_ok()
        && !scheme.contains('+')
        && !CORE_SCHEMES.contains(&scheme)
        && !norte_proto::ARCHIVE_FORMATS.contains(&scheme)
}

/// Lo que un manifiesto declara sobre hooks y sidecars (ADR 0100, ADR 0101),
/// validado aparte de [`Manifest::from_toml`] para que la lista de
/// comprobaciones del manifiesto no desborde el límite de líneas.
fn validate_hooks_and_sidecars(raw: &ManifestRaw) -> Result<(), ManifestError> {
    // Hooks (ADR 0100): cada evento del vocabulario cerrado, y un plugin
    // que se declara hook escucha al menos uno. DESPUÉS del id a
    // propósito: un manifiesto cuyo id no es de fiar se rechaza por el
    // id, que es lo accionable.
    if let Some(h) = raw
        .contributions
        .hook
        .iter()
        .find(|h| !HOOK_EVENTS.contains(&h.on.as_str()))
    {
        return Err(ManifestError::HookUnknownEvent(h.on.clone()));
    }
    if raw.plugin.category == Category::Hook && raw.contributions.hook.is_empty() {
        return Err(ManifestError::HookWithoutEvents);
    }
    if raw.plugin.category != Category::Hook && !raw.contributions.hook.is_empty() {
        return Err(ManifestError::HookOnOtherCategory);
    }
    if raw.plugin.category == Category::Hook && raw.capabilities.net.is_some() {
        return Err(ManifestError::HookWithNet);
    }
    // `fs-write` (ADR 0101): solo sidecars, solo en hooks, nombres de
    // verdad. Un `"scoped"` heredado se rechaza con lo que hay que poner.
    match &raw.capabilities.fs_write {
        crate::capability::FsWriteCap::None => {}
        crate::capability::FsWriteCap::Reserved(s) => {
            return Err(ManifestError::FsWriteReserved(s.clone()));
        }
        crate::capability::FsWriteCap::Sidecar(l) => {
            let sidecar = &l.sidecar;
            if raw.plugin.category != Category::Hook {
                return Err(ManifestError::SidecarNotForCategory);
            }
            if sidecar.is_empty() || sidecar.len() > SIDECAR_MAX_NAMES {
                return Err(ManifestError::SidecarListSize { got: sidecar.len() });
            }
            for (i, n) in sidecar.iter().enumerate() {
                if !is_valid_sidecar_name(n) || sidecar[..i].contains(n) {
                    return Err(ManifestError::SidecarName(n.clone()));
                }
            }
        }
    }
    Ok(())
}

impl Manifest {
    /// Parsea y VALIDA un `plugin.toml`.
    ///
    /// # Errors
    /// [`ManifestError`] si el TOML no parsea, el `id` no es reverse-DNS, o se
    /// declara `exec` distinto de `none` (prohibido sin excepción).
    pub fn from_toml(src: &str) -> Result<Self, ManifestError> {
        let mut raw: ManifestRaw = toml::from_str(src)?;
        // `fs-write = "none"` es la forma explícita de «sin escritura» que
        // ADR 0022 documenta: vale lo mismo que ausente, y digesta igual
        // (byte 0), así que ninguna aprobación se mueve. Cualquier OTRA cadena
        // se rechaza en la validación de abajo.
        if matches!(&raw.capabilities.fs_write, crate::capability::FsWriteCap::Reserved(s) if s == "none")
        {
            raw.capabilities.fs_write = crate::capability::FsWriteCap::None;
        }
        // Invariante dura: exec SIEMPRE none.
        if raw
            .capabilities
            .exec
            .as_deref()
            .is_some_and(|e| e != "none")
        {
            return Err(ManifestError::ExecForbidden);
        }
        // El marcador de raíz es UN nombre, jamás una ruta: con una barra
        // dentro el host estaría subiendo por un camino elegido por el plugin,
        // que es una capacidad distinta de la que se aprueba.
        if let Some(marker) = raw.capabilities.location_root_marker.as_deref() {
            if !raw.capabilities.location.granted() {
                return Err(ManifestError::LocationMarkerWithoutCap);
            }
            let malo = marker.is_empty()
                || marker.len() > 64
                || marker.contains('/')
                || marker.contains('\\')
                || marker.contains('\0')
                || marker == "."
                || marker == "..";
            if malo {
                return Err(ManifestError::LocationMarker);
            }
        }
        // id reverse-DNS REAL: uno o más segmentos `[A-Za-z0-9-]+` separados por
        // puntos, con al menos un punto, sin segmento vacío (ni punto inicial/
        // final), longitud total 1..=128. Endurecido más allá de "contiene un
        // punto" porque el id crudo del manifiesto termina en logs y en el modal
        // de aprobación (T5): un id con saltos de línea, comillas o espacios
        // permitiría inyección en el log o spoofing del diálogo de consentimiento.
        if !is_valid_plugin_id(&raw.plugin.id) {
            return Err(ManifestError::Id);
        }
        validate_hooks_and_sidecars(&raw)?;
        // `ai`: una promesa que nadie cumple.
        // Se mira la PRESENCIA, no el valor: cualquier modo sería igual de
        // inerte.
        if raw.capabilities.ai.is_some() {
            return Err(ManifestError::AiNotImplemented);
        }
        // Un provider sirve el scheme que declara, así que el scheme es un
        // nombre que se puede suplantar: los del core y los de archivo no se
        // ceden, y lo que no es un scheme no llega al connector.
        if raw
            .contributions
            .provider
            .iter()
            .any(|c| !scheme_claimable(&c.scheme))
        {
            return Err(ManifestError::ReservedScheme);
        }
        // Tope de 280 CARACTERES (no bytes: un idioma no-ASCII no debe pagar
        // el tope antes de tiempo). Cosmética pero fail-loud, como `id`.
        if raw
            .plugin
            .description
            .as_deref()
            .is_some_and(|d| d.chars().count() > 280)
        {
            return Err(ManifestError::DescriptionTooLong);
        }
        // Topes de cada comando declarado (P1 encoding audit M2), simétricos
        // con el de `description` — CHARS, no bytes. `id` primero: es el que
        // viaja al wire para despachar (`plugin.run_command`), acotarlo
        // primero da el error más específico si AMBOS desbordan a la vez.
        // Cuántos, antes de cuánto mide cada uno: con mil comandos el error
        // útil es «son demasiados», no el `id` largo del número 400.
        if raw.contributions.command.len() > COMMAND_MAX_COUNT {
            return Err(ManifestError::TooManyCommands);
        }
        for c in &raw.contributions.command {
            if c.id.chars().count() > COMMAND_ID_MAX_CHARS {
                return Err(ManifestError::CommandIdTooLong);
            }
            if c.title.chars().count() > COMMAND_TITLE_MAX_CHARS {
                return Err(ManifestError::CommandTitleTooLong);
            }
        }
        // `[config]` (P2 decisión 1): tope de claves primero (fail-fast antes
        // de validar cada entrada), luego charset + topes de tipo por clave,
        // en orden de `BTreeMap` (determinista).
        if raw.config.len() > CONFIG_MAX_KEYS {
            return Err(ManifestError::ConfigTooManyKeys);
        }
        let mut config = BTreeMap::new();
        for (key, entry) in raw.config {
            if !is_valid_config_key(&key) {
                return Err(ManifestError::ConfigKeyCharset);
            }
            config.insert(key, validate_config_entry(entry)?);
        }
        Ok(Self {
            id: raw.plugin.id,
            name: raw.plugin.name,
            publisher: raw.plugin.publisher,
            version: raw.plugin.version,
            category: raw.plugin.category,
            description: raw.plugin.description,
            contributions: raw.contributions,
            capabilities: raw.capabilities,
            config,
        })
    }

    /// Digest hex (sha256) de la forma CANÓNICA del manifiesto para ANCLAR la
    /// aprobación del humano (issue #69, defensa confused-deputy TOCTOU). Cubre
    /// no solo las `[capabilities]` (lo que el host hace cumplir) sino también la
    /// `category` y las `contributions` — los campos que deciden CUÁNDO y CÓMO se
    /// dispara el plugin (mimetypes, ids de comando, schemes…). Así, un
    /// `plugin.toml` reeditado que cambie de `command` a `previewer`, o que
    /// amplíe los mimetypes, MANTENIENDO las mismas capabilities, deja de casar el
    /// digest y fuerza re-consentimiento (si no, pasaría a auto-ejecutarse en el
    /// viewer sin que el humano lo aprobara para eso).
    ///
    /// La forma es determinista y no ambigua (tags de enum estables, cadenas
    /// longitud-prefijadas, hosts de red como conjunto ordenado y deduplicado).
    /// El id y el nombre/publisher/versión NO entran: la aprobación se indexa por
    /// id (cambiarlo es otro plugin) y el resto es cosmético — lo que importa para
    /// la seguridad es qué hace y cuándo se dispara.
    ///
    /// P2 extiende la forma canónica con una sección `config:` — pero SOLO
    /// cuando `[config]` declara alguna clave: `update_config_digest` no
    /// añade ni un byte si `self.config` está vacío, así que un manifiesto sin
    /// `[config]` digesta EXACTAMENTE igual que antes de P2 (las aprobaciones
    /// humanas existentes de plugins que no usan `[config]` no se resetean).
    #[must_use]
    pub fn approval_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        // Prefijo de dominio + versión del esquema: si cambia la forma canónica,
        // los digests viejos no colisionan con los nuevos.
        h.update(b"norte-plugin-manifest:v1\n");
        h.update([self.category.digest_tag()]);
        self.contributions.update_digest(&mut h);
        self.capabilities.update_digest(&mut h);
        update_config_digest(&self.config, &mut h);
        // ADR 0037: sección OPCIONAL igual que `config:` — ver su rustdoc.
        update_decorator_digest(&self.contributions.decorator, &mut h);
        crate::capability::hex_lower(&h.finalize())
    }
}
