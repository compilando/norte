//! The EFFECTIVE keymap: layers and contexts merged, validated prefix-free at
//! load time (ADR 0006), immutable afterwards.

use std::collections::HashSet;

use super::chord::{Chord, parse_chord};
use super::layer::{KeymapFile, Origin, RawBinding, Screen, check_layer_keys, merged_bindings};
use super::{KeymapDiagnostic, KeymapError};

/// Keymap EFECTIVO: capas y contextos ya fusionados y validados
/// (prefix-free). Inmutable tras construir; clonable barato (el hot-reload
/// construye uno nuevo y lo cambia entero, ADR 0007).
#[derive(Debug, Clone)]
pub struct Effective {
    bindings: Vec<(Vec<Chord>, String)>,
    /// Bindings `lua:` DESCARTADOS por venir de la capa de proyecto
    /// (seguridad, ver [`KeymapFile::mark_project`]). El frontend lo avisa una
    /// vez (jamás descarte mudo); el mensaje concreto es cosa del frontend.
    discarded_lua_bindings: usize,
}

/// Selects the behavior for a PRESET binding to a command absent from
/// `known_commands`: [`Effective::build_for`] is `Strict` (error),
/// [`Effective::build_for_subset`] is `Lenient` (silently filtered — the
/// frontend implements a subset of the shared preset's commands). Never
/// affects layer bindings — see the `continue` in `build_for_impl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Strictness {
    Strict,
    Lenient,
}

/// Charset de un nombre de comando Lua (`lua:<nombre>`): `[a-z0-9._-]{1,64}`.
/// FUENTE ÚNICA (#88): el motor lo usa para validar el binding `lua:<nombre>`,
/// y el runtime Lua de un frontend con host (la TUI, `norte.command`) lo reusa
/// para validar el nombre registrado — así el charset no puede derivar entre
/// «lo que el keymap acepta» y «lo que el runtime registra».
#[must_use]
pub fn valid_lua_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Validates ONE merged binding. `Ok(Some(binding))` = keep it;
/// `Ok(None)` = a lenient-filtered PRESET binding (a command this frontend
/// does not implement — skipped silently, only in [`Strictness::Lenient`]);
/// `Err` = a defect. Shared by [`Effective::build_for`] (fails on the first
/// `Err`) and [`Effective::build_diagnostics`] (collects every `Err` and
/// keeps walking) — the SINGLE source of the per-binding rules, so the two
/// paths can never drift.
fn check_binding(
    raw: &RawBinding,
    origin: Origin,
    known_commands: &[&str],
    preset_strictness: Strictness,
) -> Result<Option<(Vec<Chord>, String)>, KeymapError> {
    let seq: Vec<Chord> = raw
        .on
        .iter()
        .map(|s| parse_chord(s))
        .collect::<Result<_, _>>()?;
    if seq.is_empty() {
        return Err(KeymapError::EmptySequence {
            run: raw.run.clone(),
        });
    }
    // Esc es la cancelación de secuencia (lo cazó el proptest: un esc
    // no-inicial sería inalcanzable): solo como binding suelto.
    if seq.len() > 1 && seq.iter().any(|c| c.is_bare_esc()) {
        return Err(KeymapError::EscInSequence {
            sequence: format!("{:?}", raw.on),
        });
    }
    // `lua:<nombre>` (M4 Lua, T8): el registro de comandos Lua es DINÁMICO
    // (runtime), así que no se valida contra `known_commands` — solo el
    // charset del nombre (la MISMA `valid_lua_name`, una sola fuente). Un
    // comando lua no registrado al invocar NO es error de keymap: el frontend
    // con host avisa en runtime. Se aplica IGUAL en modo lenient — el
    // filtrado de `build_for_subset` es SOLO por `known_commands` desconocido,
    // jamás una vía para colarse del charset lua:.
    if let Some(lua_name) = raw.run.strip_prefix("lua:") {
        if !valid_lua_name(lua_name) {
            return Err(KeymapError::UnknownCommand {
                run: raw.run.clone(),
            });
        }
    } else if !known_commands.contains(&raw.run.as_str()) {
        // En modo Lenient, SOLO los bindings del `keymap` del preset se
        // filtran en silencio (el frontend no implementa ese comando
        // compartido); un binding de CAPA (prepend/append de usuario o
        // proyecto) sigue siendo estricto — un typo de usuario jamás debe
        // morir en silencio (ADR 0006).
        if preset_strictness == Strictness::Lenient && origin == Origin::Preset {
            return Ok(None);
        }
        return Err(KeymapError::UnknownCommand {
            run: raw.run.clone(),
        });
    }
    Ok(Some((seq, raw.run.clone())))
}

/// Prefix-free: no sequence is a strict prefix of another (ADR 0006 — without
/// timeouts, resolution must be deterministic). Runs over the ALREADY-filtered
/// set — a preset binding skipped in `Lenient` mode must not block a foreign
/// prefix. Returns the first ambiguous pair; shared by both builders.
fn check_prefix_free(bindings: &[(Vec<Chord>, String)]) -> Result<(), KeymapError> {
    for (i, (a, _)) in bindings.iter().enumerate() {
        for (b, _) in bindings.iter().skip(i + 1) {
            let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
            if short.len() < long.len() && long[..short.len()] == short[..] {
                return Err(KeymapError::AmbiguousPrefix {
                    shorter: format!("{short:?}"),
                    longer: format!("{long:?}"),
                });
            }
        }
    }
    Ok(())
}

impl Effective {
    /// Fusiona `preset` + capa opcional de usuario (ADR 0006). Azúcar de
    /// [`Self::build_layered`] con cero o una capa.
    ///
    /// # Errors
    /// Ver [`KeymapError`] — todos son errores de CARGA con diagnóstico.
    pub fn build(
        preset: &KeymapFile,
        user: Option<&KeymapFile>,
        known_commands: &[&str],
    ) -> Result<Self, KeymapError> {
        match user {
            Some(u) => Self::build_layered(preset, std::slice::from_ref(u), known_commands),
            None => Self::build_layered(preset, &[], known_commands),
        }
    }

    /// Fusiona `preset` + N capas de usuario en precedencia ASCENDENTE
    /// (sistema → usuario → proyecto, ADR 0007) y valida (ADR 0006): por
    /// contexto, los `prepend` de capas superiores van primero (ganan),
    /// luego el preset, luego los `append` (superiores antes); entre
    /// contextos, el específico (`pane`) pisa al `global`; el resultado
    /// debe ser prefix-free y con comandos conocidos.
    ///
    /// # Errors
    /// Ver [`KeymapError`] — todos son errores de CARGA con diagnóstico.
    pub fn build_layered(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
    ) -> Result<Self, KeymapError> {
        Self::build_for(preset, layers, known_commands, Screen::Browse)
    }

    /// Fusiona para una pantalla concreta: su contexto específico pisa a
    /// `global` por secuencia exacta (ADR 0006), capas como en
    /// [`Self::build_layered`].
    ///
    /// # Errors
    /// Ver [`KeymapError`].
    pub fn build_for(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Result<Self, KeymapError> {
        Self::build_for_impl(preset, layers, known_commands, screen, Strictness::Strict)
    }

    /// Like [`Effective::build_for`], but PRESET bindings whose command is
    /// not in `known_commands` are skipped instead of failing. For
    /// frontends that implement a subset of the shared presets' commands
    /// (the GUI). User/project LAYERS remain strict.
    ///
    /// # Errors
    /// Same as [`Effective::build_for`], except preset bindings to
    /// non-`lua:` commands absent from `known_commands` are skipped
    /// instead of failing.
    pub fn build_for_subset(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Result<Self, KeymapError> {
        Self::build_for_impl(preset, layers, known_commands, screen, Strictness::Lenient)
    }

    fn build_for_impl(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
        preset_strictness: Strictness,
    ) -> Result<Self, KeymapError> {
        check_layer_keys(preset, layers)?;
        let mut discarded_lua_bindings = 0usize;
        let ordered = merged_bindings(preset, layers, screen, &mut discarded_lua_bindings);

        let mut seen: HashSet<Vec<Chord>> = HashSet::new();
        let mut bindings: Vec<(Vec<Chord>, String)> = Vec::new();
        for (raw, origin) in ordered {
            if let Some((seq, run)) = check_binding(raw, origin, known_commands, preset_strictness)?
            {
                // El primero gana (el orden YA codifica la precedencia).
                if seen.insert(seq.clone()) {
                    bindings.push((seq, run));
                }
            }
        }
        check_prefix_free(&bindings)?;
        Ok(Self {
            bindings,
            discarded_lua_bindings,
        })
    }

    /// Builds the effective keymap for `screen` in a SINGLE pass, collecting
    /// EVERY defect as a [`KeymapDiagnostic`] instead of failing on the first
    /// (as [`Effective::build_for`] does). Unlike the diagnostic loop it
    /// replaces (issue #102), it needs no per-typo rebuild and no anti-DoS
    /// retry cap, and it cannot get stuck on the non-convergent `lua:`-charset
    /// case: a broken `lua:` name is classified directly as
    /// [`KeymapDiagnostic::Structural`] (extending `known_commands` could never
    /// fix it). Strictness matches [`Effective::build_for`] (`Strict`) — the
    /// caller (`norte doctor`) validates against the union of the bundled
    /// presets' commands, so a preset binding is never silently filtered here.
    ///
    /// Findings appear in walk order: wrong-layer-key (if any), then each
    /// binding's defect, then the first ambiguous prefix. An empty result
    /// means the keymap is clean.
    #[must_use]
    pub fn build_diagnostics(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Vec<KeymapDiagnostic> {
        let mut diags = Vec::new();
        if let Err(e) = check_layer_keys(preset, layers) {
            // A wrong layer key does not stop the walk: `merge_ctx` reads the
            // CORRECT lists, so binding-level findings are still worth
            // reporting in the same pass.
            diags.push(KeymapDiagnostic::Structural {
                message: e.to_string(),
            });
        }
        let mut discarded = 0usize;
        let ordered = merged_bindings(preset, layers, screen, &mut discarded);
        let mut seen: HashSet<Vec<Chord>> = HashSet::new();
        let mut bindings: Vec<(Vec<Chord>, String)> = Vec::new();
        for (raw, origin) in ordered {
            match check_binding(raw, origin, known_commands, Strictness::Strict) {
                Ok(Some((seq, run))) => {
                    if seen.insert(seq.clone()) {
                        bindings.push((seq, run));
                    }
                }
                // Unreachable under `Strict` (no lenient filtering), but a
                // filtered binding is simply skipped either way.
                Ok(None) => {}
                Err(KeymapError::UnknownCommand { run })
                    if run.strip_prefix("lua:").is_none_or(valid_lua_name) =>
                {
                    // A PLAIN unknown command (or a well-formed `lua:` name
                    // that just is not in `known` — impossible here since
                    // valid `lua:` names are accepted, so this arm is the
                    // plain case): recoverable.
                    diags.push(KeymapDiagnostic::UnknownCommand { run });
                }
                // The remaining `UnknownCommand` is a `lua:` name that FAILS
                // the charset (`valid_lua_name` — single source): structural,
                // never fixable by extending `known_commands`.
                Err(e) => diags.push(KeymapDiagnostic::Structural {
                    message: e.to_string(),
                }),
            }
        }
        if let Err(e) = check_prefix_free(&bindings) {
            diags.push(KeymapDiagnostic::Structural {
                message: e.to_string(),
            });
        }
        diags
    }

    /// Bindings `lua:` descartados por venir de la capa de PROYECTO (`./
    /// .norte`, sin trust — seguridad, ver [`KeymapFile::mark_project`]).
    /// El caller (main) lo pinta una vez por barra; los rebinds de proyecto
    /// a builtins NO cuentan aquí (siguen funcionando).
    #[must_use]
    pub fn discarded_lua_bindings(&self) -> usize {
        self.discarded_lua_bindings
    }

    /// Los bindings efectivos, en orden de precedencia: secuencia ya
    /// formateada (`"g g"`, `"ctrl+k"`) y comando. La AYUDA se construye
    /// de aquí — refleja preset y capas del usuario, jamás listas a mano.
    #[must_use]
    pub fn bindings(&self) -> Vec<(String, &str)> {
        self.bindings
            .iter()
            .map(|(seq, run)| {
                let teclas: Vec<String> = seq.iter().map(ToString::to_string).collect();
                (teclas.join(" "), run.as_str())
            })
            .collect()
    }

    pub(super) fn lookup(&self, candidate: &[Chord]) -> Lookup<'_> {
        for (seq, run) in &self.bindings {
            if seq[..] == candidate[..] {
                return Lookup::Exact(run);
            }
        }
        if self
            .bindings
            .iter()
            .any(|(seq, _)| seq.len() > candidate.len() && seq[..candidate.len()] == candidate[..])
        {
            Lookup::Prefix
        } else {
            Lookup::Miss
        }
    }
}

pub(super) enum Lookup<'a> {
    Exact(&'a str),
    Prefix,
    Miss,
}
