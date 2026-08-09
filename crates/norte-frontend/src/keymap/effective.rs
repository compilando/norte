//! The EFFECTIVE keymap: layers and contexts merged, validated prefix-free at
//! load time (ADR 0006), immutable afterwards.

use std::collections::HashSet;

use super::catalogue::{self, Status};
use super::chord::{Chord, parse_chord};
use super::layer::{KeymapFile, RawBinding, Screen, check_layer_keys, merged_bindings};
use super::{KeymapDiagnostic, KeymapError};

/// Why a bound key may not run anything here. Kept ON the binding instead of
/// deleting it, so a key can explain itself instead of doing nothing.
///
/// ```
/// use norte_frontend::keymap::{Availability, Effective, Screen, parse_keymap};
///
/// let preset = parse_keymap(
///     "[pane]\nkeymap = [{ on = [\"alt+f1\"], run = \"pane.select-drive\" }]\n",
/// )
/// .unwrap();
/// let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
/// let all = eff.bindings_all();
/// assert!(matches!(all[0].2, Availability::NotBuilt { issue: 131, .. }));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Bound and runnable.
    Here,
    /// In the catalogue as [`Status::Planned`] — norte has not built it.
    NotBuilt {
        /// User-facing reason, from the catalogue.
        reason: &'static str,
        /// The issue that tracks it.
        issue: u32,
    },
    /// [`Status::Live`], but this frontend does not implement it (a TUI-only
    /// command bound while running the GUI, or the reverse).
    NotHere,
}

/// ONE validated binding of the effective keymap: the parsed sequence, the
/// command it names, and whether this build can actually run it. A real
/// struct and not a tuple because [`Availability`] made it the third field
/// threaded through six signatures.
#[derive(Debug, Clone)]
pub(super) struct Binding {
    pub(super) seq: Vec<Chord>,
    pub(super) run: String,
    pub(super) avail: Availability,
}

/// Keymap EFECTIVO: capas y contextos ya fusionados y validados
/// (prefix-free). Inmutable tras construir; clonable barato (el hot-reload
/// construye uno nuevo y lo cambia entero, ADR 0007).
#[derive(Debug, Clone)]
pub struct Effective {
    bindings: Vec<Binding>,
    /// Whether the PRESET this map was built from enables numeric counts
    /// (K2a). Copied here so the resolver — which owns only the effective map
    /// — can answer "is a bare digit a count?" without keeping the source
    /// files alive.
    counts: bool,
    /// Bindings `lua:` DESCARTADOS por venir de la capa de proyecto
    /// (seguridad, ver [`KeymapFile::mark_project`]). El frontend lo avisa una
    /// vez (jamás descarte mudo); el mensaje concreto es cosa del frontend.
    discarded_lua_bindings: usize,
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

/// Validates ONE merged binding and says whether this build can run it.
/// `Ok(binding)` = keep it (with its [`Availability`]); `Err` = a defect.
/// Shared by [`Effective::build_for`] (fails on the first `Err`) and
/// [`Effective::build_diagnostics`] (collects every `Err` and keeps walking)
/// — the SINGLE source of the per-binding rules, so the two paths can never
/// drift.
///
/// It takes neither the binding's origin nor a strictness mode: since K1 the
/// verdict does not depend on WHERE the binding came from. A name absent from
/// `known_commands` is looked up in the shared catalogue, which is what
/// separates "norte has not built it" ([`Availability::NotBuilt`]) and "this
/// frontend does not implement it" ([`Availability::NotHere`]) from "you
/// misspelled it" ([`KeymapError::UnknownCommand`], still fatal, in a preset
/// and in a user layer alike).
fn check_binding(raw: &RawBinding, known_commands: &[&str]) -> Result<Binding, KeymapError> {
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
    // (runtime), así que jamás está en el catálogo — solo se valida el
    // charset del nombre (la MISMA `valid_lua_name`, una sola fuente). Un
    // comando lua no registrado al invocar NO es error de keymap: el frontend
    // con host avisa en runtime.
    let avail = if let Some(lua_name) = raw.run.strip_prefix("lua:") {
        if !valid_lua_name(lua_name) {
            return Err(KeymapError::UnknownCommand {
                run: raw.run.clone(),
            });
        }
        Availability::Here
    } else if known_commands.contains(&raw.run.as_str()) {
        Availability::Here
    } else {
        // Ausente del set de ESTE frontend. El catálogo decide si eso es
        // «norte no lo ha construido» o «lo escribiste mal».
        match catalogue::lookup(&raw.run).map(|d| d.status) {
            Some(Status::Planned { reason, issue }) => Availability::NotBuilt { reason, issue },
            Some(Status::Live) => Availability::NotHere,
            None => {
                return Err(KeymapError::UnknownCommand {
                    run: raw.run.clone(),
                });
            }
        }
    };
    Ok(Binding {
        seq,
        run: raw.run.clone(),
        avail,
    })
}

/// Prefix-free: no sequence is a strict prefix of another (ADR 0006 — without
/// timeouts, resolution must be deterministic). Runs over EVERY binding,
/// available or not: the shape of the map is a load-time property, so a
/// binding this build cannot run still blocks a prefix (it will announce
/// itself when pressed, which a pending prefix would swallow). Returns the
/// first ambiguous pair; shared by both builders.
fn check_prefix_free(bindings: &[Binding]) -> Result<(), KeymapError> {
    for (i, first) in bindings.iter().enumerate() {
        let a = &first.seq;
        for second in bindings.iter().skip(i + 1) {
            let b = &second.seq;
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
        Self::build_for_impl(preset, layers, known_commands, screen)
    }

    fn build_for_impl(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Result<Self, KeymapError> {
        check_layer_keys(preset, layers)?;
        let mut discarded_lua_bindings = 0usize;
        let ordered = merged_bindings(preset, layers, screen, &mut discarded_lua_bindings);

        let mut seen: HashSet<Vec<Chord>> = HashSet::new();
        let mut bindings: Vec<Binding> = Vec::new();
        // `merged_bindings` sigue etiquetando el origen (`merge_ctx` lo usa
        // para descartar los `lua:` de la capa de proyecto); la decisión por
        // binding ya no depende de él.
        for (raw, _origin) in ordered {
            let binding = check_binding(raw, known_commands)?;
            // El primero gana (el orden YA codifica la precedencia). Un
            // binding NO disponible participa igual: ensombrece al de menos
            // precedencia en vez de dejarlo aflorar — la tecla dice por qué
            // no hace nada en lugar de hacer otra cosa.
            if seen.insert(binding.seq.clone()) {
                bindings.push(binding);
            }
        }
        check_prefix_free(&bindings)?;
        Ok(Self {
            bindings,
            counts: preset.counts,
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
    /// fix it). It shares the per-binding check with [`Effective::build_for`], so
    /// the two agree by construction: a name the catalogue knows is never a
    /// finding (it is [`Availability::NotBuilt`] or [`Availability::NotHere`],
    /// declared, not broken), and only a name the catalogue has never heard of
    /// is reported.
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
        let mut bindings: Vec<Binding> = Vec::new();
        for (raw, _origin) in ordered {
            match check_binding(raw, known_commands) {
                Ok(binding) => {
                    if seen.insert(binding.seq.clone()) {
                        bindings.push(binding);
                    }
                }
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

    /// Whether this effective keymap's preset enables numeric counts (`5j`).
    /// The resolver asks it before treating a bare digit as a count; the
    /// status bar asks it before offering to paint one.
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
    ///
    /// let plain = parse_keymap("[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n")
    ///     .unwrap();
    /// let eff = Effective::build_for(&plain, &[], &["cursor.down"], Screen::Browse).unwrap();
    /// assert!(!eff.counts(), "opt-in: sin la clave, un dígito es una tecla");
    ///
    /// let counting =
    ///     parse_keymap("counts = true\n[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n")
    ///         .unwrap();
    /// let eff = Effective::build_for(&counting, &[], &["cursor.down"], Screen::Browse).unwrap();
    /// assert!(eff.counts());
    /// ```
    #[must_use]
    pub fn counts(&self) -> bool {
        self.counts
    }

    /// Bindings `lua:` descartados por venir de la capa de PROYECTO (`./
    /// .norte`, sin trust — seguridad, ver [`KeymapFile::mark_project`]).
    /// El caller (main) lo pinta una vez por barra; los rebinds de proyecto
    /// a builtins NO cuentan aquí (siguen funcionando).
    #[must_use]
    pub fn discarded_lua_bindings(&self) -> usize {
        self.discarded_lua_bindings
    }

    /// The effective bindings that RUN, in precedence order: sequence already
    /// formatted (`"g g"`, `"ctrl+k"`) and command. The help and the hints are
    /// built from this — reflecting preset and user layers, never hand-written
    /// lists — so its meaning is unchanged by availability: a key that cannot
    /// run is not a key the help should advertise.
    #[must_use]
    pub fn bindings(&self) -> Vec<(String, &str)> {
        self.bindings
            .iter()
            .filter(|b| b.avail == Availability::Here)
            .map(|b| (render_seq(&b.seq), b.run.as_str()))
            .collect()
    }

    /// Every binding, available or not, with why. The reference sheet (K3)
    /// renders the unavailable ones in grey; nothing else should need this.
    ///
    /// ```
    /// use norte_frontend::keymap::{Availability, Effective, Screen, parse_keymap};
    ///
    /// let preset = parse_keymap(
    ///     "[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n",
    /// )
    /// .unwrap();
    /// let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
    /// assert_eq!(eff.bindings_all(), vec![("f5".to_owned(), "pane.copy", Availability::Here)]);
    /// ```
    #[must_use]
    pub fn bindings_all(&self) -> Vec<(String, &str, Availability)> {
        self.bindings
            .iter()
            .map(|b| (render_seq(&b.seq), b.run.as_str(), b.avail))
            .collect()
    }

    pub(super) fn lookup(&self, candidate: &[Chord]) -> Lookup<'_> {
        for b in &self.bindings {
            if b.seq[..] == candidate[..] {
                return Lookup::Exact(&b.run, b.avail);
            }
        }
        if self
            .bindings
            .iter()
            .any(|b| b.seq.len() > candidate.len() && b.seq[..candidate.len()] == candidate[..])
        {
            Lookup::Prefix
        } else {
            Lookup::Miss
        }
    }
}

/// A sequence as the help spells it: each chord's `Display`, space-joined.
fn render_seq(seq: &[Chord]) -> String {
    let keys: Vec<String> = seq.iter().map(ToString::to_string).collect();
    keys.join(" ")
}

pub(super) enum Lookup<'a> {
    Exact(&'a str, Availability),
    Prefix,
    Miss,
}
