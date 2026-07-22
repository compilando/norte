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

/// Config del subsistema IA, capa de USUARIO de `norte.toml` (la de proyecto
/// se ignora fail-closed, mismo criterio que `[archive]`/policy). Todo OFF
/// por defecto (spec §9: IA opt-in).
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
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AiConfigError {
    /// TOML inválido o tipos incorrectos.
    #[error("invalid [ai] config: {0}")]
    Toml(#[from] toml::de::Error),
    /// Un `denied_prefix` no parsea como `VPath`.
    #[error("invalid denied_prefix `{0}`")]
    BadPrefix(String),
    /// Error de lectura del fichero (que no sea `NotFound`).
    #[error("io error reading norte.toml: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NorteTomlAi {
    ai: AiSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AiSection {
    enabled: bool,
    local_only: bool,
    denied_prefixes: Vec<String>,
    rename_provider: Option<String>,
    providers: std::collections::BTreeMap<String, RawProvider>,
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    kind: String,
    model: String,
    base_url: Option<String>,
}

impl AiConfig {
    /// Parsea la sección `[ai]` de un `norte.toml`. Fail-loud: TOML roto o un
    /// `denied_prefix` inválido abortan (un prefijo denegado que se cuela por
    /// un typo es un fallo de seguridad silencioso).
    ///
    /// # Errors
    /// [`AiConfigError`] si el TOML es inválido o un prefijo no parsea.
    pub fn parse(s: &str) -> Result<Self, AiConfigError> {
        let cfg: NorteTomlAi = toml::from_str(s)?;
        let a = cfg.ai;
        let mut denied = Vec::with_capacity(a.denied_prefixes.len());
        for p in &a.denied_prefixes {
            denied.push(VPath::parse(p).map_err(|_| AiConfigError::BadPrefix(p.clone()))?);
        }
        let providers = a
            .providers
            .into_iter()
            .map(|(name, r)| AiProviderConfig {
                name,
                kind: r.kind,
                model: r.model,
                base_url: r.base_url,
            })
            .collect();
        Ok(Self {
            enabled: a.enabled,
            local_only: a.local_only,
            denied_prefixes: denied,
            rename_provider: a.rename_provider,
            providers,
        })
    }

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

    /// Carga desde `config_dir()/norte.toml` (capa usuario; ausente =
    /// default deshabilitado). SÍNCRONA (arranque): `spawn_blocking` en async.
    ///
    /// # Errors
    /// Error de lectura (que no sea `NotFound`) o de parseo/validación.
    pub fn load() -> Result<Self, AiConfigError> {
        let path = crate::connect::config_dir().join("norte.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => Self::parse(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(AiConfigError::Io(e)),
        }
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
    let secret = norte_connect::SecretResolver::new(config_dir)
        .resolve(&key, &key)
        .await
        .ok()
        .flatten();
    build_provider(cfg, secret)
}

/// Operación de IA gateada (v1: solo rename).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiOp {
    /// Sugerencia de renombrado por lote.
    Rename,
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

    #[test]
    fn config_default_deshabilitado() {
        let c = AiConfig::default();
        assert!(!c.enabled && !c.local_only && c.denied_prefixes.is_empty());
        assert!(!AiConfig::parse("").expect("vacío").enabled);
    }

    #[test]
    fn config_parsea_seccion_completa() {
        let c = AiConfig::parse(
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
        assert!(matches!(
            AiConfig::parse("[ai]\ndenied_prefixes = [\"no-es-url\"]\n"),
            Err(AiConfigError::BadPrefix(_))
        ));
        assert!(AiConfig::parse("[ai]\nenabled = \"si\"\n").is_err());
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
