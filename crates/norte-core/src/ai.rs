//! Core AI subsystem (spec §9, ADR 0031, part A2): the `[ai]` config, the
//! opt-in/local-only/denied-paths gate applied BEFORE any content leaves the
//! process, and the REVIEWABLE rename plan (prompt construction + response
//! validation). Applying a plan is N ordinary `fs.move`s (journal + undo +
//! policy) — nothing new to govern.
//!
//! The providers live in `norte-ai`; here they are only orchestrated under
//! the gate.

use norte_ai::{AiError, ChatMessage, ChatRequest, JsonContract};
use norte_proto::{Segment, VPath};
use serde::Deserialize;

/// Config for the AI subsystem, merged in System+User layers from
/// `norte.toml` (ADR 0035; the Project layer is ignored fail-closed, the
/// same criterion as `[archive]`/policy). Everything OFF by default (spec
/// §9: AI is opt-in).
#[derive(Debug, Clone, Default)]
pub struct AiConfig {
    /// AI enabled. `false` (default) = the gate rejects every operation.
    pub enabled: bool,
    /// Local-only mode: rejects remote providers (spec §9). The gate
    /// enforces it as a HARD barrier, not as a courtesy from the provider.
    pub local_only: bool,
    /// Prefixes whose content/names NEVER leave to a provider. Compared
    /// segment-aware (`is_under`), not by string prefix.
    pub denied_prefixes: Vec<VPath>,
    /// Provider name to use for AI rename (from `providers`).
    pub rename_provider: Option<String>,
    /// Provider name for embeddings (`index.embed` /
    /// `index.search_semantic`), from `providers`. Same contract as
    /// `rename_provider`; absent = no embeddings.
    pub embed_provider: Option<String>,
    /// Declared providers (`[ai.providers.<name>]`).
    pub providers: Vec<AiProviderConfig>,
}

/// A provider declared in `[ai.providers.<name>]`.
#[derive(Debug, Clone)]
pub struct AiProviderConfig {
    /// Logical name (the table's key).
    pub name: String,
    /// Type: `anthropic` | `ollama` | `openai-compat`.
    pub kind: String,
    /// Model id exactly as the provider expects it.
    pub model: String,
    /// Base URL (required in `openai-compat`; defaulted in the others).
    pub base_url: Option<String>,
}

/// Error loading/validating `[ai]`.
///
/// Since the migration to `norte-config` (ADR 0035), parsing/merging
/// `[ai]` lives in `norte-config::load`; its errors arrive wrapped in
/// [`AiConfigError::Io`] (broken TOML, an invalid `denied_prefix`, wrong
/// types — all of these are `norte_config::ConfigError` at the source).
/// `Toml` and `BadPrefix` are no longer constructed from this crate, but
/// are kept: they are part of the public contract (`#[non_exhaustive]`,
/// removing them would be a visible semver change) and `BadPrefix` still
/// documents that failure mode for whoever matches the enum.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AiConfigError {
    /// Invalid TOML or wrong types.
    #[error("invalid [ai] config: {0}")]
    Toml(#[from] toml::de::Error),
    /// A `denied_prefix` does not parse as a `VPath`.
    #[error("invalid denied_prefix `{0}`")]
    BadPrefix(String),
    /// File read error, or from `norte-config` (broken TOML, invalid
    /// `denied_prefix`, wrong types) since parsing was delegated (ADR
    /// 0035) — not just `NotFound`; the message is generic because this
    /// variant also carries parse failures, not only I/O.
    #[error("config error: {0}")]
    Io(#[from] std::io::Error),
}

impl AiConfig {
    /// The [`AiProviderConfig`] for rename (the named `rename_provider`,
    /// or the only one if there is exactly one). `None` if it cannot be
    /// determined.
    #[must_use]
    pub fn rename_provider_config(&self) -> Option<&AiProviderConfig> {
        match &self.rename_provider {
            Some(name) => self.providers.iter().find(|p| &p.name == name),
            None if self.providers.len() == 1 => self.providers.first(),
            None => None,
        }
    }

    /// Embeddings provider: the one named in `embed_provider`, or the only
    /// one configured if there is only one, or `None` (same rule as
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

/// Instantiates the provider from `cfg` with `secret` already resolved (env
/// → keyring → age; providers NEVER read the secret, rule 10).
/// `openai-compat` requires `base_url`; the others default it.
///
/// # Errors
/// [`AiConfigError::BadPrefix`] reused as a generic config error does not
/// apply here; returns a diagnostic `String` instead.
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
                .ok_or_else(|| "openai-compat requires base_url".to_owned())?;
            Arc::new(norte_ai::openai_compat::OpenAiCompatProvider::new(
                base,
                cfg.model.clone(),
                secret,
            ))
        }
        other => return Err(format!("unknown AI provider type: {other}")),
    };
    Ok(p)
}

/// Resolves the provider's secret (`ai:<name>` via env → keyring → age, in
/// `config_dir`; the provider never reads it, rule 10) and instantiates it.
/// Shortcut for frontends: they don't touch `norte-connect` directly.
///
/// # Errors
/// A diagnostic `String` if the provider cannot be built (unknown type,
/// missing `base_url` in openai-compat).
pub async fn resolve_and_build(
    cfg: &AiProviderConfig,
    config_dir: std::path::PathBuf,
) -> Result<norte_ai::SharedAiProvider, String> {
    let key = format!("ai:{}", cfg.name);
    // A FAILURE resolving the secret is not "there is no secret" (#122, INFO
    // from the AI-2 security review). Swallowing it built a provider WITHOUT
    // a credential and the request went out anyway: against an endpoint that
    // does not require authentication — an internal proxy, a misconfigured
    // `openai-compat` — that sends the reader's directory names to a place
    // nobody authorized talking to. And against one that does require it,
    // the error the reader sees is a 401 from the provider instead of the
    // blocked keyring that caused it.
    //
    // `Ok(None)` IS "there is no secret", and that is legitimate: ollama and
    // any local model ask for none.
    let secret = norte_connect::SecretResolver::new(config_dir)
        .resolve(&key, &key)
        .await
        .map_err(|e| format!("could not resolve the secret for «{}»: {e}", cfg.name))?;
    build_provider(cfg, secret)
}

/// Installs `config`'s embeddings provider onto `engine` (M4-IA-2):
/// `embed_provider_config()` → [`resolve_and_build`] →
/// [`crate::Engine::set_ai_embed_provider`]. SINGLE source of the wiring
/// shared by daemon-run, the embedded CLI and the embedded TUI — it used to
/// live tripled and would diverge at the first change.
///
/// Returns a PRINTABLE notice (the caller decides the channel — eprintln in
/// CLI/TUI; the core does not write to stderr) when the provider could not
/// be installed: build failed, or `embed_provider` names a provider that
/// does not exist in `[ai.providers]` (different from "not configured",
/// which is silence — embeddings are opt-in). `None` = installed or not
/// configured.
//
// Instrumented (#122, the repo's convention for effectful core functions):
// installs global engine state and touches the keyring, and without a trace
// startup does not say why semantic search is not responding. The provider
// name is the user's configuration, not their bytes, so it goes in the
// span; the secret NEVER (rule 10).
//
// `config_dir` is where the secret is resolved from: the SAME one whoever
// is equipping uses, not the global one, so that an engine pointed at
// another directory (a test) does not reach the user's keyring.
#[tracing::instrument(skip_all, fields(provider))]
pub async fn install_embed_provider(
    engine: &crate::Engine,
    config: &AiConfig,
    config_dir: std::path::PathBuf,
) -> Option<String> {
    if let Some(pcfg) = config.embed_provider_config().cloned() {
        tracing::Span::current().record("provider", pcfg.name.as_str());
        match resolve_and_build(&pcfg, config_dir).await {
            Ok(p) => {
                tracing::info!("embeddings provider installed");
                engine.set_ai_embed_provider(p);
                None
            }
            Err(e) => Some(format!(
                "warning: embeddings provider unavailable ({e}); \
                 index.embed/search_semantic will return Unsupported"
            )),
        }
    } else if config.embed_provider.is_some() {
        Some(
            "warning: embed_provider names a provider that does not exist in \
             [ai.providers]; index.embed/search_semantic will return Unsupported"
                .to_owned(),
        )
    } else {
        None
    }
}

/// Gated AI operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiOp {
    /// Batch rename suggestion.
    Rename,
    /// `index.embed` / `index.search_semantic` — content prefixes or the
    /// query go out to the provider.
    Embed,
}

/// Reason the gate rejected an AI operation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AiDenied {
    /// AI is disabled (`[ai] enabled = false`).
    #[error("AI is disabled")]
    Disabled,
    /// Local-only mode and the provider is remote.
    #[error("local-only mode: remote provider refused")]
    LocalOnly,
    /// A path falls under a `denied_prefix`.
    #[error("path under a denied prefix")]
    DeniedPath,
}

/// The AI opt-in gate (spec §9): consulted BEFORE any name or content
/// reaches a provider. Disabled, local-only over remote, or a path under a
/// denied prefix = HARD rejection.
pub struct AiGate<'a> {
    config: &'a AiConfig,
}

impl<'a> AiGate<'a> {
    /// Gate over `config`.
    #[must_use]
    pub fn new(config: &'a AiConfig) -> Self {
        Self { config }
    }

    /// Checks the operation. `provider_is_local` = whether the chosen
    /// provider runs locally ([`norte_ai::AiProvider::is_local`]).
    ///
    /// # Errors
    /// [`AiDenied`] with the reason; the caller maps it to the wire
    /// taxonomy.
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

/// One entry of the rename plan: renames `from` (a name that exists in the
/// dir) to `to` (a valid new segment). Both are BASE names, not paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameEntry {
    /// Existing name to rename.
    pub from: Segment,
    /// Destination name.
    pub to: Segment,
}

/// The REVIEWABLE rename plan (spec §9): the AI's product. Applying it is N
/// governed `fs.move`s; building/validating it never mutates anything.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenamePlan {
    /// Plan entries (only the ones that change name).
    pub entries: Vec<RenameEntry>,
}

/// Core → proto: the plan only contains UTF-8 names (engine invariant:
/// hostile names are rejected fail-loud pre-provider).
///
/// The conversion is NOT lossy, and that is why it uses `from_utf8` and not
/// `from_utf8_lossy` (#275). The invariant that guarantees it lives two
/// functions further down — `build_rename_prompt` refuses the whole
/// directory if any name is not representable — and a `lossy` here took it
/// for granted silently: the day that invariant moves, this would let a
/// U+FFFD slip into a file name instead of saying so. An entry that is not
/// UTF-8 is SKIPPED, which is the same thing the validator does with what
/// it does not understand.
pub(crate) fn ai_plan_to_proto(plan: RenamePlan) -> norte_proto::methods::AiRenamePlanResult {
    norte_proto::methods::AiRenamePlanResult {
        refused: None,
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

/// Builds the rename chat request: sends ONLY the base names (raw bytes →
/// lossy-marked display; a hostile name with U+FFFD is rejected fail-loud,
/// never sent) + the instruction. Asks for strict JSON.
///
/// # Errors
/// [`AiError::Protocol`] if any name is not losslessly representable
/// (contains U+FFFD after the lossy conversion — a corrupt name is not
/// leaked to a provider).
pub fn build_rename_prompt(names: &[Segment], instruction: &str) -> Result<ChatRequest, AiError> {
    let mut lines = Vec::with_capacity(names.len());
    for n in names {
        let display = String::from_utf8_lossy(n.as_bytes());
        if display.contains('\u{FFFD}') {
            return Err(AiError::Protocol(
                "non-UTF8 name not representable; AI rename does not send it".into(),
            ));
        }
        lines.push(display.into_owned());
    }
    // The prompt still DESCRIBES the shape, and it is not redundant: it is
    // the only thing a provider that does not honor the contract has
    // (Ollama, a compatible server that ignores `response_format`, an old
    // Anthropic model). The contract below spares it for whoever does honor
    // it.
    let system = "You rename files. Reply with STRICT JSON only: an object \
         {\"renames\": [{\"from\": <existing name>, \"to\": <new name>}]}. \
         Include ONLY files that should be renamed. `from` must exactly match \
         an input name. `to` must be a plain file name: no slashes, no `..`, \
         no leading dot tricks. No prose, no code fences — just the JSON."
        .to_owned();
    let user = format!(
        "Instruction: {instruction}\n\nFiles (one per line):\n{}",
        lines.join("\n")
    );
    Ok(ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(user)],
        max_tokens: Some(4096),
        json_schema: Some(rename_contract()),
    })
}

/// The output contract of the rename plan.
///
/// OBJECT root and not an array: native structured-output mechanisms
/// expect an object at the top, and wrapping the list in `renames` costs
/// one field and avoids discovering it with a 400 in production.
///
/// No `minLength`, `maxLength` or `pattern`: Anthropic's structured output
/// does not support string constraints, and adding them would get the
/// whole schema rejected. The rules that actually matter — that `from`
/// exists, that `to` is a basename with no traversal, that there are no
/// duplicate destinations or collisions — **do not fit in a JSON Schema**
/// and that is not where they belong: [`validate_rename_reply`] applies
/// them against the REAL directory, whether the provider honors the
/// contract or not.
fn rename_contract() -> JsonContract {
    JsonContract::new(
        "norte_rename_plan",
        serde_json::json!({
            "type": "object",
            "properties": {
                "renames": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "from": {"type": "string"},
                            "to": {"type": "string"}
                        },
                        "required": ["from", "to"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["renames"],
            "additionalProperties": false
        }),
    )
}

#[derive(Deserialize)]
struct RawRenameEntry {
    from: String,
    to: String,
}

/// The envelope a provider that DID honor the contract returns.
///
/// `deny_unknown_fields` because the contract says `additionalProperties:
/// false`: the parser has to demand the same thing the schema does, or the
/// pair is lying. Without it, a `{"renames": [], "changes": [...the real
/// ones...]}` — a model that makes up the key, a compromised endpoint —
/// would come out as an EMPTY plan and silently: "there is nothing to
/// rename" instead of "this is not the response I asked for".
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRenamePlan {
    renames: Vec<RawRenameEntry>,
}

/// The TWO shapes in which the plan can arrive.
///
/// The object `{"renames": [...]}` is what a provider that honors the typed
/// output contract returns. The bare array is what a provider that does not
/// honor it — and only has the prompt — returns: Ollama, a compatible
/// server that ignores `response_format`, an Anthropic model without
/// structured output. Accepting both is what lets the contract be an
/// IMPROVEMENT and not a break: nothing stops working for not supporting
/// it.
///
/// What does NOT change depending on the shape is validation: both fall
/// into the same rules against the real directory.
/// It is decided by the FIRST brace, not by trying one and falling back:
/// an envelope with one bad entry has to give the envelope's error. With
/// the previous `if let Ok`, `{"renames":[{"from":"a"}]}` failed in the
/// object branch, fell to the array branch and came out as "invalid type:
/// map, expected a sequence" — the real reason, which was the missing `to`
/// field, got discarded along the way.
fn parse_plan(text: &str) -> Result<Vec<RawRenameEntry>, AiError> {
    let error = |e: serde_json::Error| AiError::Protocol(format!("response is not JSON: {e}"));
    if text.starts_with('{') {
        return serde_json::from_str::<RawRenamePlan>(text)
            .map(|s| s.renames)
            .map_err(error);
    }
    serde_json::from_str::<Vec<RawRenameEntry>>(text).map_err(error)
}

/// Validates the model's response against the real dir (spec §9: the plan
/// is the product, never a partial apply). `inputs` = existing names;
/// `existing` = the same (to detect collisions with names that are not
/// renamed). Rules: every `from` ∈ inputs; every `to` is a valid
/// [`Segment`] (no `/`, `..`, NUL, `!`); no duplicate destinations; a `to`
/// does not collide with an existing name UNLESS that name is renamed in
/// the same plan (consistent swaps allowed). Any hostile or malformed
/// output = typed error.
///
/// # Errors
/// [`AiError::Protocol`] if the JSON does not parse or violates a
/// validation rule.
pub fn validate_rename_reply(reply: &str, inputs: &[Segment]) -> Result<RenamePlan, AiError> {
    let trimmed = reply.trim();
    let raw = parse_plan(trimmed)?;

    let input_set: std::collections::HashSet<&[u8]> =
        inputs.iter().map(Segment::as_bytes).collect();

    let mut entries = Vec::with_capacity(raw.len());
    let mut froms: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut tos: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();

    for r in raw {
        let from = Segment::new(r.from.clone().into_bytes())
            .map_err(|_| AiError::Protocol(format!("invalid `from`: {:?}", r.from)))?;
        if !input_set.contains(from.as_bytes()) {
            return Err(AiError::Protocol(format!(
                "`from` does not exist in the dir: {:?}",
                r.from
            )));
        }
        if from.as_bytes() == b"!" {
            return Err(AiError::Protocol(
                "`from` file-as-directory marker forbidden".into(),
            ));
        }
        // `to`: Segment rejects `/`, `..`, `.`, NUL, empty. `!` (file-as-
        // directory marker, ADR 0018) and `\` are rejected separately: the
        // backslash is a separator on Windows → traversal (`..\evil`), and
        // the AI path is new surface through which hostile bytes arrive
        // (security MINOR from the #M4 review).
        let to = Segment::new(r.to.clone().into_bytes())
            .map_err(|_| AiError::Protocol(format!("invalid `to`: {:?}", r.to)))?;
        if to.as_bytes() == b"!" || to.as_bytes().contains(&b'\\') {
            return Err(AiError::Protocol(format!("`to` forbidden: {:?}", r.to)));
        }
        if !froms.insert(from.as_bytes().to_vec()) {
            return Err(AiError::Protocol(format!("duplicate `from`: {:?}", r.from)));
        }
        if !tos.insert(to.as_bytes().to_vec()) {
            return Err(AiError::Protocol(format!("duplicate `to`: {:?}", r.to)));
        }
        entries.push(RenameEntry { from, to });
    }

    // Collision with an EXISTING name that is NOT renamed: a `to` that
    // already exists in the dir is only valid if that name is in `froms`
    // (it is being moved).
    for e in &entries {
        if input_set.contains(e.to.as_bytes()) && !froms.contains(e.to.as_bytes()) {
            return Err(AiError::Protocol(format!(
                "`to` collides with an existing file that is not renamed: {:?}",
                String::from_utf8_lossy(e.to.as_bytes())
            )));
        }
    }

    Ok(RenamePlan { entries })
}

/// An ORGANIZE plan exactly as it comes out of the provider (phase 8).
///
/// Carries the destinations as TEXT and not as segments, on purpose: the
/// one who validates the relative path is
/// [`norte_proto::methods::validar_proposed_rel`], and it has to be the
/// same function the core applies when executing. Two validations for the
/// same rule diverge, and the one that relaxes is always the one that does
/// not delete files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrganizePlanReply {
    /// The proposed moves.
    pub moves: Vec<norte_proto::methods::OrganizeMove>,
}

/// The typed output contract for an organize plan.
fn organize_contract() -> JsonContract {
    JsonContract::new(
        "norte_organize_plan",
        serde_json::json!({
            "type": "object",
            "properties": {
                "moves": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "current": {"type": "string"},
                            "proposed_rel": {"type": "string"}
                        },
                        "required": ["current", "proposed_rel"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["moves"],
            "additionalProperties": false
        }),
    )
}

#[derive(Deserialize)]
struct RawOrganizeMove {
    current: String,
    proposed_rel: String,
}

/// The organize prompt.
///
/// States the shape in addition to sending the contract, for the same
/// reason as the rename one: it is the only thing a provider that does not
/// honor `response_format` has.
///
/// # Errors
/// [`AiError::Protocol`] if any name is not representable in UTF-8: such a
/// name cannot be part of a plan that travels over the wire, and it is set
/// aside BEFORE it leaves the machine.
pub fn build_organize_prompt(names: &[Segment], instruction: &str) -> Result<ChatRequest, AiError> {
    let mut lines = Vec::with_capacity(names.len());
    for n in names {
        let display = String::from_utf8_lossy(n.as_bytes());
        if display.contains('\u{FFFD}') {
            return Err(AiError::Protocol(
                "non-UTF8 name not representable; AI organize does not send it".into(),
            ));
        }
        lines.push(display.into_owned());
    }
    let system = "You organise files into folders. Reply with STRICT JSON only: an object \
         {\"moves\": [{\"current\": <existing name>, \"proposed_rel\": <destination>}]}. \
         Include ONLY files that should move. `current` must exactly match an input name. \
         `proposed_rel` is a path RELATIVE to the current directory, using `/` as separator, \
         and it must end in the file's new name: e.g. \"invoices/2026/march.pdf\". \
         It must NOT be absolute, must NOT contain `..` or `.` segments, and must not be \
         empty. No prose, no code fences — just the JSON."
        .to_owned();
    let user = format!(
        "Instruction: {instruction}\n\nFiles (one per line):\n{}",
        lines.join("\n")
    );
    Ok(ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(user)],
        max_tokens: Some(4096),
        json_schema: Some(organize_contract()),
    })
}

/// Validates an organize plan's response against the names that were sent.
///
/// What it checks, and why each thing:
///
/// - **`current` exists among the sent names.** A plan about a file nobody
///   mentioned is a plan about another directory.
/// - **`proposed_rel` passes
///   [`norte_proto::methods::validar_proposed_rel`]**, which is the same
///   gate the core applies when executing. Not absolute, no `..`, not
///   empty, not deeper than the cap. A compromised model — or simply a bad
///   one — cannot write outside the directory.
/// - **Neither `!` nor `\` in any segment.** The backslash is a separator on
///   Windows, so `..\outside` is traversal as soon as the plan crosses
///   systems; `!` is the file-as-directory marker (ADR 0018).
/// - **No repeated sources or destinations**: a plan that contradicts
///   itself cannot be carried out in full, and applying it halfway is
///   exactly what this exists to prevent.
///
/// # Errors
/// [`AiError::Protocol`] with what failed, for the operator's log.
pub fn validate_organize_reply(
    reply: &str,
    inputs: &[Segment],
) -> Result<OrganizePlanReply, AiError> {
    let trimmed = reply.trim();
    let raw: Vec<RawOrganizeMove> = parse_organize(trimmed)?;

    let input_set: std::collections::HashSet<&[u8]> =
        inputs.iter().map(Segment::as_bytes).collect();
    let mut moves = Vec::with_capacity(raw.len());
    let mut sources: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut destinations: std::collections::HashSet<String> = std::collections::HashSet::new();

    for r in raw {
        let current = Segment::new(r.current.clone().into_bytes())
            .map_err(|_| AiError::Protocol(format!("invalid `current`: {:?}", r.current)))?;
        if !input_set.contains(current.as_bytes()) {
            return Err(AiError::Protocol(format!(
                "`current` does not exist in the dir: {:?}",
                r.current
            )));
        }
        if current.as_bytes() == b"!" {
            return Err(AiError::Protocol("`current` file marker forbidden".into()));
        }
        // THE gate: the same one the core applies when executing.
        let segs = norte_proto::methods::validar_proposed_rel(&r.proposed_rel).map_err(|e| {
            AiError::Protocol(format!(
                "invalid `proposed_rel` ({e}): {:?}",
                r.proposed_rel
            ))
        })?;
        for s in &segs {
            if s.as_bytes() == b"!" || s.as_bytes().contains(&b'\\') {
                return Err(AiError::Protocol(format!(
                    "`proposed_rel` forbidden: {:?}",
                    r.proposed_rel
                )));
            }
        }
        if !sources.insert(current.as_bytes().to_vec()) {
            return Err(AiError::Protocol(format!(
                "duplicate `current`: {:?}",
                r.current
            )));
        }
        if !destinations.insert(r.proposed_rel.clone()) {
            return Err(AiError::Protocol(format!(
                "duplicate `proposed_rel`: {:?}",
                r.proposed_rel
            )));
        }
        moves.push(norte_proto::methods::OrganizeMove {
            current: r.current,
            proposed_rel: r.proposed_rel,
        });
    }
    Ok(OrganizePlanReply { moves })
}

/// Pulls the moves array out of the response, with the same tolerance as
/// the rename one: the object with its key, or the bare array a provider
/// that ignores the contract returns.
fn parse_organize(s: &str) -> Result<Vec<RawOrganizeMove>, AiError> {
    #[derive(Deserialize)]
    struct Envelope {
        moves: Vec<RawOrganizeMove>,
    }
    if let Ok(e) = serde_json::from_str::<Envelope>(s) {
        return Ok(e.moves);
    }
    serde_json::from_str::<Vec<RawOrganizeMove>>(s)
        .map_err(|e| AiError::Protocol(format!("response is not an organize plan: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(b: &[u8]) -> Segment {
        Segment::new(b.to_vec()).expect("test segment")
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
    fn two_layers_merge_by_provider_name() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ai]\nenabled = true\n[ai.providers.x]\nkind = \"ollama\"\nmodel = \"old\"\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ai.providers.x]\nkind = \"ollama\"\nmodel = \"new\"\n",
        )
        .unwrap();
        let layers = norte_config::Layers {
            dirs: vec![
                (system.path().to_path_buf(), norte_config::Layer::System),
                (user.path().to_path_buf(), norte_config::Layer::User),
            ],
        };
        let cfg = AiConfig::load_from(&layers).expect("load");
        assert!(cfg.enabled);
        let x = cfg
            .providers
            .iter()
            .find(|p| p.name == "x")
            .expect("provider x present");
        assert_eq!(x.model, "new");
    }

    #[test]
    fn config_default_disabled() {
        let c = AiConfig::default();
        assert!(!c.enabled && !c.local_only && c.denied_prefixes.is_empty());
        assert!(!load_from_toml("").expect("empty").enabled);
    }

    #[test]
    fn config_parses_full_section() {
        let c = load_from_toml(
            "[ai]\nenabled = true\nlocal_only = true\n\
             denied_prefixes = [\"file:///secret\", \"file:///home/o/.ssh\"]\n\
             rename_provider = \"local\"\n",
        )
        .expect("parses");
        assert!(c.enabled && c.local_only);
        assert_eq!(c.denied_prefixes.len(), 2);
        assert_eq!(c.rename_provider.as_deref(), Some("local"));
    }

    #[test]
    fn config_invalid_prefix_is_error() {
        // norte-config validates `denied_prefixes` in its own loader: the
        // variant now is an `AiConfigError::Io`-wrapped `ConfigError`,
        // not `AiConfigError::BadPrefix` (that variant remains documented
        // but is not constructed from here — see its rustdoc).
        assert!(load_from_toml("[ai]\ndenied_prefixes = [\"not-a-url\"]\n").is_err());
        assert!(load_from_toml("[ai]\nenabled = \"yes\"\n").is_err());
    }

    #[test]
    fn embed_provider_config_named_else_single_else_none() {
        let mut cfg = AiConfig::default();
        assert!(cfg.embed_provider_config().is_none());
        cfg.providers.push(AiProviderConfig {
            name: "only".into(),
            kind: "ollama".into(),
            model: "nomic-embed-text".into(),
            base_url: None,
        });
        // a single provider with no explicit name ⇒ that one
        assert_eq!(cfg.embed_provider_config().unwrap().name, "only");
        cfg.providers.push(AiProviderConfig {
            name: "b".into(),
            kind: "ollama".into(),
            model: "x".into(),
            base_url: None,
        });
        // two and no name ⇒ None (ambiguous)
        assert!(cfg.embed_provider_config().is_none());
        cfg.embed_provider = Some("b".into());
        assert_eq!(cfg.embed_provider_config().unwrap().name, "b");
    }

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("wire")
    }

    #[test]
    fn gate_disabled_rejects() {
        let c = AiConfig::default();
        let g = AiGate::new(&c);
        assert_eq!(
            g.check(AiOp::Rename, true, &[&vp("file:///d")]),
            Err(AiDenied::Disabled)
        );
    }

    #[test]
    fn gate_local_only_rejects_remote_allows_local() {
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
        // Under the prefix: rejected.
        assert_eq!(
            g.check(AiOp::Rename, true, &[&vp("file:///home/o/secret/k")]),
            Err(AiDenied::DeniedPath)
        );
        // Sibling with a common string prefix BUT not under the segment: OK.
        assert!(
            g.check(AiOp::Rename, true, &[&vp("file:///home/o/secrets/x")])
                .is_ok()
        );
    }

    #[test]
    fn prompt_rejects_non_utf8_name() {
        let names = [seg(b"ok.txt"), seg(b"caf\xe9\xff")];
        assert!(matches!(
            build_rename_prompt(&names, "lower"),
            Err(AiError::Protocol(_))
        ));
    }

    #[test]
    fn prompt_includes_instruction_and_names() {
        let req =
            build_rename_prompt(&[seg(b"A.TXT"), seg(b"B.TXT")], "lowercase").expect("prompt");
        assert!(req.system.is_some());
        let u = &req.messages[0].content;
        assert!(u.contains("lowercase") && u.contains("A.TXT") && u.contains("B.TXT"));
    }

    #[test]
    fn validate_plan_valid() {
        let inputs = [seg(b"A.TXT"), seg(b"B.TXT")];
        let plan = validate_rename_reply(
            r#"[{"from":"A.TXT","to":"a.txt"},{"from":"B.TXT","to":"b.txt"}]"#,
            &inputs,
        )
        .expect("plan");
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].to.as_bytes(), b"a.txt");
    }

    /// **The plan arrives in TWO shapes, and validation is the same.**
    ///
    /// The object is returned by whoever honored the typed output contract;
    /// the bare array, by whoever only had the prompt. That both are valid
    /// is what makes the contract an improvement and not a break for Ollama
    /// or for an Anthropic model without structured output.
    #[test]
    fn the_envelope_and_the_bare_array_give_the_same_plan() {
        let inputs = [seg(b"A.TXT"), seg(b"B.TXT")];
        let from_contract = validate_rename_reply(
            r#"{"renames":[{"from":"A.TXT","to":"a.txt"},{"from":"B.TXT","to":"b.txt"}]}"#,
            &inputs,
        )
        .expect("plan from contract");
        let from_prompt = validate_rename_reply(
            r#"[{"from":"A.TXT","to":"a.txt"},{"from":"B.TXT","to":"b.txt"}]"#,
            &inputs,
        )
        .expect("plan from prompt");
        assert_eq!(from_contract.entries.len(), 2);
        assert_eq!(from_contract.entries.len(), from_prompt.entries.len());
        for (a, b) in from_contract.entries.iter().zip(&from_prompt.entries) {
            assert_eq!(a.from.as_bytes(), b.from.as_bytes());
            assert_eq!(a.to.as_bytes(), b.to.as_bytes());
        }
    }

    /// **A hostile envelope with the contract does not relax a single
    /// rule.**
    ///
    /// This is half the security of all of this: structured output reduces
    /// format errors and says nothing about CONTENT. A `to` with traversal
    /// is perfectly valid JSON against the schema — the schema cannot
    /// express "no `..`" — so what rejects it is still local validation,
    /// whether the provider honored the contract or not.
    #[test]
    fn a_hostile_envelope_is_rejected_the_same() {
        let inputs = [seg(b"A")];
        for body in [
            r#"{"renames":[{"from":"A","to":"../outside"}]}"#,
            r#"{"renames":[{"from":"A","to":"a/b"}]}"#,
            r#"{"renames":[{"from":"A","to":"..\\evil"}]}"#,
            r#"{"renames":[{"from":"A","to":"!"}]}"#,
            r#"{"renames":[{"from":"Z","to":"z"}]}"#,
        ] {
            assert!(
                validate_rename_reply(body, &inputs).is_err(),
                "slipped through: {body}"
            );
        }
    }

    /// The plan asks for the contract, and the contract is what native
    /// mechanisms accept: object root, `additionalProperties: false`, and
    /// without the string constraints Anthropic's structured output
    /// rejects (`minLength`, `maxLength`, `pattern`).
    #[test]
    fn the_prompt_carries_a_contract_the_providers_accept() {
        let req = build_rename_prompt(&[seg(b"a.txt")], "lower").expect("prompt");
        let c = req.json_schema.expect("the plan asks for typed output");
        assert_eq!(c.name, "norte_rename_plan");
        assert_eq!(c.schema["type"], "object");
        assert_eq!(c.schema["additionalProperties"], false);
        assert_eq!(
            c.schema["properties"]["renames"]["items"]["additionalProperties"],
            false
        );
        let text = c.schema.to_string();
        for forbidden in ["minLength", "maxLength", "pattern", "minimum", "maximum"] {
            assert!(
                !text.contains(forbidden),
                "`{forbidden}` gets the whole schema rejected"
            );
        }
    }

    #[test]
    fn validate_nonexistent_from_is_error() {
        let inputs = [seg(b"A.TXT")];
        assert!(validate_rename_reply(r#"[{"from":"Z.TXT","to":"z"}]"#, &inputs).is_err());
    }

    #[test]
    fn validate_to_with_traversal_is_error() {
        let inputs = [seg(b"A")];
        assert!(validate_rename_reply(r#"[{"from":"A","to":"../x"}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":"a/b"}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":".."}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":"!"}]"#, &inputs).is_err());
        // security MINOR #M4: backslash = traversal on Windows.
        assert!(validate_rename_reply(r#"[{"from":"A","to":"..\\evil"}]"#, &inputs).is_err());
        assert!(validate_rename_reply(r#"[{"from":"A","to":"a\\b"}]"#, &inputs).is_err());
    }

    #[test]
    fn validate_duplicate_destination_is_error() {
        let inputs = [seg(b"A"), seg(b"B")];
        assert!(
            validate_rename_reply(r#"[{"from":"A","to":"x"},{"from":"B","to":"x"}]"#, &inputs)
                .is_err()
        );
    }

    #[test]
    fn validate_consistent_swap_ok() {
        // a→b, b→a: each `to` collides with an existing one BUT both are moved.
        let inputs = [seg(b"a"), seg(b"b")];
        let plan =
            validate_rename_reply(r#"[{"from":"a","to":"b"},{"from":"b","to":"a"}]"#, &inputs)
                .expect("valid swap");
        assert_eq!(plan.entries.len(), 2);
    }

    #[test]
    fn validate_collision_with_not_renamed_is_error() {
        // A→B but B exists and is NOT renamed: collision.
        let inputs = [seg(b"A"), seg(b"B")];
        assert!(validate_rename_reply(r#"[{"from":"A","to":"B"}]"#, &inputs).is_err());
    }

    #[test]
    fn validate_broken_json_is_protocol() {
        let inputs = [seg(b"A")];
        assert!(matches!(
            validate_rename_reply("i am not json", &inputs),
            Err(AiError::Protocol(_))
        ));
    }

    /// **A destination that leaves the directory is rejected**, no matter
    /// how it arrives: it is phase 8's security property, and the provider
    /// is on the other side of a network.
    #[test]
    fn organize_rejects_everything_that_leaves_the_directory() {
        let inputs = [seg(b"a.txt")];
        for bad in [
            "../outside.txt",
            "x/../../outside.txt",
            "/etc/passwd",
            "",
            "x//y.txt",
            "x/",
            "./x.txt",
            "..\\outside.txt",
            "x/..\\y.txt",
            "!/x.txt",
        ] {
            let reply = serde_json::json!({
                "moves": [{"current": "a.txt", "proposed_rel": bad}]
            })
            .to_string();
            assert!(
                matches!(
                    validate_organize_reply(&reply, &inputs),
                    Err(AiError::Protocol(_))
                ),
                "«{bad}» had to be rejected"
            );
        }
    }

    /// A `current` that was not among the sent names is a plan about
    /// another directory.
    #[test]
    fn organize_rejects_a_source_that_was_not_sent() {
        let inputs = [seg(b"a.txt")];
        let reply = serde_json::json!({
            "moves": [{"current": "other.txt", "proposed_rel": "x/other.txt"}]
        })
        .to_string();
        assert!(matches!(
            validate_organize_reply(&reply, &inputs),
            Err(AiError::Protocol(_))
        ));
    }

    /// And a plan that contradicts itself — twice the same source, or
    /// twice the same destination — does not pass either: it cannot be
    /// carried out in full.
    #[test]
    fn organize_rejects_a_plan_that_contradicts_itself() {
        let inputs = [seg(b"a.txt"), seg(b"b.txt")];
        let same_source = serde_json::json!({
            "moves": [
                {"current": "a.txt", "proposed_rel": "x/1.txt"},
                {"current": "a.txt", "proposed_rel": "x/2.txt"}
            ]
        })
        .to_string();
        assert!(matches!(
            validate_organize_reply(&same_source, &inputs),
            Err(AiError::Protocol(_))
        ));
        let same_destination = serde_json::json!({
            "moves": [
                {"current": "a.txt", "proposed_rel": "x/1.txt"},
                {"current": "b.txt", "proposed_rel": "x/1.txt"}
            ]
        })
        .to_string();
        assert!(matches!(
            validate_organize_reply(&same_destination, &inputs),
            Err(AiError::Protocol(_))
        ));
    }

    /// A good plan passes, with subdirectories and all — which is the
    /// point of the phase.
    #[test]
    fn organize_accepts_a_plan_with_subdirectories() {
        let inputs = [seg(b"invoice.pdf"), seg(b"note.txt")];
        let reply = serde_json::json!({
            "moves": [
                {"current": "invoice.pdf", "proposed_rel": "invoices/2026/march.pdf"},
                {"current": "note.txt", "proposed_rel": "notes/note.txt"}
            ]
        })
        .to_string();
        let plan = validate_organize_reply(&reply, &inputs).expect("valid plan");
        assert_eq!(plan.moves.len(), 2);
        assert_eq!(plan.moves[0].proposed_rel, "invoices/2026/march.pdf");
    }

    /// The bare array too, as in the rename plan: a provider that ignores
    /// the contract is still useful.
    #[test]
    fn organize_accepts_the_bare_array() {
        let inputs = [seg(b"a.txt")];
        let reply = r#"[{"current": "a.txt", "proposed_rel": "x/a.txt"}]"#;
        let plan = validate_organize_reply(reply, &inputs).expect("valid plan");
        assert_eq!(plan.moves.len(), 1);
    }

    /// A name that is not UTF-8 does not leave the machine: the prompt
    /// refuses to build, instead of sending a replacement the provider
    /// cannot return correctly.
    #[test]
    fn organize_does_not_send_a_name_that_is_not_text() {
        let inputs = [seg(b"caf\xff")];
        assert!(matches!(
            build_organize_prompt(&inputs, "organize"),
            Err(AiError::Protocol(_))
        ));
    }
}
