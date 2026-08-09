//! `keymap.toml` as data: the raw binding lists, the layer file, and the
//! per-context merge order (ADR 0006/0007).

use serde::Deserialize;

use super::{KeymapError, presets};

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

impl RawSection {
    /// The name of the first of the three lists that declares anything, or
    /// `None` if the section is empty. Used by the `dialog_from` rule:
    /// "declares a `[dialog]` of its own" has to mean ANY list, not just
    /// `keymap`, or the same question could be answered twice through
    /// `append_keymap` — and the diagnostic has to name the list it found, or
    /// it sends the reader looking for a `keymap` that is not there.
    fn declared_list(&self) -> Option<&'static str> {
        if !self.keymap.is_empty() {
            Some("keymap")
        } else if !self.prepend_keymap.is_empty() {
            Some("prepend_keymap")
        } else if !self.append_keymap.is_empty() {
            Some("append_keymap")
        } else {
            None
        }
    }

    /// Whether the section declares no binding at all, in any of its three
    /// lists (see [`RawSection::declared_list`]). Only the tests ask it this
    /// way round — production wants the NAME of the offending list, for the
    /// diagnostic — so it is gated rather than left as dead code.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.declared_list().is_none()
    }
}

/// A parsed `keymap.toml` preset or user layer.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapFile {
    /// Whether a numeric prefix multiplies the next command (`5j`). Opt-in per
    /// PRESET, and `vim` is the only bundled preset that takes the option up:
    /// vi's counts are the reason vi is imitated at all. Every other original
    /// spends its digits elsewhere and would have them stolen — `orthodox`,
    /// `cua`, Total Commander, Krusader and Norton plainly, and Far too: Far
    /// has no numeric prefix, it binds `Ctrl+1`..`Ctrl+0` to panel view modes.
    // K2b rule 3. The K2a version of this doc said "vim and far set it because
    // their originals have counts", which was wrong about Far — kept as a `//`
    // note because the `///` text above is published verbatim in
    // `docs/schema/keymap.schema.json`, where our own history is noise.
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
    /// The preset whose `[dialog]` section this one adopts. norte's dialogs are
    /// norte's, not the imitated program's: a Total Commander user expects TC's
    /// panel keys, not a TC confirmation dialog that TC never had. Only legal in
    /// a PRESET (a user layer that set it would silently redefine every overlay
    /// key — the same reason `counts` is refused there), only one level deep, and
    /// only naming a bundled preset. Declaring it together with a `[dialog]`
    /// section of your own is an error rather than a precedence puzzle.
    //
    // Notes for us, deliberately NOT rustdoc: this text ships as the schema
    // `description` in `docs/schema/keymap.schema.json`, which is the only
    // documentation of `keymap.toml` a third party has (ADR 0045).
    //
    // `parse_keymap` resolves the key and then LEAVES IT SET, so the value is
    // afterwards provenance rather than input — which is what lets
    // `check_layer_keys` still refuse it on a `KeymapFile` that did not come
    // through `parse_keymap_layer`.
    //
    // `skip_serializing_if` is inert for serde here (`KeymapFile` derives only
    // `Deserialize`); it is load-bearing for schemars, where it is what keeps
    // `"default": null` out of the published property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) dialog_from: Option<String>,
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

/// The bare TOML parse, WITHOUT resolving `dialog_from`. It exists so that
/// [`parse_keymap`] can read the inherited preset without calling itself:
/// inheritance is one level, and with no recursion there is no depth to
/// bound — not even a `dialog_from` that names its own file can loop.
/// Swapping this call for [`parse_keymap`] is the change the
/// `un_dialog_from_que_se_apunta_a_si_mismo_no_recursa` test exists to catch.
fn parse_raw(s: &str) -> Result<KeymapFile, KeymapError> {
    toml::from_str(s).map_err(|e| KeymapError::Toml(toml_diag(s, &e)))
}

/// Parses a PRESET's `keymap.toml` and resolves its `dialog_from` (see
/// `KeymapFile::dialog_from`): the named preset's `[dialog]` section is
/// copied in, so that every later reader sees an ordinary [`KeymapFile`] and
/// need not know inheritance exists. User and project layers go through
/// [`parse_keymap_layer`] instead, which refuses the key.
///
/// ```
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
///
/// // A preset that says nothing about overlays still gets norte's.
/// let tc = parse_keymap("dialog_from = \"orthodox\"\n\n[pane]\nkeymap = []\n").unwrap();
/// let eff = Effective::build_for(&tc, &[], &["dialog.approve"], Screen::Dialog).unwrap();
/// assert!(eff.bindings().iter().any(|(_, run)| *run == "dialog.approve"));
///
/// // Naming something that is not a bundled preset is a load error, not silence.
/// assert!(parse_keymap("dialog_from = \"totalcommander\"\n").is_err());
/// ```
///
/// # Errors
/// - [`KeymapError::Toml`] if it does not parse or has unknown keys.
/// - [`KeymapError::DialogFromAndDialog`] if it declares both.
/// - [`KeymapError::UnknownDialogFrom`] if it names a preset that does not exist.
/// - [`KeymapError::DialogFromChain`] if the named preset inherits in turn.
pub fn parse_keymap(s: &str) -> Result<KeymapFile, KeymapError> {
    let mut file = parse_raw(s)?;
    resolve_dialog_from(&mut file, presets::source)?;
    Ok(file)
}

/// Parses a `keymap.toml` that is a user or project LAYER: identical to
/// [`parse_keymap`] except that `dialog_from` is REFUSED here instead of
/// resolved. A layer is never a preset, so nothing is ever copied into one —
/// which is a stronger statement than "the copy turns out to be unreachable",
/// and it is what keeps the diagnostic honest: the load error names
/// `dialog_from`, the key the user actually wrote, instead of the `keymap`
/// list the resolver would otherwise have put there
/// (`config::load_keymap_layer` checks [`KeymapFile::has_full_keymap`] right
/// after parsing, and that reads `dialog.keymap`).
///
/// `check_layer_keys` carries the same refusal, and keeps carrying it:
/// this function is the door every real layer comes through, but a caller
/// that builds a [`KeymapFile`] some other way and passes it as a layer must
/// still be told no.
///
/// # Errors
/// - [`KeymapError::Toml`] if it does not parse or has unknown keys.
/// - [`KeymapError::WrongLayerKey`] if it declares `dialog_from`.
pub fn parse_keymap_layer(s: &str) -> Result<KeymapFile, KeymapError> {
    let file = parse_raw(s)?;
    if file.dialog_from.is_some() {
        return Err(KeymapError::WrongLayerKey {
            layer: "usuario",
            key: "dialog_from",
        });
    }
    Ok(file)
}

/// The resolution of [`KeymapFile::dialog_from`], with the preset catalogue
/// INJECTED. [`parse_keymap`] passes it [`presets::source`]; the tests pass
/// inline sources, which is the only way to exercise the "chain" and
/// "self-reference" cases while no bundled preset inherits yet.
fn resolve_dialog_from(
    file: &mut KeymapFile,
    lookup: impl Fn(&str) -> Option<&'static str>,
) -> Result<(), KeymapError> {
    let Some(name) = file.dialog_from.clone() else {
        return Ok(());
    };
    if let Some(list) = file.dialog.declared_list() {
        return Err(KeymapError::DialogFromAndDialog { name, list });
    }
    let src = lookup(&name).ok_or_else(|| KeymapError::UnknownDialogFrom {
        name: name.clone(),
        // Built from `NAMES` so the list cannot drift when K2b's four land.
        known: presets::NAMES.join(", "),
    })?;
    // `parse_raw`, NOT `parse_keymap`: one level and no more. Chaining would
    // make the effective `[dialog]` depend on a hop nobody sees when reading
    // the file, which is exactly the opacity ADR 0045 rejects. It is also
    // what makes the loop impossible rather than merely bounded — a
    // `dialog_from` naming its own file stops at the chain check below
    // instead of recursing.
    let parent = parse_raw(src)?;
    if let Some(then) = parent.dialog_from {
        return Err(KeymapError::DialogFromChain { name, then });
    }
    file.dialog = parent.dialog;
    Ok(())
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
        // Same argument as `counts`, and one notch sharper: `dialog_from` in a
        // layer would replace the WHOLE overlay context — every confirmation,
        // approval and overwrite key at once — from one line that names no
        // key.
        //
        // The layer that came through `parse_keymap_layer` (every real one,
        // via `config::load_keymap_layer`) has already been refused there, so
        // this is the second lock: a caller that built its `KeymapFile` with
        // `parse_keymap` — a test, or a frontend embedding a layer literal —
        // is still told no, and told it here rather than through the copied
        // `keymap` list. Both locks are cheap and neither subsumes the other.
        if layer.dialog_from.is_some() {
            return Err(KeymapError::WrongLayerKey {
                layer: "usuario",
                key: "dialog_from",
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

#[cfg(test)]
mod dialog_from_tests {
    use super::{
        KeymapError, KeymapFile, check_layer_keys, parse_keymap, parse_keymap_layer,
        resolve_dialog_from,
    };

    /// Un preset importado en línea: el que traerán las tareas 2 y 3, sin el
    /// fichero. Nada aquí depende de que exista `total-commander.toml`.
    const IMPORTADO: &str = r#"
dialog_from = "orthodox"

[pane]
keymap = [{ on = ["f5"], run = "pane.copy" }]
"#;

    /// El `[dialog]` de un [`super::RawSection`] como pares comparables: una
    /// comparación por LONGITUD aceptaría una copia parcial, o la sección del
    /// preset equivocado si midiese lo mismo.
    fn pares(s: &super::RawSection) -> Vec<(Vec<String>, String)> {
        s.keymap
            .iter()
            .map(|b| (b.on.clone(), b.run.clone()))
            .collect()
    }

    /// Lo que compra la clave: el preset importado NO copia las 25 líneas de
    /// `[dialog]` y aun así sale del parseo con el contexto puesto — el mismo,
    /// binding a binding.
    #[test]
    fn heredar_puebla_el_contexto_dialog() {
        let kf = parse_keymap(IMPORTADO).expect("el preset importado parsea");
        let orthodox = parse_keymap(super::presets::ORTHODOX).expect("orthodox parsea");
        assert!(!kf.dialog.keymap.is_empty(), "[dialog] quedó vacío");
        assert_eq!(
            pares(&kf.dialog),
            pares(&orthodox.dialog),
            "el [dialog] heredado no es el de orthodox"
        );
        let heredado: Vec<&str> = kf.dialog.keymap.iter().map(|b| b.run.as_str()).collect();
        assert!(heredado.contains(&"dialog.approve"), "{heredado:?}");
        // El campo SIGUE puesto tras resolver: es lo que `check_layer_keys`
        // mira para negárselo a una capa.
        assert_eq!(kf.dialog_from.as_deref(), Some("orthodox"));
        // Y lo suyo no se toca.
        assert_eq!(kf.pane.keymap.len(), 1);
    }

    /// Dos respuestas a la misma pregunta. Cuenta CUALQUIERA de las tres
    /// listas, no solo `keymap`.
    #[test]
    fn heredar_y_declarar_dialog_a_la_vez_es_error() {
        for lista in ["keymap", "prepend_keymap", "append_keymap"] {
            let src = format!(
                "dialog_from = \"orthodox\"\n\n[dialog]\n{lista} = [{{ on = [\"y\"], run = \"dialog.deny\" }}]\n"
            );
            let e = parse_keymap(&src)
                .err()
                .unwrap_or_else(|| panic!("{lista}: se aceptó"));
            assert!(
                matches!(
                    &e,
                    KeymapError::DialogFromAndDialog { name, list }
                        if name == "orthodox" && *list == lista
                ),
                "{lista}: {e:?}"
            );
            // El mensaje nombra la lista que encontró, no `keymap` por defecto.
            assert!(e.to_string().contains(lista), "{lista}: {e}");
        }
    }

    /// Un nombre que no existe es un typo, y el mensaje lo dice con la clave
    /// y el valor.
    #[test]
    fn heredar_de_un_preset_inexistente_es_error() {
        let e = parse_keymap("dialog_from = \"totalcommander\"\n")
            .expect_err("se aceptó un preset inexistente");
        assert!(
            matches!(&e, KeymapError::UnknownDialogFrom { name, .. } if name == "totalcommander"),
            "{e:?}"
        );
        let msg = e.to_string();
        assert!(msg.contains("dialog_from"), "{msg}");
        assert!(msg.contains("totalcommander"), "{msg}");
        // Y dice cuáles SÍ valen, tomados de `NAMES` para que no se queden
        // atrás cuando K2b registre los cuatro importados.
        for name in super::presets::NAMES {
            assert!(msg.contains(name), "{msg} no ofrece {name}");
        }
    }

    /// Un solo nivel: si el preset nombrado hereda a su vez, se para. Se
    /// inyecta el catálogo porque ningún preset de fábrica hereda todavía —
    /// y el día que uno lo haga (tarea 2), esta regla ya está puesta.
    #[test]
    fn una_cadena_de_herencia_es_error() {
        let mut kf: KeymapFile =
            toml::from_str("dialog_from = \"intermedio\"\n").expect("el hijo parsea");
        let e = resolve_dialog_from(&mut kf, |n| {
            (n == "intermedio").then_some("dialog_from = \"orthodox\"\n")
        })
        .expect_err("se aceptó una cadena");
        assert!(
            matches!(
                &e,
                KeymapError::DialogFromChain { name, then } if name == "intermedio" && then == "orthodox"
            ),
            "{e:?}"
        );
        assert!(
            kf.dialog.is_empty(),
            "una cadena rota no puede dejar rastro"
        );
    }

    /// El `parse_raw` de `resolve_dialog_from` es LOAD-BEARING, y esto es lo
    /// único que lo sostiene: con `parse_keymap` en su lugar —la
    /// «simplificación» obvia, sobre todo el día que alguien quiera dos
    /// niveles— un preset que se nombra a sí mismo recursaría hasta desbordar
    /// la pila ANTES de llegar al chequeo de cadena, y desbordar la pila es un
    /// abort, no un error de carga. El test de la cadena NO cubre esto: con
    /// recursión seguiría pasando en verde.
    #[test]
    fn un_dialog_from_que_se_apunta_a_si_mismo_no_recursa() {
        let mut kf: KeymapFile =
            toml::from_str("dialog_from = \"bucle\"\n").expect("el fichero parsea");
        let e = resolve_dialog_from(&mut kf, |n| {
            (n == "bucle").then_some("dialog_from = \"bucle\"\n")
        })
        .expect_err("un auto-préstamo tiene que parar");
        assert!(
            matches!(
                &e,
                KeymapError::DialogFromChain { name, then } if name == "bucle" && then == "bucle"
            ),
            "{e:?}"
        );
    }

    /// La puerta por la que entra toda capa real (`config::load_keymap_layer`)
    /// la RECHAZA antes de resolver nada: una capa nunca es un preset, así que
    /// no se le copia un `[dialog]` que luego haya que declarar inalcanzable.
    #[test]
    fn parse_keymap_layer_rechaza_dialog_from_sin_resolverlo() {
        let e = parse_keymap_layer("dialog_from = \"orthodox\"\n")
            .expect_err("una capa no puede heredar [dialog]");
        assert!(
            matches!(
                e,
                KeymapError::WrongLayerKey {
                    layer: "usuario",
                    key: "dialog_from"
                }
            ),
            "{e:?}"
        );
        // Y lo normal sigue pasando por la misma puerta.
        let capa = parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
        )
        .expect("una capa normal parsea");
        assert!(capa.dialog.is_empty());
    }

    /// Segunda cerradura: un `KeymapFile` construido con `parse_keymap` y
    /// pasado como capa —un test, o un frontend con una capa literal— también
    /// se rechaza, y por el nombre de la clave escrita.
    #[test]
    fn dialog_from_en_una_capa_es_wrong_layer_key() {
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
            .expect("preset");
        let capa = parse_keymap("dialog_from = \"orthodox\"\n").expect("la capa parsea");
        let e = check_layer_keys(&preset, std::slice::from_ref(&capa))
            .expect_err("se aceptó dialog_from en una capa");
        assert!(
            matches!(
                e,
                KeymapError::WrongLayerKey {
                    layer: "usuario",
                    key: "dialog_from"
                }
            ),
            "{e:?}"
        );
    }

    /// Y la capa de PROYECTO —contenido ajeno— tampoco cuela los bindings
    /// heredados por la puerta de atrás: `merge_ctx` solo lee
    /// `prepend_keymap`/`append_keymap` de una capa, así que la copia queda
    /// estructuralmente inalcanzable aunque el diagnóstico siga andando.
    #[test]
    fn una_capa_de_proyecto_no_contrabandea_el_dialog_heredado() {
        let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
            .expect("preset");
        let mut capa = parse_keymap("dialog_from = \"orthodox\"\n").expect("la capa parsea");
        capa.mark_project();
        assert!(!capa.dialog.keymap.is_empty(), "la copia sí ocurrió");
        let mut descartados = 0;
        let fusion = super::merged_bindings(
            &preset,
            std::slice::from_ref(&capa),
            super::Screen::Dialog,
            &mut descartados,
        );
        assert!(
            fusion.is_empty(),
            "la capa aportó bindings de dialog: {fusion:?}"
        );
    }

    /// `orthodox` no usa la clave y no cambia por esto.
    #[test]
    fn orthodox_no_hereda_de_nadie() {
        let kf = parse_keymap(super::presets::ORTHODOX).expect("orthodox parsea");
        assert!(kf.dialog_from.is_none());
        assert!(!kf.dialog.keymap.is_empty());
        check_layer_keys(&kf, &[]).expect("orthodox sigue siendo un preset válido");
    }
}
