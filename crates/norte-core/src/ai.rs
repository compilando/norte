//! Subsistema IA del core (spec §9, ADR 0031, parte A2): config `[ai]`, el
//! gate opt-in/local-only/denied-paths que se aplica ANTES de que ningún
//! contenido salga del proceso, y el plan de rename REVISABLE (construcción
//! del prompt + validación de la respuesta). Aplicar un plan es N
//! `fs.move` ordinarios (journal + undo + policy) — nada nuevo que gobernar.
//!
//! Los proveedores viven en `norte-ai`; aquí solo se orquestan bajo el gate.

use norte_ai::{AiError, ChatMessage, ChatRequest};
use norte_proto::{Segment, VPath};
use serde::Deserialize;

/// Config del subsistema IA, mezclada en capas Sistema+Usuario de
/// `norte.toml` (ADR 0035; la capa de proyecto se ignora fail-closed, mismo
/// criterio que `[archive]`/policy). Todo OFF por defecto (spec §9: IA
/// opt-in).
#[derive(Debug, Clone, Default)]
pub struct AiConfig {
    /// IA habilitada. `false` (default) = el gate rechaza toda operación.
    pub enabled: bool,
    /// Modo solo-local: rechaza proveedores remotos (spec §9). El gate lo
    /// aplica como barrera DURA, no como cortesía del proveedor.
    pub local_only: bool,
    /// Prefijos cuyo contenido/nombres JAMÁS salen a un proveedor. Se
    /// comparan segment-aware (`is_under`), no por prefijo de string.
    pub denied_prefixes: Vec<VPath>,
    /// Nombre del proveedor a usar para el rename IA (de `providers`).
    pub rename_provider: Option<String>,
    /// Nombre del proveedor para embeddings (`index.embed` /
    /// `index.search_semantic`), de `providers`. Mismo contrato que
    /// `rename_provider`; ausente = sin embeddings.
    pub embed_provider: Option<String>,
    /// Proveedores declarados (`[ai.providers.<nombre>]`).
    pub providers: Vec<AiProviderConfig>,
}

/// Un proveedor declarado en `[ai.providers.<nombre>]`.
#[derive(Debug, Clone)]
pub struct AiProviderConfig {
    /// Nombre lógico (clave de la tabla).
    pub name: String,
    /// Tipo: `anthropic` | `ollama` | `openai-compat`.
    pub kind: String,
    /// Id del modelo tal cual lo espera el proveedor.
    pub model: String,
    /// URL base (obligatoria en `openai-compat`; default en los otros).
    pub base_url: Option<String>,
}

/// Error al cargar/validar `[ai]`.
///
/// Desde la migración a `norte-config` (ADR 0035) el parseo/merge de
/// `[ai]` vive en `norte-config::load`; sus errores llegan envueltos en
/// [`AiConfigError::Io`] (TOML roto, un `denied_prefix` inválido, tipos
/// incorrectos — todos son `norte_config::ConfigError` en origen). `Toml` y
/// `BadPrefix` ya no se construyen desde este crate, pero se conservan: son
/// parte del contrato público (`#[non_exhaustive]`, quitarlas sería un
/// cambio de semver visible) y `BadPrefix` sigue documentando ese modo de
/// fallo para quien matchee el enum.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AiConfigError {
    /// TOML inválido o tipos incorrectos.
    #[error("invalid [ai] config: {0}")]
    Toml(#[from] toml::de::Error),
    /// Un `denied_prefix` no parsea como `VPath`.
    #[error("invalid denied_prefix `{0}`")]
    BadPrefix(String),
    /// Error de lectura del fichero, o de `norte-config` (TOML roto,
    /// `denied_prefix` inválido, tipos incorrectos) desde que el parseo se
    /// delegó (ADR 0035) — no solo `NotFound`; el mensaje es genérico
    /// porque este variant también carga fallos de parseo, no solo de I/O.
    #[error("config error: {0}")]
    Io(#[from] std::io::Error),
}

impl AiConfig {
    /// El [`AiProviderConfig`] para el rename (el `rename_provider`
    /// nombrado, o el único si hay exactamente uno). `None` si no se puede
    /// determinar.
    #[must_use]
    pub fn rename_provider_config(&self) -> Option<&AiProviderConfig> {
        match &self.rename_provider {
            Some(name) => self.providers.iter().find(|p| &p.name == name),
            None if self.providers.len() == 1 => self.providers.first(),
            None => None,
        }
    }

    /// Proveedor de embeddings: el nombrado en `embed_provider`, o el único
    /// configurado si solo hay uno, o `None` (misma regla que
    /// [`Self::rename_provider_config`]).
    #[must_use]
    pub fn embed_provider_config(&self) -> Option<&AiProviderConfig> {
        match &self.embed_provider {
            Some(name) => self.providers.iter().find(|p| &p.name == name),
            None if self.providers.len() == 1 => self.providers.first(),
            None => None,
        }
    }

    /// Build from the already-merged `[ai]` settings (norte-config).
    fn from_settings(s: norte_config::AiSettings) -> Self {
        Self {
            enabled: s.enabled,
            local_only: s.local_only,
            denied_prefixes: s.denied_prefixes,
            rename_provider: s.rename_provider,
            embed_provider: s.embed_provider,
            providers: s
                .providers
                .into_iter()
                .map(|(name, r)| AiProviderConfig {
                    name,
                    kind: r.kind,
                    model: r.model,
                    base_url: r.base_url,
                })
                .collect(),
        }
    }

    /// Layered load (ADR 0035): System+User layers, Project ignored
    /// fail-closed. SYNC (startup): `spawn_blocking` in async contexts.
    ///
    /// C1 review (item 2): uses [`norte_config::standard_layers_no_project`]
    /// rather than [`norte_config::standard_layers`] — every `[ai]` value the
    /// Project layer could provide is carved out in `Self::from_settings`
    /// anyway (`norte-config::load` never merges `[ai]` from Project), so
    /// parsing `./.norte/norte.toml` here would give a foreign repo a
    /// startup-abort lever over `norte daemon run` (a broken or hostile
    /// project file, combined with `deny_unknown_fields`) and no other
    /// effect.
    ///
    /// # Errors
    /// [`AiConfigError`] if any layer's TOML is invalid.
    pub fn load() -> Result<Self, AiConfigError> {
        Self::load_from(&norte_config::standard_layers_no_project())
    }

    /// Like [`AiConfig::load`] with explicit layers (test injection).
    ///
    /// # Errors
    /// Any layer that exists but does not parse strictly, or cannot be
    /// read.
    pub fn load_from(layers: &norte_config::Layers) -> Result<Self, AiConfigError> {
        let cfg = norte_config::load(layers).map_err(|e| {
            AiConfigError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        Ok(Self::from_settings(cfg.ai))
    }
}

/// Instancia el proveedor de `cfg` con el `secret` ya resuelto (env → keyring
/// → age; los proveedores JAMÁS leen el secreto, regla 10). `openai-compat`
/// exige `base_url`; los demás la tienen por default.
///
/// # Errors
/// [`AiConfigError::BadPrefix`] se reusa como error genérico de config aquí
/// no aplica; devuelve un `String` de diagnóstico en su lugar.
pub fn build_provider(
    cfg: &AiProviderConfig,
    secret: Option<norte_connect::Secret>,
) -> Result<norte_ai::SharedAiProvider, String> {
    use std::sync::Arc;
    let p: norte_ai::SharedAiProvider = match cfg.kind.as_str() {
        "anthropic" => Arc::new(norte_ai::anthropic::AnthropicProvider::new(
            cfg.base_url.clone(),
            cfg.model.clone(),
            secret,
        )),
        "ollama" => Arc::new(norte_ai::ollama::OllamaProvider::new(
            cfg.base_url.clone(),
            cfg.model.clone(),
        )),
        "openai-compat" => {
            let base = cfg
                .base_url
                .clone()
                .ok_or_else(|| "openai-compat requiere base_url".to_owned())?;
            Arc::new(norte_ai::openai_compat::OpenAiCompatProvider::new(
                base,
                cfg.model.clone(),
                secret,
            ))
        }
        other => return Err(format!("tipo de proveedor de IA desconocido: {other}")),
    };
    Ok(p)
}

/// Resuelve el secreto del proveedor (`ai:<nombre>` vía env → keyring → age,
/// en `config_dir`; el proveedor jamás lo lee, regla 10) y lo instancia.
/// Atajo para los frontends: no tocan `norte-connect` directamente.
///
/// # Errors
/// Un `String` de diagnóstico si el proveedor no se puede construir (tipo
/// desconocido, `base_url` ausente en openai-compat).
pub async fn resolve_and_build(
    cfg: &AiProviderConfig,
    config_dir: std::path::PathBuf,
) -> Result<norte_ai::SharedAiProvider, String> {
    let key = format!("ai:{}", cfg.name);
    // Un FALLO al resolver el secreto no es «no hay secreto» (#122, INFO de la
    // revisión de seguridad de IA-2). Tragárselo construía un proveedor SIN
    // credencial y la petición salía igual: contra un endpoint que no exige
    // autenticación —un proxy interno, un `openai-compat` mal configurado— eso
    // manda los nombres del directorio del lector a un sitio al que nadie
    // autorizó a hablar. Y contra uno que sí la exige, el error que el lector
    // ve es un 401 del proveedor en vez del keyring bloqueado que lo causó.
    //
    // `Ok(None)` sí es «no hay secreto», y eso es legítimo: ollama y cualquier
    // modelo local no piden ninguno.
    let secret = norte_connect::SecretResolver::new(config_dir)
        .resolve(&key, &key)
        .await
        .map_err(|e| format!("no se pudo resolver el secreto de «{}»: {e}", cfg.name))?;
    build_provider(cfg, secret)
}

/// Instala el proveedor de embeddings de `config` en `engine` (M4-IA-2):
/// `embed_provider_config()` → [`resolve_and_build`] →
/// [`crate::Engine::set_ai_embed_provider`]. Fuente ÚNICA del wiring que
/// comparten daemon-run, la CLI embebida y la TUI embebida — antes vivía
/// triplicado y divergiría al primer cambio.
///
/// Devuelve un aviso IMPRIMIBLE (el caller decide el canal — eprintln en
/// CLI/TUI; el core no escribe a stderr) cuando el proveedor no se pudo
/// instalar: construcción fallida, o `embed_provider` nombra un proveedor
/// inexistente en `[ai.providers]` (distinto de "sin configurar", que es
/// silencio — los embeddings son opt-in). `None` = instalado o no
/// configurado.
//
// Instrumentada (#122, convención del repo para funciones efectivas del core):
// instala estado global del engine y toca el keyring, y sin traza el arranque
// no dice por qué la búsqueda semántica no responde. El nombre del proveedor
// es configuración del usuario, no bytes suyos, así que va en el span; el
// secreto JAMÁS (regla 10).
#[tracing::instrument(skip_all, fields(provider))]
pub async fn install_embed_provider(engine: &crate::Engine, config: &AiConfig) -> Option<String> {
    if let Some(pcfg) = config.embed_provider_config().cloned() {
        tracing::Span::current().record("provider", pcfg.name.as_str());
        match resolve_and_build(&pcfg, crate::connect::config_dir()).await {
            Ok(p) => {
                tracing::info!("proveedor de embeddings instalado");
                engine.set_ai_embed_provider(p);
                None
            }
            Err(e) => Some(format!(
                "aviso: proveedor de embeddings no disponible ({e}); \
                 index.embed/search_semantic darán Unsupported"
            )),
        }
    } else if config.embed_provider.is_some() {
        Some(
            "aviso: embed_provider nombra un proveedor que no existe en \
             [ai.providers]; index.embed/search_semantic darán Unsupported"
                .to_owned(),
        )
    } else {
        None
    }
}

/// Operación de IA gateada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiOp {
    /// Sugerencia de renombrado por lote.
    Rename,
    /// `index.embed` / `index.search_semantic` — prefijos de contenido o la
    /// query salen hacia el proveedor.
    Embed,
}

/// Motivo por el que el gate rechazó una operación de IA.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AiDenied {
    /// La IA está deshabilitada (`[ai] enabled = false`).
    #[error("AI is disabled")]
    Disabled,
    /// Modo local-only y el proveedor es remoto.
    #[error("local-only mode: remote provider refused")]
    LocalOnly,
    /// Alguna ruta cae bajo un `denied_prefix`.
    #[error("path under a denied prefix")]
    DeniedPath,
}

/// El gate opt-in de IA (spec §9): se consulta ANTES de que ningún nombre o
/// contenido llegue a un proveedor. Deshabilitado, local-only sobre remoto, o
/// una ruta bajo un prefijo denegado = rechazo DURO.
pub struct AiGate<'a> {
    config: &'a AiConfig,
}

impl<'a> AiGate<'a> {
    /// Gate sobre `config`.
    #[must_use]
    pub fn new(config: &'a AiConfig) -> Self {
        Self { config }
    }

    /// Comprueba la operación. `provider_is_local` = si el proveedor elegido
    /// corre localmente ([`norte_ai::AiProvider::is_local`]).
    ///
    /// # Errors
    /// [`AiDenied`] con el motivo; el caller lo mapea a la taxonomía del wire.
    pub fn check(
        &self,
        _op: AiOp,
        provider_is_local: bool,
        paths: &[&VPath],
    ) -> Result<(), AiDenied> {
        if !self.config.enabled {
            return Err(AiDenied::Disabled);
        }
        if self.config.local_only && !provider_is_local {
            return Err(AiDenied::LocalOnly);
        }
        for path in paths {
            if self
                .config
                .denied_prefixes
                .iter()
                .any(|prefix| crate::policy::is_under(prefix, path))
            {
                return Err(AiDenied::DeniedPath);
            }
        }
        Ok(())
    }
}

/// Una entrada del plan de rename: renombra `from` (nombre que existe en el
/// dir) a `to` (segmento válido nuevo). Ambos son nombres BASE, no rutas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameEntry {
    /// Nombre existente a renombrar.
    pub from: Segment,
    /// Nombre destino.
    pub to: Segment,
}

/// El plan de rename REVISABLE (spec §9): el producto de la IA. Aplicarlo es
/// N `fs.move` gobernados; construirlo/validarlo jamás muta nada.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenamePlan {
    /// Entradas del plan (solo las que cambian de nombre).
    pub entries: Vec<RenameEntry>,
}

/// Core → proto: el plan solo contiene nombres UTF-8 (invariante del engine:
/// hostiles rechazados fail-loud pre-proveedor).
///
/// La conversión NO es lossy, y por eso se hace con `from_utf8` y no con
/// `from_utf8_lossy` (#275). El invariante que lo garantiza vive dos
/// funciones más allá —`build_rename_prompt` rehúsa el directorio entero si
/// algún nombre no es representable— y un `lossy` aquí lo daba por hecho en
/// silencio: el día que ese invariante se mueva, esto colaría un U+FFFD en un
/// nombre de fichero en vez de decirlo. Una entrada que no sea UTF-8 se
/// SALTA, que es lo mismo que hace el validador con lo que no entiende.
pub(crate) fn ai_plan_to_proto(plan: RenamePlan) -> norte_proto::methods::AiRenamePlanResult {
    norte_proto::methods::AiRenamePlanResult {
        entries: plan
            .entries
            .into_iter()
            .filter_map(|e| {
                let from = String::from_utf8(e.from.as_bytes().to_vec()).ok()?;
                let to = String::from_utf8(e.to.as_bytes().to_vec()).ok()?;
                Some(norte_proto::methods::AiRenameEntry { from, to })
            })
            .collect(),
    }
}

/// Construye la petición de chat del rename: envía SOLO los nombres base
/// (bytes crudos → display lossy-marcado; un nombre hostil con U+FFFD se
/// rechaza fail-loud, jamás se manda) + la instrucción. Pide JSON estricto.
///
/// # Errors
/// [`AiError::Protocol`] si algún nombre no es representable sin pérdida
/// (contiene U+FFFD tras la conversión lossy — no se filtra un nombre
/// corrupto a un proveedor).
pub fn build_rename_prompt(names: &[Segment], instruction: &str) -> Result<ChatRequest, AiError> {
    let mut lines = Vec::with_capacity(names.len());
    for n in names {
        let display = String::from_utf8_lossy(n.as_bytes());
        if display.contains('\u{FFFD}') {
            return Err(AiError::Protocol(
                "nombre no-UTF8 no representable; rename IA no lo envía".into(),
            ));
        }
        lines.push(display.into_owned());
    }
    let system = "You rename files. Reply with STRICT JSON only: an array of \
         objects {\"from\": <existing name>, \"to\": <new name>}. Include ONLY \
         files that should be renamed. `from` must exactly match an input name. \
         `to` must be a plain file name: no slashes, no `..`, no leading dot \
         tricks. No prose, no code fences — just the JSON array."
        .to_owned();
    let user = format!(
        "Instruction: {instruction}\n\nFiles (one per line):\n{}",
        lines.join("\n")
    );
    Ok(ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(user)],
        max_tokens: Some(4096),
        json_schema: None,
    })
}

#[derive(Deserialize)]
struct RawRenameEntry {
    from: String,
    to: String,
}

/// Valida la respuesta del modelo contra el dir real (spec §9: el plan es el
/// producto, jamás un apply parcial). `inputs` = nombres existentes;
/// `existing` = los mismos (para detectar colisiones con nombres no
/// renombrados). Reglas: cada `from` ∈ inputs; cada `to` es un [`Segment`]
/// válido (sin `/`, `..`, NUL, `!`); sin destinos duplicados; un `to` no
/// colisiona con un nombre existente SALVO que ese nombre se renombre en el
/// mismo plan (swaps consistentes permitidos). Cualquier salida hostil o
/// malformada = error tipado.
///
/// # Errors
/// [`AiError::Protocol`] si el JSON no parsea o viola una regla de validación.
pub fn validate_rename_reply(reply: &str, inputs: &[Segment]) -> Result<RenamePlan, AiError> {
    let trimmed = reply.trim();
    let raw: Vec<RawRenameEntry> = serde_json::from_str(trimmed)
        .map_err(|e| AiError::Protocol(format!("respuesta no es JSON de rename: {e}")))?;

    let input_set: std::collections::HashSet<&[u8]> =
        inputs.iter().map(Segment::as_bytes).collect();

    let mut entries = Vec::with_capacity(raw.len());
    let mut froms: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut tos: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();

    for r in raw {
        let from = Segment::new(r.from.clone().into_bytes())
            .map_err(|_| AiError::Protocol(format!("`from` inválido: {:?}", r.from)))?;
        if !input_set.contains(from.as_bytes()) {
            return Err(AiError::Protocol(format!(
                "`from` no existe en el dir: {:?}",
                r.from
            )));
        }
        if from.as_bytes() == b"!" {
            return Err(AiError::Protocol(
                "`from` marcador de archivo prohibido".into(),
            ));
        }
        // `to`: Segment rechaza `/`, `..`, `.`, NUL, vacío. `!` (marcador de
        // archivo-como-directorio, ADR 0018) y `\` se rechazan aparte: el
        // backslash es separador en Windows → traversal (`..\evil`), y el
        // camino IA es superficie nueva por la que llegan bytes hostiles
        // (security MINOR del review #M4).
        let to = Segment::new(r.to.clone().into_bytes())
            .map_err(|_| AiError::Protocol(format!("`to` inválido: {:?}", r.to)))?;
        if to.as_bytes() == b"!" || to.as_bytes().contains(&b'\\') {
            return Err(AiError::Protocol(format!("`to` prohibido: {:?}", r.to)));
        }
        if !froms.insert(from.as_bytes().to_vec()) {
            return Err(AiError::Protocol(format!("`from` duplicado: {:?}", r.from)));
        }
        if !tos.insert(to.as_bytes().to_vec()) {
            return Err(AiError::Protocol(format!("`to` duplicado: {:?}", r.to)));
        }
        entries.push(RenameEntry { from, to });
    }

    // Colisión con un nombre EXISTENTE que NO se renombra: un `to` que ya
    // existe en el dir solo vale si ese nombre está en `froms` (se mueve).
    for e in &entries {
        if input_set.contains(e.to.as_bytes()) && !froms.contains(e.to.as_bytes()) {
            return Err(AiError::Protocol(format!(
                "`to` colisiona con un archivo existente que no se renombra: {:?}",
                String::from_utf8_lossy(e.to.as_bytes())
            )));
        }
    }

    Ok(RenamePlan { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("segmento de test")
    }

    /// Loads `[ai]` from a single-file User layer (test injection, mirrors
    /// `archive_config`'s tempdir pattern now that parsing lives in
    /// norte-config).
    fn load_from_toml(s: &str) -> Result<AiConfig, AiConfigError> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), s).unwrap();
        let layers = norte_config::Layers {
            dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
        };
        AiConfig::load_from(&layers)
    }

    /// Pins that `from_settings` sees the MERGED result across layers, not
    /// just the last one read: System enables IA and declares provider `x`
    /// with an old model; User overrides only the model, by name.
    #[test]
    fn dos_capas_se_mezclan_por_nombre_de_proveedor() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[ai]\nenabled = true\n[ai.providers.x]\nkind = \"ollama\"\nmodel = \"viejo\"\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ai.providers.x]\nkind = \"ollama\"\nmodel = \"nuevo\"\n",
        )
        .unwrap();
        let layers = norte_config::Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), norte_config::Layer::System),
                (usuario.path().to_path_buf(), norte_config::Layer::User),
            ],
        };
        let cfg = AiConfig::load_from(&layers).expect("carga");
        assert!(cfg.enabled);
        let x = cfg
            .providers
            .iter()
            .find(|p| p.name == "x")
            .expect("proveedor x presente");
        assert_eq!(x.model, "nuevo");
    }

    #[test]
    fn config_default_deshabilitado() {
        let c = AiConfig::default();
        assert!(!c.enabled && !c.local_only && c.denied_prefixes.is_empty());
        assert!(!load_from_toml("").expect("vacío").enabled);
    }

    #[test]
    fn config_parsea_seccion_completa() {
        let c = load_from_toml(
            "[ai]\nenabled = true\nlocal_only = true\n\
             denied_prefixes = [\"file:///secret\", \"file:///home/o/.ssh\"]\n\
             rename_provider = \"local\"\n",
        )
        .expect("parsea");
        assert!(c.enabled && c.local_only);
        assert_eq!(c.denied_prefixes.len(), 2);
        assert_eq!(c.rename_provider.as_deref(), Some("local"));
    }

    #[test]
    fn config_prefijo_invalido_es_error() {
        // norte-config valida `denied_prefixes` en su propio loader: la
        // variante ahora es un `AiConfigError::Io`-envuelto `ConfigError`,
        // no `AiConfigError::BadPrefix` (ese variant queda documentado pero
        // sin construir desde aquí — ver su rustdoc).
        assert!(load_from_toml("[ai]\ndenied_prefixes = [\"no-es-url\"]\n").is_err());
        assert!(load_from_toml("[ai]\nenabled = \"si\"\n").is_err());
    }

    #[test]
    fn embed_provider_config_named_else_single_else_none() {
        let mut cfg = AiConfig::default();
        assert!(cfg.embed_provider_config().is_none());
        cfg.providers.push(AiProviderConfig {
            name: "solo".into(),
            kind: "ollama".into(),
            model: "nomic-embed-text".into(),
            base_url: None,
        });
        // un único proveedor sin nombre explícito ⇒ ese
        assert_eq!(cfg.embed_provider_config().unwrap().name, "solo");
        cfg.providers.push(AiProviderConfig {
            name: "b".into(),
            kind: "ollama".into(),
            model: "x".into(),
            base_url: None,
        });
        // dos y sin nombre ⇒ None (ambiguo)
        assert!(cfg.embed_provider_config().is_none());
        cfg.embed_provider = Some("b".into());
        assert_eq!(cfg.embed_provider_config().unwrap().name, "b");
    }

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("wire")
    }

    #[test]
    fn gate_deshabilitado_rechaza() {
        let c = AiConfig::default();
        let g = AiGate::new(&c);
        assert_eq!(
            g.check(AiOp::Rename, true, &[&vp("file:///d")]),
            Err(AiDenied::Disabled)
        );
    }

    #[test]
    fn gate_local_only_rechaza_remoto_permite_local() {
        let c = AiConfig {
            enabled: true,
            local_only: true,
            ..Default::default()
        };
        let g = AiGate::new(&c);
        assert_eq!(
            g.check(AiOp::Rename, false, &[&vp("file:///d")]),
            Err(AiDenied::LocalOnly)
        );
        assert!(g.check(AiOp::Rename, true, &[&vp("file:///d")]).is_ok());
    }

    #[test]
    fn gate_denied_prefix_segment_aware() {
        let c = AiConfig {
            enabled: true,
            denied_prefixes: vec![vp("file:///home/o/secret")],
            ..Default::default()
        };
        let g = AiGate::new(&c);
        // Bajo el prefijo: rechazo.
        assert_eq!(
            g.check(AiOp::Rename, true, &[&vp("file:///home/o/secret/k")]),
            Err(AiDenied::DeniedPath)
        );
        // Hermano con prefijo de string común PERO no bajo el segmento: OK.
        assert!(
            g.check(AiOp::Rename, true, &[&vp("file:///home/o/secretos/x")])
                .is_ok()
        );
    }

    #[test]
    fn prompt_rechaza_nombre_no_utf8() {
        let names = [seg(b"ok.txt"), seg(b"caf\xe9\xff")];
        assert!(matches!(
            build_rename_prompt(&names, "lower"),
            Err(AiError::Protocol(_))
        ));
    }

    #[test]
    fn prompt_incluye_instruccion_y_nombres() {
        let req =
            build_rename_prompt(&[seg(b"A.TXT"), seg(b"B.TXT")], "lowercase").expect("prompt");
        assert!(req.system.is_some());
        let u = &req.messages[0].content;
        assert!(u.contains("lowercase") && u.contains("A.TXT") && u.contains("B.TXT"));
    }

    #[test]
    fn validate_plan_valido() {
        let inputs = [seg(b"A.TXT"), seg(b"B.TXT")];
        let plan = validate_rename_reply(
            r#"[{"from":"A.TXT","to":"a.txt"},{"from":"B.TXT","to":"b.txt"}]"#,
            &inputs,
        )
        .expect("plan");
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].to.as_bytes(), b"a.txt");
    }

    #[test]
    fn validate_from_inexistente_es_error() {
        let inputs = [seg(b"A.TXT")];
        assert!(validate_rename_reply(r#"[{"from":"Z.TXT","to":"z"}]"#, &inputs).is_err());
    }

    #[test]
    fn validate_to_con_traversal_es_error() {
        let inputs = [seg(b"A")];
        assert!(validate_rename_reply(r#"[{"from":"A","to":"../x"}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":"a/b"}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":".."}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":"!"}]"#, &inputs).is_err());
        // security MINOR #M4: backslash = traversal en Windows.
        assert!(validate_rename_reply(r#"[{"from":"A","to":"..\\evil"}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":"a\\b"}]"#, &inputs).is_err());
    }

    #[test]
    fn validate_destino_duplicado_es_error() {
        let inputs = [seg(b"A"), seg(b"B")];
        assert!(
            validate_rename_reply(r#"[{"from":"A","to":"x"},{"from":"B","to":"x"}]"#, &inputs)
                .is_err()
        );
    }

    #[test]
    fn validate_swap_consistente_ok() {
        // a→b, b→a: cada `to` colisiona con un existente PERO ambos se mueven.
        let inputs = [seg(b"a"), seg(b"b")];
        let plan =
            validate_rename_reply(r#"[{"from":"a","to":"b"},{"from":"b","to":"a"}]"#, &inputs)
                .expect("swap válido");
        assert_eq!(plan.entries.len(), 2);
    }

    #[test]
    fn validate_colision_con_no_renombrado_es_error() {
        // A→B pero B existe y NO se renombra: colisión.
        let inputs = [seg(b"A"), seg(b"B")];
        assert!(validate_rename_reply(r#"[{"from":"A","to":"B"}]"#, &inputs).is_err());
    }

    #[test]
    fn validate_json_roto_es_protocol() {
        let inputs = [seg(b"A")];
        assert!(matches!(
            validate_rename_reply("no soy json", &inputs),
            Err(AiError::Protocol(_))
        ));
    }
}
