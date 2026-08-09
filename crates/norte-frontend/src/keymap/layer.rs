//! `keymap.toml` as data: the raw binding lists, the layer file, and the
//! per-context merge order (ADR 0006/0007).

use serde::Deserialize;

use super::KeymapError;

/// One binding as represented in TOML.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(super) struct RawBinding {
    pub(super) on: Vec<String>,
    pub(super) run: String,
}

/// The three binding lists in a section: `keymap` for presets and
/// `prepend_keymap`/`append_keymap` for user layers.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(super) struct RawSection {
    #[serde(default)]
    pub(super) keymap: Vec<RawBinding>,
    #[serde(default)]
    pub(super) prepend_keymap: Vec<RawBinding>,
    #[serde(default)]
    pub(super) append_keymap: Vec<RawBinding>,
}

/// A parsed `keymap.toml` preset or user layer.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapFile {
    /// Whether a numeric prefix multiplies the next command (`5j`). Opt-in per
    /// PRESET: `vim` and `far` set it because their originals have counts;
    /// `orthodox`, `cua`, Total Commander, Krusader and Norton do not, and
    /// turning it on there would steal their digit keys.
    #[serde(default)]
    pub(super) counts: bool,
    #[serde(default)]
    pub(super) global: RawSection,
    #[serde(default)]
    pub(super) pane: RawSection,
    #[serde(default)]
    pub(super) viewer: RawSection,
    /// Contexto `dialog` (H1, issue #24): teclas de modales/overlays
    /// (confirmación, aprobación, popups de navegación…) como keymap de
    /// datos en vez de handlers ad hoc — la ayuda generada nunca puede
    /// desincronizarse de un rebind. Se fusiona con `global` igual que
    /// `pane`/`viewer` (ver [`Screen::Dialog`]).
    #[serde(default)]
    pub(super) dialog: RawSection,
    /// `true` si esta capa es la de PROYECTO (`./.norte`) — contenido
    /// potencialmente AJENO (viene con un repo clonado) que se carga SIN
    /// trust. Un keymap de proyecto NO puede bindear `lua:`:
    /// [`Effective::build_for`](super::Effective::build_for) descarta esos
    /// bindings (contados en
    /// [`Effective::discarded_lua_bindings`](super::Effective::discarded_lua_bindings))
    /// — rebindear una tecla común a
    /// un comando del `init.lua` del USUARIO (sin sandbox) sería ejecución
    /// dirigida por el repo sin confirmación alguna. No viene del TOML
    /// (`serde(skip)`): lo marca `load_keymap_layer` (`config.rs`) leyendo
    /// el [`Layer`](norte_config::Layer) del `dir` que trae cada capa
    /// (ADR 0035: el kind viaja POR DIR en `Layers`, ya no se infiere por
    /// posición — deuda #75 cerrada).
    #[serde(skip)]
    project: bool,
}

impl KeymapFile {
    /// ¿Define `keymap` (lista completa de preset)? Las CAPAS de usuario
    /// no lo admiten — el diagnóstico con archivo vive en `config::load`.
    #[must_use]
    pub fn has_full_keymap(&self) -> bool {
        !self.global.keymap.is_empty()
            || !self.pane.keymap.is_empty()
            || !self.viewer.keymap.is_empty()
            || !self.dialog.keymap.is_empty()
    }

    /// Marca esta capa como la de PROYECTO (ver el campo `project`): sus
    /// bindings `lua:` se descartan al fusionar. La llama `config::load`
    /// con el `keymap.toml` de `./.norte`.
    pub fn mark_project(&mut self) {
        self.project = true;
    }

    /// ¿Es la capa de proyecto? (ver [`Self::mark_project`]).
    #[must_use]
    pub fn is_project(&self) -> bool {
        self.project
    }
}

/// Pantalla activa: decide qué contexto específico se fusiona con
/// `global` (ADR 0006; el stack crece con la UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Los dos panes (contexto `pane`).
    Browse,
    /// El viewer (contexto `viewer`, fase 7).
    Viewer,
    /// Modales/overlays (contexto `dialog`, H1 — issue #24): confirmación,
    /// aprobación, popups de navegación… cada overlay declara su propio
    /// ALLOWLIST de qué `dialog.*` comandos soporta (la semántica de
    /// seguridad vive en código, no aquí).
    Dialog,
}

/// Diagnóstico compacto de un error de parseo TOML: `"line N: msg"` si el
/// error trae span, o solo el mensaje si no (errores semánticos). Copia
/// LOCAL de `norte_tui::config::toml_diag` — el motor no depende de la TUI.
fn toml_diag(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(s) => {
            let line = 1 + raw[..s.start.min(raw.len())].matches('\n').count();
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

/// Parsea un `keymap.toml`.
///
/// # Errors
/// [`KeymapError::Toml`] si no parsea o hay claves desconocidas.
pub fn parse_keymap(s: &str) -> Result<KeymapFile, KeymapError> {
    toml::from_str(s).map_err(|e| KeymapError::Toml(toml_diag(s, &e)))
}

/// Where a merged binding comes from: the shared preset or a user/project
/// layer. Since K1 the per-binding verdict no longer depends on it (a name
/// absent from `known_commands` is looked up in the shared catalogue, the same
/// way for both) — it survives because `merge_ctx` needs it to discard the
/// `lua:` bindings of a project layer, and because a future rule that DOES
/// depend on provenance would otherwise have to re-derive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    Preset,
    Layer,
}

/// Fusión de un contexto (ADR 0006/0007): prepends de capa superior primero
/// (ganan), luego el preset, luego los appends (superiores antes). Los
/// bindings `lua:` de una capa de PROYECTO se DESCARTAN aquí, contados en
/// `discarded_lua` (seguridad: ver [`KeymapFile::mark_project`] — el
/// keymap de un repo ajeno no puede dirigir la ejecución de comandos Lua).
/// Cada binding se etiqueta con su [`Origin`] (preset vs. capa).
fn merge_ctx<'a>(
    preset: &'a KeymapFile,
    layers: &'a [KeymapFile],
    get: fn(&KeymapFile) -> &RawSection,
    discarded_lua: &mut usize,
) -> Vec<(&'a RawBinding, Origin)> {
    let mut out = Vec::new();
    let mut push =
        |layer_project: bool, b: &'a RawBinding, out: &mut Vec<(&'a RawBinding, Origin)>| {
            if layer_project && b.run.starts_with("lua:") {
                *discarded_lua += 1;
            } else {
                out.push((b, Origin::Layer));
            }
        };
    for l in layers.iter().rev() {
        for b in &get(l).prepend_keymap {
            push(l.project, b, &mut out);
        }
    }
    out.extend(get(preset).keymap.iter().map(|b| (b, Origin::Preset)));
    for l in layers.iter().rev() {
        for b in &get(l).append_keymap {
            push(l.project, b, &mut out);
        }
    }
    out
}

/// Each layer admits ONLY its own lists (phase-4 review): a preset defines
/// `keymap`; a user/project layer defines `prepend_keymap`/`append_keymap`.
/// Silently dropping the wrong list would be the "weird behavior" the ADR
/// forbids. Returns the first offending layer/key (there is at most one kind
/// of mistake worth reporting per source). Shared by
/// [`Effective::build_for`](super::Effective::build_for) (fails on it) and
/// [`Effective::build_diagnostics`](super::Effective::build_diagnostics)
/// (reports it and keeps walking — the bindings still merge from the CORRECT
/// lists via `merge_ctx`).
pub(super) fn check_layer_keys(
    preset: &KeymapFile,
    layers: &[KeymapFile],
) -> Result<(), KeymapError> {
    for section in [&preset.global, &preset.pane, &preset.viewer, &preset.dialog] {
        if !(section.prepend_keymap.is_empty() && section.append_keymap.is_empty()) {
            return Err(KeymapError::WrongLayerKey {
                layer: "preset",
                key: "prepend_keymap/append_keymap",
            });
        }
    }
    for layer in layers {
        // The count POLICY is the preset's. A layer that could turn counts on
        // would silently change what EVERY digit key means — the "weird
        // behaviour" ADR 0006 forbids, so it is a load error like any other
        // wrong key.
        if layer.counts {
            return Err(KeymapError::WrongLayerKey {
                layer: "usuario",
                key: "counts",
            });
        }
        for section in [&layer.global, &layer.pane, &layer.viewer, &layer.dialog] {
            if !section.keymap.is_empty() {
                return Err(KeymapError::WrongLayerKey {
                    layer: "usuario",
                    key: "keymap",
                });
            }
        }
    }
    Ok(())
}

/// The merged, ordered binding list for `screen` (screen-specific context
/// before `global`, ADR 0006), with project-layer `lua:` bindings discarded
/// and counted. Shared by
/// [`Effective::build_for`](super::Effective::build_for) and
/// [`Effective::build_diagnostics`](super::Effective::build_diagnostics) so the
/// merge order is defined once.
pub(super) fn merged_bindings<'a>(
    preset: &'a KeymapFile,
    layers: &'a [KeymapFile],
    screen: Screen,
    discarded_lua_bindings: &mut usize,
) -> Vec<(&'a RawBinding, Origin)> {
    let specific: fn(&KeymapFile) -> &RawSection = match screen {
        Screen::Browse => |f| &f.pane,
        Screen::Viewer => |f| &f.viewer,
        Screen::Dialog => |f| &f.dialog,
    };
    merge_ctx(preset, layers, specific, discarded_lua_bindings)
        .into_iter()
        .chain(merge_ctx(
            preset,
            layers,
            |f| &f.global,
            discarded_lua_bindings,
        ))
        .collect()
}
