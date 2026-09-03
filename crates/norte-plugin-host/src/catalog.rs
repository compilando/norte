//! Catálogo de plugins (ADR 0022 D5/D6): descubre los `.wasm` locales y sus
//! manifiestos, y los ordena por categoría para el gestor de extensiones.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::config_values::resolve_settings;
use crate::manifest::{Category, Manifest, ManifestError};

/// Nivel de una acción invocable (ADR 0022 D5): los tres NUNCA se mezclan. El
/// catálogo cubre `Plugin`; `Script` (Lua) y `BuiltIn` (comandos nativos) los
/// aporta la UI desde sus propias fuentes y los muestra en grupos aparte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// WASM de terceros, sandbox por capabilities.
    Plugin,
    /// Lua del usuario, permisos del usuario (no sandbox).
    Script,
    /// Comando nativo del binario.
    BuiltIn,
}

/// Un plugin instalado: su manifiesto + estado local.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Manifiesto validado.
    pub manifest: Manifest,
    /// Directorio del plugin (`~/.config/norte/plugins/<id>/`).
    pub dir: PathBuf,
    /// Activado por el usuario.
    pub enabled: bool,
    /// El usuario aprobó las capabilities declaradas (si no, `⚠ sin aprobar`
    /// y el host no lo carga — ADR 0022 D4).
    pub approved: bool,
    /// Valores EFECTIVOS de `[config]` (P2 decisión 3): defaults del esquema
    /// del manifiesto, con `dir/config.toml` superpuesto y ya validado —
    /// [`crate::resolve_settings`] corre en `load_dir` y, si falla, el
    /// plugin va a `errors` en vez de aquí (ver [`Catalog::load_dir`]).
    /// Codificación canónica de string (decisión 4). Vacío si el manifiesto
    /// no declara `[config]`.
    pub settings: BTreeMap<String, String>,
    /// Qué hay en `<dir>/help.md` al descubrir (H3e).
    pub help: HelpPresence,
    /// Sha256 hex de `<dir>/plugin.wasm` al DESCUBRIR, o `None` si no hay
    /// binario que hashear (#241).
    ///
    /// Entra en [`Self::approval_anchor`], y ese es todo su motivo: el digest
    /// del manifiesto cierra el TOCTOU de aprobar↔ejecutar por el lado del
    /// `plugin.toml`, y dejaba el otro lado abierto de par en par. Quien
    /// pudiera cambiar el `.wasm` sin tocar el `.toml` —un instalador, un
    /// paquete comprometido, cualquier proceso del usuario— se quedaba con
    /// las capacidades que un humano aprobó para OTRO código.
    ///
    /// Se calcula UNA vez, al descubrir, y no en cada llamada: la aprobación
    /// se comprueba por cada previsualización y por cada página de columnas,
    /// y leer megabytes ahí sería pagar el hash en el bucle de pintado.
    pub wasm_digest: Option<String>,
    /// Los paquetes `norte:*` que el binario nombra, con su versión, leídos
    /// del `.wasm` al descubrir (ADR 0094, [`crate::wit_packages`]). Vacío
    /// sin binario. Un plugin que llega aquí NO tiene mismatch: el que lo
    /// tiene va a [`Catalog::errors`] con [`ManifestError::WitMismatch`].
    pub wit: Vec<(String, String)>,
}

impl PluginEntry {
    /// El ancla de una aprobación humana: manifiesto **y** binario (#241).
    ///
    /// [`Manifest::approval_digest`] contesta «¿sigue pidiendo lo mismo, y
    /// disparándose igual?». Le faltaba la otra mitad: «¿sigue siendo el mismo
    /// código?». Sin ella, cambiar `plugin.wasm` y dejar el `plugin.toml`
    /// quieto conservaba la aprobación — que es justo el confused-deputy que
    /// el digest existe para cerrar, entrando por la otra puerta del bundle.
    ///
    /// Un plugin sin binario ancla solo el manifiesto: no hay código que
    /// pueda cambiar sin que se note, porque no hay código.
    ///
    /// **Subir esto invalida todas las aprobaciones ya dadas**, y es
    /// deliberado: la pregunta que el humano contestó no incluía «y este
    /// binario», así que su respuesta no cubre lo que se le está preguntando
    /// ahora.
    #[must_use]
    pub fn approval_anchor(&self) -> String {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(
            b"norte-plugin-approval:v2
",
        );
        let manifiesto = self.manifest.approval_digest();
        h.update((manifiesto.len() as u64).to_le_bytes());
        h.update(manifiesto.as_bytes());
        match &self.wasm_digest {
            None => h.update([0u8]),
            Some(d) => {
                h.update([1u8]);
                h.update((d.len() as u64).to_le_bytes());
                h.update(d.as_bytes());
            }
        }
        crate::capability::hex_lower(&h.finalize())
    }
}

/// Qué encontró el descubrimiento en `<dir>/help.md` (H3e).
///
/// Un TRI-ESTADO y no dos banderas: "pasa la guarda" implica "existe", nunca al
/// revés, así que dos bools tendrían una cuarta combinación imposible
/// (verificado y ausente) que alguien acabaría construyendo.
///
/// Se resuelve AQUÍ, al descubrir, y no en `plugin.list`: ese listado corre en
/// el reactor async y bajo el lock global de plugins, así que las tres llamadas
/// al sistema de la guarda (un `is_file` y dos `canonicalize`) por plugin
/// bloquearían a todas las demás conexiones sobre un directorio que puede estar
/// en autofs o NFS, y `plugin.list` está ABIERTO a un agente. El descubrimiento
/// ya es I/O y ya corre fuera del reactor, así que este es su sitio — como
/// `name`, `commands` o `capabilities`, que también son instantáneas del momento
/// de descubrir.
///
/// NUNCA es una lectura del contenido: el catálogo se recorre entero en cada
/// `plugin.list` (el registro es efímero por llamada), y leer 64 KiB por plugin
/// ahí pagaría el contenido en cada listado para decidir si se pinta un nodo en
/// una barra lateral. El contenido se lee bajo demanda, en `plugin.help`.
///
/// Es una PISTA cacheada, y por eso puede quedar rancia sin consecuencias: quien
/// sirve el contenido vuelve a aplicar la guarda al leer, así que un
/// [`Self::Servable`] rancio no entrega nada de fuera. Lo único que revela es
/// que al descubrir había un fichero regular no-escapado en la ruta FIJA
/// `<dir>/help.md`, que no la elige quien pregunta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpPresence {
    /// No hay `help.md`.
    Absent,
    /// Hay un `help.md` pero el host NO lo servirá: no pasa la guarda de escape
    /// ([`verified_child`]) — un symlink que sale del directorio del plugin, o
    /// uno roto. Existe para el DIAGNÓSTICO (`norte doctor` lo reporta), nunca
    /// para el wire: distinguirlo ahí sería un oráculo de rutas.
    Unservable,
    /// Hay un `help.md` y pasa la guarda. Es el estado que cruza el wire como
    /// `PluginInfo::has_help`.
    Servable,
}

impl HelpPresence {
    /// ¿El autor puso un `help.md`, lo sirvamos o no? La pregunta LAXA, la del
    /// diagnóstico.
    #[must_use]
    pub fn is_present(self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// ¿Hay una página que el host vaya a servir? La pregunta ESTRICTA, la que
    /// cruza el wire.
    #[must_use]
    pub fn is_servable(self) -> bool {
        matches!(self, Self::Servable)
    }
}

/// Los bytes de `<dir>/plugin.wasm`, por la misma guarda con la que se
/// ejecuta ([`verified_child`]) — hashear un fichero y ejecutar otro sería
/// peor que no hashear.
///
/// `None` cuando no hay binario servible: un plugin sin `.wasm` no ejecuta
/// nada, así que no hay código que anclar.
fn read_wasm(dir: &Path) -> Option<Vec<u8>> {
    let path = verified_child(dir, "plugin.wasm")?;
    std::fs::read(path).ok()
}

/// El digest de un binario tal como el catálogo lo ancla en
/// [`PluginEntry::wasm_digest`]: sha256 en hex minúscula. Público para que
/// quien vaya a INSTANCIAR unos bytes pueda comprobar que son los que el
/// humano aprobó, en vez de confiar en que la ruta no cambió entre el
/// descubrimiento y la carga.
///
/// ```
/// use norte_plugin_host::wasm_digest_of;
/// assert_eq!(wasm_digest_of(b"").len(), 64);
/// assert_ne!(wasm_digest_of(b"a"), wasm_digest_of(b"b"));
/// ```
#[must_use]
pub fn wasm_digest_of(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    crate::capability::hex_lower(&h.finalize())
}

/// Resuelve el tri-estado de `<dir>/help.md` (H3e). El `is_file` LAXO sigue
/// enlaces a propósito —presencia, no permiso—, y la guarda decide si además es
/// servible.
fn help_presence(dir: &Path) -> HelpPresence {
    if verified_child(dir, "help.md").is_some() {
        HelpPresence::Servable
    } else if dir.join("help.md").is_file() {
        HelpPresence::Unservable
    } else {
        HelpPresence::Absent
    }
}

/// `<dir>/<name>` canonicalizado, SOLO si el fichero real cae DENTRO de `dir`.
///
/// La forma común del guard de issue #69: un fichero que el host lee o ejecuta
/// desde el directorio de un plugin no puede resolver fuera de él POR SYMLINK.
/// `None` si no existe, no es fichero, no canonicaliza (enlace roto) o escapa.
///
/// Vive aquí, y no en `norte-core` junto a sus llamadores, para que exista UNA
/// sola implementación: el catálogo necesita el veredicto al descubrir (ver
/// [`PluginEntry::help`]) y `norte-core` lo necesita al leer o
/// ejecutar. Copiar la guarda para evitar la dependencia sería mucho peor que
/// tenerla aquí — dos copias de un guard de seguridad divergen.
///
/// # Qué NO cubre (dicho, no insinuado)
///
/// - **Solo symlinks.** Un HARDLINK no tiene ruta de destino: `<dir>/x`
///   canonicaliza a sí mismo y pasa la guarda aunque su inodo sea el de
///   `~/.ssh/id_ed25519`. Un bind mount igual. Ninguna guarda BASADA EN RUTAS
///   puede verlos, así que "no puede escapar de su directorio" es más fuerte de
///   lo que esto entrega: lo que entrega es "no puede escapar por symlink".
/// - **Es una observación PUNTUAL, no un handle.** Devolver la ruta canónica
///   evita re-resolver los componentes INTERMEDIOS, pero el kernel resuelve la
///   ruta entera en cada `open`, componente final incluido: quien pueda escribir
///   en el directorio puede cambiar ese último componente entre el chequeo y la
///   apertura (TOCTOU). Una ruta canónica no congela nada. Está FUERA del modelo
///   de amenaza —quien escribe ahí ya puede reemplazar el bundle entero, misma
///   frontera de confianza— pero se dice en vez de darse por resuelto.
///
/// `O_NOFOLLOW` cerraría esa carrera del componente final y se DESCARTA a
/// sabiendas: también prohibiría un symlink INTERNO al directorio, que un plugin
/// organizando sus propios ficheros con enlaces usa legítimamente (hay un test
/// que lo fija). No re-litigar sin ese caso a mano.
///
/// ```
/// use norte_plugin_host::verified_child;
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("help.md"), "hola").unwrap();
/// assert!(verified_child(dir.path(), "help.md").is_some());
/// assert!(verified_child(dir.path(), "ausente.md").is_none());
/// ```
#[must_use]
pub fn verified_child(dir: &Path, name: &str) -> Option<PathBuf> {
    let child = dir.join(name);
    if !child.is_file() {
        return None;
    }
    let canon_child = child.canonicalize().ok()?;
    let canon_dir = dir.canonicalize().ok()?;
    canon_child.starts_with(&canon_dir).then_some(canon_child)
}

/// Un manifiesto que no cargó, con su causa (para avisar en el gestor en vez de
/// desaparecer en silencio).
#[derive(Debug)]
pub struct LoadError {
    /// Directorio culpable.
    pub dir: PathBuf,
    /// La causa.
    pub error: ManifestError,
}

/// El catálogo de plugins descubiertos + los que fallaron al cargar.
#[derive(Debug, Default)]
pub struct Catalog {
    /// Plugins válidos, ordenados por categoría y luego por id.
    pub plugins: Vec<PluginEntry>,
    /// Manifiestos inválidos (se muestran como error, no se ocultan).
    pub errors: Vec<LoadError>,
}

impl Catalog {
    /// Descubre plugins en `root/<id>/plugin.toml`. No es I/O async: el host lo
    /// llama una vez al arrancar (o vía `spawn_blocking` desde contexto async).
    /// Un `root` inexistente = catálogo vacío (no es error).
    #[must_use]
    pub fn load_dir(root: &Path) -> Catalog {
        let mut cat = Catalog::default();
        let Ok(entries) = std::fs::read_dir(root) else {
            return cat;
        };
        // Se recogen los manifiestos válidos aparte para poder DEDUPLICAR por id
        // antes de aceptarlos: dos directorios con el mismo `plugin.id` son un
        // vector de confused-deputy (issue #69) — el segundo podría reclamar la
        // aprobación del primero. Se rechazan TODOS los colisionantes (fail-closed).
        let mut parsed: Vec<(Manifest, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let toml_path = dir.join("plugin.toml");
            let Ok(src) = std::fs::read_to_string(&toml_path) else {
                continue; // sin plugin.toml no es un plugin (no es error)
            };
            match Manifest::from_toml(&src) {
                Ok(manifest) => parsed.push((manifest, dir)),
                Err(error) => cat.errors.push(LoadError { dir, error }),
            }
        }
        // Cuenta de ocurrencias por id: un id que aparece más de una vez es
        // ambiguo y se rechaza en todos sus directorios.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (manifest, _) in &parsed {
            *counts.entry(manifest.id.clone()).or_insert(0) += 1;
        }
        for (manifest, dir) in parsed {
            if counts.get(&manifest.id).copied().unwrap_or(0) > 1 {
                let id = manifest.id.clone();
                cat.errors.push(LoadError {
                    dir,
                    error: ManifestError::DuplicateId(id),
                });
            } else {
                // P2 decisión 3: los VALORES de `[config]` se resuelven y
                // validan AQUÍ, al descubrir — fail-closed a nivel de
                // catálogo (mismo trato que `DuplicateId`): un `config.toml`
                // que no valida excluye el plugin ENTERO, nunca carga con
                // valores a medias.
                let settings = match resolve_settings(&manifest, &dir) {
                    Ok(settings) => settings,
                    Err(error) => {
                        cat.errors.push(LoadError {
                            dir,
                            error: ManifestError::from(error),
                        });
                        continue;
                    }
                };
                // El binario se lee UNA vez: de esos bytes salen el digest
                // que ancla la aprobación (#241) y los paquetes WIT que
                // nombra (ADR 0094). Un guest compilado contra otra versión
                // se lista como roto con las dos versiones, en vez de
                // cargarse y morir en wasmtime nombrando una interfaz.
                let bytes = read_wasm(&dir);
                let wit = bytes
                    .as_deref()
                    .map(crate::wit_packages)
                    .unwrap_or_default();
                if let Some(m) = crate::wit_mismatch(&wit) {
                    cat.errors.push(LoadError {
                        dir,
                        error: ManifestError::WitMismatch {
                            package: m.package,
                            built_against: m.built_against,
                            served: m.served,
                        },
                    });
                    continue;
                }
                cat.plugins.push(PluginEntry {
                    help: help_presence(&dir),
                    wasm_digest: bytes.as_deref().map(wasm_digest_of),
                    wit,
                    manifest,
                    dir,
                    enabled: false,
                    approved: false,
                    settings,
                });
            }
        }
        cat.plugins.sort_by(|a, b| {
            a.manifest
                .category
                .as_str()
                .cmp(b.manifest.category.as_str())
                .then_with(|| a.manifest.id.cmp(&b.manifest.id))
        });
        cat
    }

    /// Agrupa los plugins por categoría, en orden estable — la base de la vista
    /// ordenada del gestor (ADR 0022 D5). Solo categorías con algún plugin.
    #[must_use]
    pub fn by_category(&self) -> Vec<(Category, Vec<&PluginEntry>)> {
        const ORDER: [Category; 6] = [
            Category::Previewer,
            Category::Provider,
            Category::Command,
            Category::Columns,
            Category::Hook,
            Category::Decorator,
        ];
        ORDER
            .into_iter()
            .filter_map(|cat| {
                let group: Vec<&PluginEntry> = self
                    .plugins
                    .iter()
                    .filter(|p| p.manifest.category == cat)
                    .collect();
                (!group.is_empty()).then_some((cat, group))
            })
            .collect()
    }
}
