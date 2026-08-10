//! The EFFECTIVE keymap: layers and contexts merged, validated prefix-free at
//! load time (ADR 0006), immutable afterwards.

use std::collections::HashSet;

use super::catalogue::{self, Status};
use super::chord::{Chord, KeyCode, Mods, parse_chord};
use super::layer::{KeymapFile, RawBinding, Screen, Section, check_layer_keys, merged_bindings};
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
    /// Whether this binding was read out of `[global]` rather than the map's
    /// own screen-specific section (`merged_bindings`'s [`Section`]) — the
    /// provenance [`Effective::is_global`] answers from. `check_binding`
    /// itself never sets this: it validates a binding without caring where it
    /// came from, so the caller stamps it on afterwards.
    pub(super) global: bool,
}

/// ONE key that may follow a pending prefix — a row of the which-key panel
/// ([`Effective::continuations`]).
///
/// Borrows its command from the map it was read out of: building one copies
/// nothing but two words and a flag, so the panel's cost is the `Vec` and the
/// frontend's own strings, not a clone of the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Continuation<'a> {
    /// The chord to press next.
    pub next: Chord,
    /// Length of the WHOLE sequence this row was taken from. Greater than
    /// `prefix.len() + 1` means [`Self::next`] opens yet another sequence
    /// rather than running [`Self::command`] — the overlay marks those with a
    /// trailing `…` instead of claiming they run something.
    pub seq_len: usize,
    /// The command at the end of that sequence. When several sequences share
    /// [`Self::next`], it is the first in precedence order — see
    /// [`Effective::continuations`].
    pub command: &'a str,
    /// Whether this build can run [`Self::command`]. Unavailable rows are
    /// painted, dimmed and with their reason: a key that cannot run must say
    /// so, not vanish from the panel that lists it.
    pub avail: Availability,
}

/// Keymap EFECTIVO: capas y contextos ya fusionados y validados
/// (prefix-free). Inmutable tras construir; clonable barato (el hot-reload
/// construye uno nuevo y lo cambia entero, ADR 0007).
#[derive(Debug, Clone)]
pub struct Effective {
    bindings: Vec<Binding>,
    /// The screen this map was built FOR. Carried rather than passed around
    /// because two of the load rules are screen-dependent (`check_sacred` only
    /// applies to Browse) and an [`Effective`] plus a screen given separately
    /// is a pair a caller can get wrong: asking the browse map about
    /// [`Screen::Dialog`] would hand out `Tab`, which is exactly the load
    /// error a rebind gate exists to stop
    /// ([`rebind_check`](super::rebind_check)).
    screen: Screen,
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
        // Stamped by the caller (`merged_bindings` knows the section; this
        // function does not) — `false` here is overwritten, never read.
        global: false,
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

/// Specification §12: `Tab` switches panes, and no preset or layer takes it.
/// BROWSE only — every bundled preset binds `tab` to `dialog.pane` inside
/// `[dialog]`, which is not pane switching, and banning the key outright would
/// break the dialogs norte already ships.
///
/// The table holds the [`KeyCode`], not the text that spells it: a `&str`
/// would have to go through [`parse_chord`], and a hardcoded constant that can
/// return [`KeymapError::BadChord`] is a fallible path with no failure — it
/// would either be `unwrap`ed (rule 6) or blame the user's file for a typo in
/// ours. The spelling in the diagnostic comes back out of `Display`, so the
/// round trip is closed by construction.
const SACRED_BROWSE: &[(KeyCode, &str)] = &[(KeyCode::Tab, "pane.switch")];

/// The reserved keys of `screen` and what each is reserved FOR — empty
/// outside Browse, for the reason [`SACRED_BROWSE`] gives. The single source
/// shared by the load-time rule ([`check_sacred`]) and the editor's pre-write
/// refusal ([`rebind_check`](super::rebind_check)), so the two can never
/// disagree about which keys are not for sale.
pub(super) fn sacred_chords(screen: Screen) -> &'static [(KeyCode, &'static str)] {
    match screen {
        Screen::Browse => SACRED_BROWSE,
        Screen::Viewer | Screen::Dialog => &[],
    }
}

/// A reserved key bound to anything else is a LOAD error, in the spirit of
/// prefix-free (ADR 0006): the conflict surfaces when the file loads, not when
/// a finger slips. Runs over EVERY binding, available or not, for the same
/// reason [`check_prefix_free`] does — the shape of the map is a load-time
/// property. Returns the first offender; shared by both builders.
///
/// The rule is about the FIRST chord, not the whole sequence (ADR 0044):
/// binding `["tab", "j"]` and no bare `tab` would leave Tab sitting pending,
/// which loses pane switching just as completely as rebinding it. The single
/// legal shape is the reserved chord bound, alone, to its reserved command.
fn check_sacred(bindings: &[Binding], screen: Screen) -> Result<(), KeymapError> {
    for (code, reserved_for) in sacred_chords(screen) {
        let sacred = Chord::new(Mods::default(), *code);
        for b in bindings {
            let starts_sacred = b.seq.first() == Some(&sacred);
            let is_the_reserved_binding = b.seq.as_slice() == [sacred] && b.run == *reserved_for;
            if starts_sacred && !is_the_reserved_binding {
                return Err(KeymapError::SacredKey {
                    chord: sacred.to_string(),
                    reserved_for,
                    run: b.run.clone(),
                });
            }
        }
    }
    Ok(())
}

/// With counts on, a bare digit 1-9 cannot ALSO open a binding. `0` is exempt:
/// a count never starts with zero, so the two never compete for it.
///
/// Only the FIRST chord of a sequence is inspected, which is exactly what the
/// accumulator does — it never opens a count with a sequence in flight, so a
/// digit anywhere but the front is an ordinary key. The two share
/// [`super::resolve::digit_of`] rather than each deciding what a digit is:
/// two answers to that question is how the rules would drift apart.
fn check_digits_free(bindings: &[Binding], counts: bool) -> Result<(), KeymapError> {
    if !counts {
        return Ok(());
    }
    for b in bindings {
        let Some(first) = b.seq.first() else { continue };
        if super::resolve::digit_of(*first).is_some_and(|d| d != 0) {
            return Err(KeymapError::DigitBoundWithCounts {
                chord: first.to_string(),
                run: b.run.clone(),
            });
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
        // binding ya no depende de él. La SECCIÓN sí importa: es la
        // procedencia que el editor de atajos necesita para marcar una fila
        // `[global]` como no editable (#141) — específico gana, así que la
        // primera vez que `seen` acepta una secuencia es también la única vez
        // que su sección cuenta.
        for (raw, _origin, section) in ordered {
            let binding = check_binding(raw, known_commands)?;
            // El primero gana (el orden YA codifica la precedencia). Un
            // binding NO disponible participa igual: ensombrece al de menos
            // precedencia en vez de dejarlo aflorar — la tecla dice por qué
            // no hace nada en lugar de hacer otra cosa.
            if seen.insert(binding.seq.clone()) {
                bindings.push(Binding {
                    global: section == Section::Global,
                    ..binding
                });
            }
        }
        check_prefix_free(&bindings)?;
        check_digits_free(&bindings, preset.counts)?;
        check_sacred(&bindings, screen)?;
        Ok(Self {
            bindings,
            screen,
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
    /// binding's defect, then the whole-map rules — the first ambiguous
    /// prefix, the first digit bound while counts are on, the first reserved
    /// key taken. An empty result means the keymap is clean.
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
        // Diagnostics never expose per-binding provenance, so the section is
        // not stamped here the way `build_for_impl` stamps it.
        for (raw, _origin, _section) in ordered {
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
        // The three whole-map rules each report their FIRST offender, like
        // `build_for` does — but here none of them stops the walk, so a keymap
        // that breaks all three gets all three rows in one pass.
        for check in [
            check_prefix_free(&bindings),
            check_digits_free(&bindings, preset.counts),
            check_sacred(&bindings, screen),
        ] {
            if let Err(e) = check {
                diags.push(KeymapDiagnostic::Structural {
                    message: e.to_string(),
                });
            }
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

    /// The screen this map was built for ([`Effective::build_for`]).
    ///
    /// The shortcut editor needs it twice over: to ask
    /// [`rebind_check`](super::rebind_check) the screen-dependent questions,
    /// and to know which `keymap.toml` section a binding for this map goes in
    /// ([`Screen::section`]). Taking it from the map instead of carrying it
    /// alongside is what stops the two from ever being about different
    /// screens.
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
    ///
    /// let preset = parse_keymap("[viewer]\nkeymap = [{ on = [\"q\"], run = \"viewer.close\" }]\n")
    ///     .unwrap();
    /// let eff = Effective::build_for(&preset, &[], &["viewer.close"], Screen::Viewer).unwrap();
    /// assert_eq!(eff.screen(), Screen::Viewer);
    /// assert_eq!(eff.screen().section(), "viewer");
    /// ```
    #[must_use]
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The validated bindings themselves — sequences as [`Chord`]s, not as the
    /// rendered strings [`Self::bindings_all`] hands out. Only
    /// [`rebind_check`](super::rebind_check) needs this: it compares a captured
    /// sequence against the map, and re-parsing rendered text to do that would
    /// put a second grammar between the two.
    pub(super) fn raw_bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Whether `seq`, exactly, was read out of `[global]` rather than this
    /// map's own screen-specific section — the provenance a shortcut editor
    /// needs to mark a row non-editable (#141) instead of guessing from the
    /// SCREEN a row is displayed under, which the merge has already erased:
    /// every screen's map contains `[global]`'s bindings indistinguishably
    /// from its own, and [`Screen::section`] never answers `"global"`, so a
    /// write there from a row naming one screen would change all three.
    ///
    /// `false` for a sequence this map does not bind at all — nothing here
    /// claims a global binding for a key nobody has.
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    ///
    /// let preset = parse_keymap(
    ///     "[global]\nkeymap = [{ on = [\"ctrl+p\"], run = \"app.palette\" }]\n\
    ///      [pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n",
    /// )
    /// .unwrap();
    /// let eff = Effective::build_for(&preset, &[], &["app.palette", "pane.copy"], Screen::Browse)
    ///     .unwrap();
    /// assert!(eff.is_global(&[parse_chord("ctrl+p").unwrap()]));
    /// assert!(!eff.is_global(&[parse_chord("f5").unwrap()]));
    /// // A key nobody binds is not global either.
    /// assert!(!eff.is_global(&[parse_chord("ctrl+z").unwrap()]));
    /// ```
    #[must_use]
    pub fn is_global(&self, seq: &[Chord]) -> bool {
        self.bindings.iter().any(|b| b.seq == seq && b.global)
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

    /// The same rows as [`Self::bindings_all`], with the sequence in CHORDS
    /// instead of rendered — what an EDITOR needs and the rendered form cannot
    /// give back.
    ///
    /// The shortcut editor (K3c) has to hand a row's existing sequence to
    /// [`rebind_check`](super::rebind_check) and to
    /// `norte_config::persist_keymap_unbind`, and it cannot re-derive it from
    /// what it painted: the sheet's chord goes through
    /// [`paint_chord`](super::paint_chord), which masks, and masking is not
    /// reversible. Reading it here also keeps the editor off a round trip
    /// through the grammar for a sequence that came out of it three lines
    /// earlier.
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    ///
    /// let preset = parse_keymap(
    ///     "[pane]\nkeymap = [{ on = [\"g\", \"g\"], run = \"cursor.top\" }]\n",
    /// )
    /// .unwrap();
    /// let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse).unwrap();
    /// let g = parse_chord("g").unwrap();
    /// assert_eq!(eff.bindings_all_seq()[0].0, &[g, g]);
    /// ```
    #[must_use]
    pub fn bindings_all_seq(&self) -> Vec<(&[Chord], &str, Availability)> {
        self.bindings
            .iter()
            .map(|b| (b.seq.as_slice(), b.run.as_str(), b.avail))
            .collect()
    }

    /// Is `chord`, pressed ALONE, bound to `command` and runnable here?
    ///
    /// The question a frontend asks on EVERY key event — "did this press mean
    /// the command this overlay/menu item cares about?" — so it must not
    /// allocate. It replaces the round trip the GUI used to do
    /// ([`Self::bindings`] renders every sequence with `Display` and the caller
    /// re-parsed each one with [`parse_chord`]): ~3 allocations per binding,
    /// ~140 per keystroke with the presets of today.
    ///
    /// Two answers are deliberately `false`, and both are load-bearing:
    ///
    /// - a SEQUENCE (`g g`) never matches a single press. These callers keep no
    ///   pending state — the pane's [`Resolver`](super::Resolver) does, they do
    ///   not — so the honest answer is that the first chord of a sequence is
    ///   not the sequence;
    /// - a binding this build cannot run ([`Availability`] other than
    ///   [`Availability::Here`]) is not a match. It still RESOLVES, so the
    ///   resolver can say why the key does nothing; what it must not do is
    ///   light up a menu item or close an overlay.
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    ///
    /// let src = r#"
    /// [pane]
    /// keymap = [
    ///     { on = ["f5"], run = "pane.copy" },
    ///     { on = ["g", "g"], run = "cursor.top" },
    ///     { on = ["ctrl+d"], run = "pane.hotlist" },
    /// ]
    /// "#;
    /// let preset = parse_keymap(src).unwrap();
    /// // `pane.hotlist` is NOT in this frontend's command list: it survives,
    /// // marked, but it is not a shortcut match.
    /// let known = ["pane.copy", "cursor.top"];
    /// let eff = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
    ///
    /// assert!(eff.single_chord_runs(parse_chord("f5").unwrap(), "pane.copy"));
    /// assert!(!eff.single_chord_runs(parse_chord("f5").unwrap(), "pane.move"));
    /// assert!(!eff.single_chord_runs(parse_chord("g").unwrap(), "cursor.top"));
    /// assert!(!eff.single_chord_runs(parse_chord("ctrl+d").unwrap(), "pane.hotlist"));
    /// ```
    #[must_use]
    pub fn single_chord_runs(&self, chord: Chord, command: &str) -> bool {
        self.bindings.iter().any(|b| {
            b.avail == Availability::Here && b.run == command && b.seq.as_slice() == [chord]
        })
    }

    /// Every binding whose sequence CONTINUES `prefix`: the rows a which-key
    /// overlay paints while `prefix` is pending. The next chord of each match,
    /// its command and its availability — unavailable ones INCLUDED, because a
    /// which-key panel that hides them recreates the silence K1 removed: the
    /// key still resolves, it just says why it does nothing.
    ///
    /// One row per NEXT chord, in [`paint_chord`](super::paint_chord) order, so the panel does not
    /// reshuffle between two builds of the same map. When several sequences
    /// share a next chord (`g a b` and `g a c` under `g`), the FIRST in
    /// precedence order supplies `command`/`avail` — meaningful only when
    /// `seq_len == prefix.len() + 1`, which is exactly when the chord runs
    /// something rather than opening more keys. A caller that paints
    /// `command` without testing `seq_len` is claiming a prefix runs the
    /// command at the end of one arbitrary branch of it.
    ///
    /// The two are never in doubt at once: the map is validated prefix-free
    /// (ADR 0006), so `g a` and `g a b` cannot both exist, and therefore
    /// sequences sharing a next chord always AGREE on whether that chord runs
    /// something. And "first wins" is the resolver's own rule —
    /// `lookup` scans the same `bindings` in the same order — so the
    /// panel cannot disagree with the key about which binding a chord means.
    ///
    /// An empty `prefix` returns every single-chord binding, and every first
    /// chord of a longer one. Legal, and what a future "show me everything"
    /// key would use; K3a's frontends never call it that way — a bare count
    /// does not open the panel, because the continuation of a count is any key
    /// at all.
    ///
    /// **Not the per-keystroke path.** Call it once per keystroke that leaves
    /// the resolver PENDING — the one that opens the sequence and each one
    /// that deepens it, since the rows change — never on every key event the
    /// way [`Self::single_chord_runs`] is asked, and never from inside a
    /// per-frame `render`. It allocates one `Vec` and one sort key per row,
    /// and the row builder on top of it
    /// ([`WhichKeyRows::build`](crate::whichkey::WhichKeyRows::build)) is the
    /// expensive half: a Fluent format and several `String`s per row. At
    /// typing speed that is nothing; at 60 Hz it is the ~140-allocations-per-
    /// keystroke pattern K2a deleted from `means_command`, wearing a hat. The
    /// remedy is the one `norte-tui` uses: build the snapshot on the resolver
    /// transition, store it, and let the renderer read the stored value.
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    ///
    /// let src = r#"
    /// [pane]
    /// keymap = [
    ///     { on = ["g", "g"], run = "cursor.top" },
    ///     { on = ["g", "h"], run = "cursor.bottom" },
    ///     { on = ["f5"], run = "pane.copy" },
    /// ]
    /// "#;
    /// let preset = parse_keymap(src).unwrap();
    /// let known = ["cursor.top", "cursor.bottom", "pane.copy"];
    /// let eff = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
    ///
    /// let g = parse_chord("g").unwrap();
    /// let rows = eff.continuations(&[g]);
    /// assert_eq!(rows.len(), 2);
    /// assert_eq!(rows[0].command, "cursor.top");
    /// assert_eq!(rows[1].command, "cursor.bottom");
    /// // `f5` continues nothing: it is a whole binding, not a prefix.
    /// assert!(eff.continuations(&[parse_chord("f5").unwrap()]).is_empty());
    /// ```
    #[must_use]
    pub fn continuations(&self, prefix: &[Chord]) -> Vec<Continuation<'_>> {
        // Decorate-sort-undecorate: the sort key is the chord as it is
        // PAINTED — the string the reader actually sees, so the panel is in
        // the order it looks like it is in (`F5` before `a`, not after it,
        // which raw `Display` would give) — and computing it inside a
        // comparator would allocate a String per comparison instead of one
        // per row.
        let mut rows: Vec<(String, Continuation<'_>)> = Vec::new();
        for b in &self.bindings {
            if b.seq.len() <= prefix.len() || b.seq[..prefix.len()] != prefix[..] {
                continue;
            }
            let next = b.seq[prefix.len()];
            // Dedup by NEXT chord: `g g` and `g h` are two rows, one chord
            // reachable through two longer sequences is one. First wins,
            // which is precedence order.
            if rows.iter().any(|(_, c)| c.next == next) {
                continue;
            }
            rows.push((
                super::chord::paint_chord(&next.to_string()),
                Continuation {
                    next,
                    seq_len: b.seq.len(),
                    command: b.run.as_str(),
                    avail: b.avail,
                },
            ));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows.into_iter().map(|(_, c)| c).collect()
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
/// RAW (lower case, unmasked) — a surface that shows it to a reader runs it
/// through [`paint_chord`](super::paint_chord) first.
pub(crate) fn render_seq(seq: &[Chord]) -> String {
    let keys: Vec<String> = seq.iter().map(ToString::to_string).collect();
    keys.join(" ")
}

pub(super) enum Lookup<'a> {
    Exact(&'a str, Availability),
    Prefix,
    Miss,
}

#[cfg(test)]
mod continuation_tests {
    use super::super::{Screen, parse_chord, parse_keymap};
    use super::{Availability, Effective};

    /// Vim-shaped fixture: a `g` prefix with two branches, one three-chord
    /// branch under it, a plain binding that is nobody's prefix, and one
    /// binding this build cannot run (`pane.pack` is `Planned`, #132).
    fn vim_shaped() -> Effective {
        let src = r#"
[pane]
keymap = [
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["g", "h"], run = "cursor.bottom" },
    { on = ["g", "a", "b"], run = "mark.all" },
    { on = ["g", "a", "c"], run = "mark.invert" },
    { on = ["g", "p"], run = "pane.pack" },
    { on = ["f5"], run = "pane.copy" },
]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = [
            "cursor.top",
            "cursor.bottom",
            "mark.all",
            "mark.invert",
            "pane.copy",
        ];
        Effective::build_for(&preset, &[], &known, Screen::Browse).expect("fixture builds")
    }

    #[test]
    fn continuations_of_a_prefix_are_its_next_chords_deduplicated() {
        let eff = vim_shaped();
        let g = parse_chord("g").expect("chord");
        let rows = eff.continuations(&[g]);
        let painted: Vec<String> = rows.iter().map(|c| c.next.to_string()).collect();
        // `a` ONCE, although two sequences (`g a b`, `g a c`) reach it.
        assert_eq!(painted, vec!["a", "g", "h", "p"], "{painted:?}");
        let a = rows
            .iter()
            .find(|c| c.next == parse_chord("a").expect("chord"));
        let a = a.expect("the `a` row");
        assert_eq!(a.seq_len, 3, "`g a` opens a longer sequence");
        assert_eq!(a.command, "mark.all", "first in precedence order");
        let gg = rows.iter().find(|c| c.next == g).expect("the `g` row");
        assert_eq!(gg.seq_len, 2, "`g g` runs something");
        assert_eq!(gg.command, "cursor.top");
    }

    #[test]
    fn a_complete_binding_that_is_nobody_prefix_continues_nothing() {
        let eff = vim_shaped();
        let f5 = parse_chord("f5").expect("chord");
        assert!(eff.continuations(&[f5]).is_empty());
    }

    /// A key bound to something this build has not got is a ROW, not a hole:
    /// hiding it is the silence K1 removed, and with K2b's presets naming
    /// ~30 `Planned` commands it is the common case, not an edge one.
    #[test]
    fn an_unavailable_continuation_is_listed_with_its_reason() {
        let eff = vim_shaped();
        let rows = eff.continuations(&[parse_chord("g").expect("chord")]);
        let p = rows
            .iter()
            .find(|c| c.command == "pane.pack")
            .expect("the unavailable row");
        assert!(
            matches!(
                p.avail,
                Availability::NotBuilt {
                    issue: 132,
                    reason: "keymap-reason-archive-write"
                }
            ),
            "{:?}",
            p.avail
        );
    }

    /// Same map, same order, every time: the panel must not reshuffle between
    /// two builds, or a reader's muscle memory reads the wrong row.
    #[test]
    fn the_order_is_deterministic_across_builds() {
        let first = vim_shaped();
        let second = vim_shaped();
        let g = parse_chord("g").expect("chord");
        let a: Vec<String> = first
            .continuations(&[g])
            .iter()
            .map(|c| c.next.to_string())
            .collect();
        let b: Vec<String> = second
            .continuations(&[g])
            .iter()
            .map(|c| c.next.to_string())
            .collect();
        assert_eq!(a, b);
    }

    /// The empty prefix is the whole first level — legal, and what a future
    /// "show me everything" key would ask for. Every binding contributes its
    /// FIRST chord, so `g` appears once for its four branches.
    #[test]
    fn an_empty_prefix_returns_the_first_level() {
        let eff = vim_shaped();
        let rows = eff.continuations(&[]);
        let painted: Vec<String> = rows.iter().map(|c| c.next.to_string()).collect();
        assert_eq!(painted, vec!["f5", "g"], "{painted:?}");
        let g = rows.iter().find(|c| c.next.to_string() == "g").expect("g");
        assert_eq!(g.seq_len, 2, "`g` opens a sequence, it runs nothing");
        let f5 = rows
            .iter()
            .find(|c| c.next.to_string() == "f5")
            .expect("f5");
        assert_eq!(f5.seq_len, 1, "`f5` is the whole binding");
    }
}
