//! Motor de keymap PURO compartido por los frontends (ADR 0006/0007/0043/0044):
//! mapa `(contexto, secuencia) → comando`, capas estilo Yazi, prefix-free
//! validado al cargar — la resolución es un scan lineal determinista sobre el
//! efectivo (≤ centenas de bindings), sin timeouts. Tecla NEUTRA (sin
//! crossterm/gpui): cada frontend convierte su evento nativo a [`Chord`] con
//! [`Chord::new`].
//!
//! ADR 0044 añade el prefijo numérico (`5j`) y las dos reglas de carga que lo
//! acompañan: el contador viaja CON el comando ([`Count`]) y es el frontend
//! quien repite el despacho, así que ninguna firma de comando cambia; un
//! dígito 1-9 no puede ser binding con contadores activos, y `Tab` está
//! reservada a `pane.switch` en Browse (la regla mira el PRIMER acorde de la
//! secuencia).

pub mod catalogue;
mod chord;
mod effective;
mod layer;
pub mod presets;
mod rebind;
mod resolve;

pub use catalogue::{CATALOGUE, CommandDef, Status};
pub use chord::{Chord, KeyCode, ModKey, Mods, mod_key, paint_chord, parse_chord, set_mod_key};
pub use effective::{Availability, Continuation, Effective, valid_lua_name};
// Not public API: the spelling a sequence has IN THE FILE, which the keyboard
// sheet paints (after `paint_chord`) and the shortcut editor hands to the
// `keymap.toml` writer. Two copies of it is how the writer and the loader
// would eventually disagree about what `g g` is called.
pub(crate) use effective::render_seq;
pub use layer::{KeymapFile, Screen, parse_keymap, parse_keymap_layer};
pub use rebind::{
    Rebind, RebindError, RebindSources, RebindSplit, RebindWrite, rebind_check, rebind_dry_run,
};
pub use resolve::{Count, Resolution, Resolver};

use layer::RawSection;

/// Error de carga o parseo de un keymap. Diagnóstico SIEMPRE accionable:
/// la config rota es un error claro, jamás comportamiento raro.
#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    /// El TOML no parsea o tiene claves desconocidas.
    #[error("keymap.toml inválido: {0}")]
    Toml(String),
    /// Una tecla no se entiende (`"megatecla"`, `"ctrl+"`, `"f99"`).
    #[error("tecla inválida: {chord:?}")]
    BadChord {
        /// El texto que no parseó.
        chord: String,
    },
    /// Un binding con secuencia vacía.
    #[error("binding con secuencia vacía (run = {run:?})")]
    EmptySequence {
        /// El comando del binding vacío.
        run: String,
    },
    /// El comando no existe (typo o versión vieja).
    #[error("comando desconocido: {run:?}")]
    UnknownCommand {
        /// El nombre que no se reconoce.
        run: String,
    },
    /// `shift+<char>` jamás matchearía (el char YA codifica shift): se
    /// rechaza con diagnóstico en vez de ser un binding muerto.
    #[error(
        "{chord:?}: shift no se combina con caracteres — escribe la tecla ya «shifteada» (\"G\", \"plus\")"
    )]
    ShiftWithChar {
        /// El texto ofensor.
        chord: String,
    },
    /// La capa trae la lista equivocada: un preset define `keymap`; la capa
    /// de usuario define `prepend_keymap`/`append_keymap` (modelo Yazi).
    /// Ignorarlo en silencio sería config rota sin error.
    #[error("la capa {layer} no admite {key} (preset: keymap; usuario: prepend/append)")]
    WrongLayerKey {
        /// `"preset"` o `"usuario"`.
        layer: &'static str,
        /// La clave que sobra.
        key: &'static str,
    },
    /// `dialog_from` y una sección `[dialog]` propia a la vez (K2b): dos
    /// respuestas a la misma pregunta, y elegir una en silencio dejaría al
    /// usuario con un contexto de overlays que no escribió.
    #[error("dialog_from = {name:?} junto a un [dialog] {list} propio: elige una de las dos")]
    DialogFromAndDialog {
        /// El preset que se pretendía heredar.
        name: String,
        /// Cuál de las tres listas de la sección lo desencadenó — sin esto el
        /// mensaje manda a buscar un `keymap` que quizá no existe.
        list: &'static str,
    },
    /// `dialog_from` nombra algo que no es un preset de fábrica (typo, o un
    /// preset de otra versión).
    #[error("dialog_from = {name:?}: no hay ningún preset de fábrica con ese nombre ({known})")]
    UnknownDialogFrom {
        /// El nombre que no resuelve.
        name: String,
        /// Los que sí, separados por comas (viene de `presets::NAMES`).
        known: String,
    },
    /// El preset heredado hereda a su vez: la herencia es de UN nivel, sin
    /// cadenas — si no, el `[dialog]` efectivo depende de un salto que no se
    /// ve leyendo el fichero.
    #[error(
        "dialog_from = {name:?}, pero ese preset hereda a su vez de {then:?}: la herencia de [dialog] es de un solo nivel"
    )]
    DialogFromChain {
        /// El preset nombrado por el fichero que se está cargando.
        name: String,
        /// A quién hereda ESE, que es lo que cierra la cadena.
        then: String,
    },
    /// `esc` dentro de una secuencia multi-tecla: inalcanzable, porque
    /// `Esc` SIEMPRE cancela un pendiente (solo vale como binding suelto).
    #[error("esc solo puede ligarse como tecla suelta, no dentro de {sequence:?}")]
    EscInSequence {
        /// La secuencia ofensora.
        sequence: String,
    },
    /// Una secuencia es prefijo estricto de otra: prohibido (ADR 0006 —
    /// sin timeouts, la resolución debe ser determinista).
    #[error("secuencias ambiguas: {shorter:?} es prefijo de {longer:?}")]
    AmbiguousPrefix {
        /// La secuencia corta (la que se dispararía siempre).
        shorter: String,
        /// La secuencia larga (la inalcanzable).
        longer: String,
    },
    /// Un dígito abre la secuencia de un binding en un contexto cuyo preset
    /// habilita los contadores numéricos (K2a). Las dos cosas no pueden ser
    /// ciertas a la vez, y elegir en silencio por el usuario es como un
    /// keymap se vuelve impredecible. El `0` está exento: un contador jamás
    /// empieza por cero, así que nunca se disputan la tecla.
    #[error(
        "{chord:?} no puede ser tecla y contador a la vez: está ligada a {run:?} y el preset activa contadores (el 0 sí es ligable)"
    )]
    DigitBoundWithCounts {
        /// El chord ofensor, tal y como se escribe.
        chord: String,
        /// A qué está ligado.
        run: String,
    },
    /// Un binding toma una tecla que la especificación reserva (§12: `Tab`
    /// cambia de panel). Un preset que imita a otro programa DOCUMENTA la
    /// diferencia; no se queda con la tecla.
    #[error("{chord:?} está reservada para {reserved_for} (spec §12) y no puede ligarse a {run:?}")]
    SacredKey {
        /// El chord reservado, tal y como se escribe.
        chord: String,
        /// El comando para el que está reservado.
        reserved_for: &'static str,
        /// Lo que el binding ofensor intentaba ejecutar en su lugar.
        run: String,
    },
}

/// One finding from [`Effective::build_diagnostics`]: reported WITHOUT
/// stopping the walk, unlike [`Effective::build_for`], which fails on the
/// first defect. `norte doctor` (#102) maps each to a report row in a
/// SINGLE pass — no per-typo rebuild, no retry cap, and no non-convergent
/// `lua:`-charset case (a broken `lua:` name can never be fixed by extending
/// `known_commands`, so the retry-with-known trick never terminates for it;
/// the one-pass walk classifies it directly instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapDiagnostic {
    /// A plain (non-`lua:`) `run` name absent from `known_commands` — a typo
    /// or a binding for a newer version's command. Recoverable: the rest of
    /// the keymap is unaffected. `run` is UNTRUSTED config text.
    UnknownCommand {
        /// The unrecognized command name (untrusted config text).
        run: String,
    },
    /// A defect that would make [`Effective::build_for`] fail outright: a bad
    /// chord, an empty or `esc`-bearing sequence, a wrong layer key, an
    /// ambiguous prefix, or a `lua:` name that fails the charset. Carries the
    /// rendered [`KeymapError`] message (may embed UNTRUSTED config text).
    Structural {
        /// Human-readable description (from the underlying [`KeymapError`]).
        message: String,
    },
}

/// The union of `run` names bound (in ANY of the three lists — `keymap`,
/// `prepend_keymap`, `append_keymap`, though only `keymap` is actually used
/// by a bundled preset today) by ANY bundled preset (`orthodox`/`vim`/`cua`)
/// for `screen` — its screen-specific context (`pane`/`viewer`/`dialog`)
/// merged with `global`. A preset that fails to parse is skipped silently
/// (the three bundled presets are compile-time embedded and pinned by this
/// module's own test suite, so this only matters if that invariant ever
/// breaks).
///
/// This is an HONEST APPROXIMATION, not a frontend's actual command
/// catalog: it only sees command names bound by a keymap, not the full set
/// a frontend implements (a command with no default binding in any preset
/// is invisible here). `norte doctor` (H2) uses it to flag a config layer's
/// `run` name that no bundled preset recognizes for that screen — worth a
/// warning, not proof the command doesn't exist (see its rustdoc/report
/// footer for the caveat).
#[must_use]
pub fn preset_commands(screen: Screen) -> Vec<String> {
    let specific = screen.specific();
    let mut out: Vec<String> = Vec::new();
    let push_all = |section: &RawSection, out: &mut Vec<String>| {
        for list in [
            &section.keymap,
            &section.prepend_keymap,
            &section.append_keymap,
        ] {
            for b in list {
                if !out.contains(&b.run) {
                    out.push(b.run.clone());
                }
            }
        }
    };
    for name in presets::NAMES {
        let Some(src) = presets::source(name) else {
            continue;
        };
        let Ok(kf) = parse_keymap(src) else {
            continue;
        };
        push_all(specific(&kf), &mut out);
        push_all(&kf.global, &mut out);
    }
    out
}

/// The user-facing sentence for an unavailable key. Lives here rather than in
/// each frontend so the TUI and the GUI cannot word it differently.
///
/// [`Availability::Here`] has nothing to say — the key runs — so it renders
/// empty; a caller only ever builds this from a
/// [`Resolution::Unavailable`], which never carries it.
///
/// ```
/// use norte_frontend::keymap::{Availability, unavailable_message};
///
/// let m = unavailable_message(
///     "pane.select-drive",
///     Availability::NotBuilt { reason: "keymap-reason-volume-enumeration", issue: 131 },
/// );
/// assert!(m.contains("pane.select-drive"), "{m}");
/// assert!(m.contains("131"), "{m}");
/// // The catalogue holds a Fluent ID: it must be TRANSLATED, not pasted.
/// assert!(!m.contains("keymap-reason-"), "{m}");
///
/// assert!(unavailable_message("pane.hotlist", Availability::NotHere).contains("pane.hotlist"));
/// assert!(unavailable_message("pane.copy", Availability::Here).is_empty());
/// ```
#[must_use]
pub fn unavailable_message(command: &str, why: Availability) -> String {
    match why {
        Availability::Here => String::new(),
        // `reason` is a Fluent ID, not prose (see the catalogue): translate it
        // first, then interpolate. Interpolating the id would print English
        // inside a Spanish sentence.
        Availability::NotBuilt { reason, issue } => norte_i18n::ta(
            "keymap-unavailable-not-built",
            &[
                ("command", command),
                ("reason", &norte_i18n::t(reason)),
                ("issue", &issue.to_string()),
            ],
        ),
        Availability::NotHere => {
            norte_i18n::ta("keymap-unavailable-not-here", &[("command", command)])
        }
    }
}

/// The SHORT form of the same fact, for a surface that already names the
/// command in the row it is decorating: the which-key panel (K3a) and the
/// reference sheet (K3b). No command in it, and the language is a parameter
/// rather than the process-wide one, because both callers render a whole page
/// in one language chosen by the caller.
///
/// It still says "not built yet" rather than only the reason, because the
/// third surface that prints it — `norte help keys` — writes to a pipe and
/// cannot dim anything: on its own, `writing archives (#132)` reads like a
/// description of what the key DOES. The wording has to carry the meaning that
/// styling carries elsewhere.
///
/// One function and not one per surface, next to [`unavailable_message`], so
/// the long form and the short form cannot drift into disagreeing about the
/// same `Availability`.
///
/// ```
/// use norte_frontend::keymap::{Availability, short_unavailable_message};
/// use norte_i18n::Lang;
///
/// let m = short_unavailable_message(
///     Availability::NotBuilt { reason: "keymap-reason-archive-write", issue: 132 },
///     Lang::En,
/// );
/// assert!(m.contains("132"), "{m}");
/// // It SAYS "not built": the bare reason would read as a description of
/// // what the key does on the one surface that cannot dim a row.
/// assert!(m.contains("not built"), "{m}");
/// // The catalogue holds a Fluent ID: it must be TRANSLATED, not pasted.
/// assert!(!m.contains("keymap-reason-"), "{m}");
/// // The command is the row's job, not this sentence's.
/// assert!(!m.contains("pane.pack"), "{m}");
/// assert!(short_unavailable_message(Availability::Here, Lang::En).is_empty());
/// ```
#[must_use]
pub fn short_unavailable_message(why: Availability, lang: norte_i18n::Lang) -> String {
    match why {
        Availability::Here => String::new(),
        // Same rule as the long form: `reason` is a Fluent id, so translate it
        // before interpolating, or a Spanish page carries an English clause.
        Availability::NotBuilt { reason, issue } => norte_i18n::ta_in(
            lang,
            "keymap-short-not-built",
            &[
                ("reason", &norte_i18n::t_in(lang, reason)),
                ("issue", &issue.to_string()),
            ],
        ),
        Availability::NotHere => norte_i18n::t_in(lang, "keymap-short-not-here"),
    }
}

/// The user-facing sentence for a count that landed on a command that takes
/// none. Lives here, like [`unavailable_message`], so the TUI and the GUI
/// cannot word it differently.
///
/// The count is never swallowed: the command runs ONCE and this says the
/// number went nowhere. Silence would leave the user believing `3q` did
/// something three times.
///
/// ```
/// use norte_frontend::keymap::count_ignored_message;
///
/// let m = count_ignored_message("app.quit", 3);
/// assert!(m.contains("app.quit"), "{m}");
/// assert!(m.contains('3'), "{m}");
/// ```
#[must_use]
pub fn count_ignored_message(command: &str, count: u32) -> String {
    norte_i18n::ta(
        "keymap-count-ignored",
        &[("command", command), ("count", &count.to_string())],
    )
}

#[cfg(test)]
mod preset_commands_tests {
    use super::{Screen, preset_commands};

    /// Orthodox binds `app.quit` in `[global]` (ADR 0006: global merges
    /// into every screen), so `Browse` must see it.
    #[test]
    fn orthodox_browse_contiene_app_quit() {
        let v = preset_commands(Screen::Browse);
        assert!(v.contains(&"app.quit".to_owned()), "{v:?}");
    }

    /// Every bundled preset binds `y` to `dialog.approve` in `[dialog]`.
    #[test]
    fn dialog_contiene_dialog_approve() {
        let v = preset_commands(Screen::Dialog);
        assert!(v.contains(&"dialog.approve".to_owned()), "{v:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(preset: &str, user: Option<&str>) -> Result<Effective, KeymapError> {
        const COMANDOS: &[&str] = &[
            "app.quit",
            "pane.switch",
            "cursor.up",
            "cursor.down",
            "cursor.top",
            "cursor.bottom",
            "nav.enter",
        ];
        let preset = parse_keymap(preset)?;
        let user = user.map(parse_keymap).transpose()?;
        Effective::build(&preset, user.as_ref(), COMANDOS)
    }

    /// A `Resolution::Run` with NO count — what every assertion written before
    /// K2a means, and what a preset without `counts = true` can ever produce.
    fn run(command: &str) -> Resolution {
        Resolution::Run {
            command: command.to_owned(),
            count: Count::None,
        }
    }

    #[test]
    fn parse_de_chords() {
        assert_eq!(
            parse_chord("f5").unwrap(),
            Chord::new(Mods::default(), KeyCode::F(5))
        );
        assert_eq!(
            parse_chord("ctrl+c").unwrap(),
            Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('c')
            )
        );
        assert_eq!(
            parse_chord("alt+enter").unwrap(),
            Chord::new(
                Mods {
                    alt: true,
                    ..Default::default()
                },
                KeyCode::Enter
            )
        );
        // Mayúscula: el char YA codifica shift.
        assert_eq!(
            parse_chord("G").unwrap(),
            Chord::new(Mods::default(), KeyCode::Char('G'))
        );
        assert_eq!(
            parse_chord("shift+f5").unwrap(),
            Chord::new(
                Mods {
                    shift: true,
                    ..Default::default()
                },
                KeyCode::F(5)
            )
        );
        for s in ["", "ctrl+", "megatecla", "ctrl+ctrl+c", "f99"] {
            assert!(parse_chord(s).is_err(), "{s:?} debe fallar");
        }
    }

    /// `+` is the modifier separator, so a bare "+" is unparseable and `plus`
    /// is the only spelling. Pinned because a future refactor that "simplifies"
    /// the token table would silently make the mark.pattern-add chord
    /// unreachable (#103).
    #[test]
    fn plus_token_is_the_only_spelling_of_the_plus_key() {
        assert_eq!(
            parse_chord("plus").unwrap(),
            Chord::new(Mods::default(), KeyCode::Char('+'))
        );
        assert!(matches!(
            parse_chord("+"),
            Err(KeymapError::BadChord { .. })
        ));
        assert_eq!(
            parse_chord("ctrl+plus").unwrap(),
            Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('+')
            )
        );
        // …y bajo los modificadores nuevos igual: `plus` sigue siendo la
        // única grafía, el separador no cambia de significado.
        assert_eq!(
            parse_chord("cmd+plus").unwrap(),
            Chord::new(
                Mods {
                    cmd: true,
                    ..Default::default()
                },
                KeyCode::Char('+')
            )
        );
        assert_eq!(
            parse_chord("mod+plus").unwrap(),
            parse_chord("ctrl+plus").unwrap(),
            "la política por defecto es Ctrl"
        );
        assert!(matches!(
            parse_chord("cmd++"),
            Err(KeymapError::BadChord { .. })
        ));
    }

    /// `mod+` is the one per-OS mechanism: a preset stays a single file. The
    /// process picks which physical modifier it means, once, at startup.
    #[test]
    fn mod_es_ctrl_por_defecto() {
        let c = parse_chord("mod+c").unwrap();
        assert_eq!(c, parse_chord("ctrl+c").unwrap());
    }

    /// `cmd+` is literal, for a preset that means Cmd and nothing else.
    #[test]
    fn cmd_es_su_propio_modificador_y_no_es_ctrl() {
        let cmd = parse_chord("cmd+c").unwrap();
        let ctrl = parse_chord("ctrl+c").unwrap();
        assert_ne!(cmd, ctrl);
    }

    /// The resolution is a pure function of the policy, so it is testable
    /// without a macOS machine — which matters, because CI is off and nobody
    /// here has one. NOTE: no test may call `set_mod_key`; the policy is
    /// process-wide and a test that sets it would poison every later test in
    /// the same binary. `apply` is the pure half, and it is the half worth
    /// pinning.
    #[test]
    fn la_politica_decide_a_que_se_traduce_mod() {
        assert_eq!(
            ModKey::Ctrl.apply(Mods::default()),
            Mods {
                ctrl: true,
                ..Mods::default()
            }
        );
        assert_eq!(
            ModKey::Cmd.apply(Mods::default()),
            Mods {
                cmd: true,
                ..Mods::default()
            }
        );
    }

    /// The repeated-modifier check sees THROUGH the alias: with the default
    /// policy `mod` IS `ctrl`, so `ctrl+mod+x` names the same key twice and
    /// dies as `BadChord`, exactly like `ctrl+ctrl+x`. `cmd+mod+x` is the
    /// same story on the other side, and stays legal here only because the
    /// default policy resolves `mod` to Ctrl.
    #[test]
    fn el_alias_cuenta_como_su_modificador_para_el_repetido() {
        assert!(matches!(
            parse_chord("ctrl+mod+x"),
            Err(KeymapError::BadChord { .. })
        ));
        assert!(matches!(
            parse_chord("mod+ctrl+x"),
            Err(KeymapError::BadChord { .. })
        ));
        assert!(matches!(
            parse_chord("cmd+cmd+x"),
            Err(KeymapError::BadChord { .. })
        ));
        // rust-reviewer MINOR-10: `cmd`+`mod` se rechaza SIEMPRE, no solo
        // bajo la política Cmd (donde sería la misma tecla dos veces). Este
        // test pinaba antes lo contrario —«legal bajo la política por
        // defecto»— y eso hacía que la validez de un chord dependiera del
        // sistema operativo: cargaba en Linux y reventaba en macOS, que es la
        // asimetría que la decisión 8 del ADR 0043 dice evitar.
        assert!(matches!(
            parse_chord("cmd+mod+x"),
            Err(KeymapError::BadChord { .. })
        ));
        // Y el orden no importa: es la combinación lo que se rechaza.
        assert!(matches!(
            parse_chord("mod+cmd+x"),
            Err(KeymapError::BadChord { .. })
        ));
    }

    /// `cmd` sorts BEFORE `ctrl` in `Display`, so a chord carrying both has
    /// exactly ONE spelling and the round trip is closed.
    #[test]
    fn cmd_precede_a_ctrl_en_la_grafia_canonica() {
        let c = Chord::new(
            Mods {
                cmd: true,
                ctrl: true,
                ..Mods::default()
            },
            KeyCode::Char('x'),
        );
        assert_eq!(c.to_string(), "cmd+ctrl+x");
        assert_eq!(parse_chord(&c.to_string()).unwrap(), c);
    }

    #[test]
    fn plus_chord_round_trips_through_display() {
        let c = Chord::new(Mods::default(), KeyCode::Char('+'));
        assert_eq!(c.to_string(), "plus");
        assert_eq!(parse_chord(&c.to_string()).unwrap(), c);
    }

    /// Table-wide `parse(display(c)) == c` property, scoped to the domain
    /// where `Display` and `parse_chord` actually agree: every non-`Char`
    /// key, `F(1..=12)`, and a char sample (space, `+`, an uppercase ASCII
    /// letter, and a non-ASCII char).
    ///
    /// `F(n)` OUTSIDE `1..=12` is deliberately EXCLUDED — `F(0)` as much as
    /// `F(13)`: `Display` renders ANY `F(n)` as
    /// `"f{n}"`, but `parse_chord` accepts only `1..=12`, so the two domains
    /// disagree there. Since #109 both frontend adapters clamp to `1..=12`
    /// (classic xterm reports Shift+F1 as F13 — the TUI adapter used to
    /// forward it unclamped), so no runtime path constructs one; the type
    /// still allows it, and this property scopes itself to the shared
    /// domain rather than pretending `Display` is total.
    #[test]
    fn display_and_parse_chord_round_trip_over_the_token_table() {
        let non_char = [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Esc,
            KeyCode::Backspace,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Insert,
            KeyCode::Delete,
        ];
        // encoding-auditor MINOR 1+2: this is the CROSS PRODUCT, not two
        // sweeps that never meet. The previous shape ran all 16 modifier
        // combinations against `F(5)` only, and every key against
        // `Mods::default()` only — so no modifier was ever applied to
        // `space`. That gap was load-bearing: `Char(' ')` and `Char('+')`
        // spell as WORDS precisely because the raw characters collide with
        // `render_seq`'s space join and `paint_chord`'s `split`, and a
        // refactor that "simplified" `Display` back to the raw char would
        // have kept the old test green while quietly turning a two-key
        // sequence into a three-key one in the help.
        //
        // `shift` is fed in unconditionally rather than masked off for
        // `Char`: `Chord::new` DROPS it there (the character already encodes
        // it) and `parse_chord` rejects the spelling, so running the bit
        // exercises that normalisation inside the round trip instead of
        // assuming it.
        let codes = non_char
            .into_iter()
            .chain((1..=12u8).map(KeyCode::F))
            .chain([' ', '+', 'G', 'ñ'].map(KeyCode::Char));
        for code in codes {
            for bits in 0..16u8 {
                let mods = Mods {
                    cmd: bits & 1 != 0,
                    ctrl: bits & 2 != 0,
                    alt: bits & 4 != 0,
                    shift: bits & 8 != 0,
                };
                // `Display` orders them `cmd+ctrl+alt+shift+`, the one
                // canonical spelling `parse_chord` reads back whatever order
                // it was written in.
                let c = Chord::new(mods, code);
                assert_eq!(parse_chord(&c.to_string()).unwrap(), c, "{mods:?} {code:?}");
            }
        }
    }

    #[test]
    fn chord_new_normaliza_shift_en_chars_pero_no_en_otras_teclas() {
        // Un evento nativo con Char('G')+shift: el chord canónico descarta
        // shift (el char ya lo codifica) — paridad con el viejo
        // `Chord::from_event` de la TUI (ahora el comportamiento por
        // defecto de `Chord::new`).
        let c = Chord::new(
            Mods {
                shift: true,
                ..Default::default()
            },
            KeyCode::Char('G'),
        );
        assert_eq!(c, Chord::new(Mods::default(), KeyCode::Char('G')));
        // En teclas no-char, shift ES información.
        let f = Chord::new(
            Mods {
                shift: true,
                ..Default::default()
            },
            KeyCode::F(5),
        );
        assert_eq!(
            f,
            Chord::new(
                Mods {
                    shift: true,
                    ..Default::default()
                },
                KeyCode::F(5)
            )
        );
    }

    #[test]
    fn parse_chord_rechaza_tokens_multi_codepoint_sin_partir() {
        // Un token que NO es exactamente un char (é descompuesto = e+U+0301,
        // o un emoji ZWJ) se rechaza limpio, jamás se trunca a medias.
        assert!(matches!(
            parse_chord("e\u{0301}"),
            Err(KeymapError::BadChord { .. })
        ));
        assert!(matches!(
            parse_chord("👨\u{200d}👩\u{200d}👧"),
            Err(KeymapError::BadChord { .. })
        ));
    }

    #[test]
    fn resuelve_secuencias_multi_tecla() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g", "g"], run = "cursor.top" },
                { on = ["G"], run = "cursor.bottom" },
            ]
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("g").unwrap()),
            Resolution::Pending(1),
            "prefijo válido: espera"
        );
        assert_eq!(r.push(parse_chord("g").unwrap()), run("cursor.top"));
        // Tras ejecutar, el estado queda limpio.
        assert_eq!(r.push(parse_chord("G").unwrap()), run("cursor.bottom"));
        // Tecla sin binding: reset silencioso.
        assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
        // Prefijo pendiente + tecla que no continúa: reset (no ejecuta nada).
        r.push(parse_chord("g").unwrap());
        assert_eq!(r.push(parse_chord("q").unwrap()), Resolution::Reset);
        // q suelto (contexto global) sí corre.
        assert_eq!(r.push(parse_chord("q").unwrap()), run("app.quit"));
    }

    #[test]
    fn esc_cancela_la_secuencia_pendiente() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g", "g"], run = "cursor.top" },
                { on = ["esc"], run = "app.quit" },
            ]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        r.push(parse_chord("g").unwrap());
        // Con secuencia pendiente, Esc SIEMPRE cancela (jamás ejecuta binding).
        assert_eq!(r.push(parse_chord("esc").unwrap()), Resolution::Reset);
        // Sin pendiente, Esc es una tecla más.
        assert_eq!(r.push(parse_chord("esc").unwrap()), run("app.quit"));
    }

    #[test]
    fn prefijo_ambiguo_es_error_de_carga() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g"], run = "cursor.top" },
                { on = ["g", "g"], run = "cursor.bottom" },
            ]
        "#;
        match eff(preset, None) {
            Err(KeymapError::AmbiguousPrefix { .. }) => {}
            other => panic!("esperaba AmbiguousPrefix, fue {other:?}"),
        }
    }

    #[test]
    fn shift_con_char_es_error_diagnosticable() {
        // Un binding "shift+g" jamás matchearía (el chord canónico descarta
        // shift en chars): rechazo al parsear, no binding muerto.
        match parse_chord("shift+g") {
            Err(KeymapError::ShiftWithChar { .. }) => {}
            other => panic!("esperaba ShiftWithChar, fue {other:?}"),
        }
        assert!(parse_chord("ctrl+shift+c").is_err());
        // En teclas no-char, shift es legítimo.
        assert!(parse_chord("shift+f5").is_ok());
    }

    #[test]
    fn lista_equivocada_en_una_capa_es_error() {
        // Usuario con `keymap` (en vez de prepend/append): error, no silencio.
        let preset = r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        let user = r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.up" }]
        "#;
        match eff(preset, Some(user)) {
            Err(KeymapError::WrongLayerKey {
                layer: "usuario", ..
            }) => {}
            other => panic!("esperaba WrongLayerKey usuario, fue {other:?}"),
        }
        // Preset con prepend: mismo trato.
        let preset_malo = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        match eff(preset_malo, None) {
            Err(KeymapError::WrongLayerKey {
                layer: "preset", ..
            }) => {}
            other => panic!("esperaba WrongLayerKey preset, fue {other:?}"),
        }
    }

    #[test]
    fn la_especificidad_de_contexto_prevalece_sobre_la_capa() {
        // ADR 0006 (desambiguado en fase 4): las capas se fusionan POR
        // contexto; entre contextos gana el específico. Un append de usuario
        // en [pane] pisa al keymap [global] del preset…
        let preset = r#"
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        let user = r#"
            [pane]
            append_keymap = [{ on = ["q"], run = "cursor.up" }]
            [global]
            prepend_keymap = [{ on = ["j"], run = "app.quit" }]
        "#;
        let eff = eff(preset, Some(user)).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            run("cursor.up"),
            "pane.append gana a global.keymap (especificidad > capa)"
        );
        // …y un prepend de usuario en [global] NO pisa al keymap [pane].
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.down"),
            "global.prepend no pisa a pane.keymap"
        );
    }

    #[test]
    fn esc_dentro_de_secuencia_es_error_de_carga() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["a", "esc"], run = "cursor.up" }]
        "#;
        match eff(preset, None) {
            Err(KeymapError::EscInSequence { .. }) => {}
            other => panic!("esperaba EscInSequence, fue {other:?}"),
        }
    }

    #[test]
    fn comando_desconocido_es_error_de_carga() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["x"], run = "comando.inventado" }]
        "#;
        match eff(preset, None) {
            Err(KeymapError::UnknownCommand { .. }) => {}
            other => panic!("esperaba UnknownCommand, fue {other:?}"),
        }
    }

    /// M4 Lua (T8, espejado): un binding a `lua:<nombre>` pasa la validación
    /// aunque el nombre no esté en `known_commands` — el registro Lua es
    /// dinámico (runtime); un comando lua no registrado NO es error de
    /// keymap. El NOMBRE sí se valida con el mismo charset que
    /// `norte.command` (`[a-z0-9._-]{1,64}`): un binding a un nombre que
    /// jamás podría registrarse es config rota diagnosticable, no un binding
    /// muerto en silencio.
    #[test]
    fn lua_prefijado_pasa_la_validacion_de_comandos() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["x"], run = "lua:mi-comando.v2" }]
        "#;
        let mut r = Resolver::new(eff(preset, None).expect("lua: con nombre válido pasa"));
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            run("lua:mi-comando.v2"),
            "el binding resuelve al comando lua: completo"
        );

        // Nombres fuera del charset [a-z0-9._-]{1,64}: error de CARGA.
        let largo = format!("lua:{}", "a".repeat(65));
        for bad in ["lua:", "lua:Mayuscula", "lua:con espacio", largo.as_str()] {
            let preset = format!(
                r#"
                [pane]
                keymap = [{{ on = ["x"], run = "{bad}" }}]
                "#
            );
            match eff(&preset, None) {
                Err(KeymapError::UnknownCommand { .. }) => {}
                other => panic!("esperaba UnknownCommand para {bad:?}, fue {other:?}"),
            }
        }
    }

    #[test]
    fn capas_yazi_prepend_pisa_y_append_solo_anade() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["j"], run = "cursor.down" },
                { on = ["k"], run = "cursor.up" },
            ]
        "#;
        let user = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [
                { on = ["k"], run = "cursor.bottom" },
                { on = ["x"], run = "app.quit" },
            ]
        "#;
        let eff = eff(preset, Some(user)).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.top"),
            "prepend PISA al preset"
        );
        assert_eq!(
            r.push(parse_chord("k").unwrap()),
            run("cursor.up"),
            "append NO pisa una secuencia existente"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            run("app.quit"),
            "append añade lo nuevo"
        );
    }

    #[test]
    fn el_contexto_especifico_pisa_al_global_por_secuencia_exacta() {
        let preset = r#"
            [global]
            keymap = [
                { on = ["q"], run = "app.quit" },
                { on = ["tab"], run = "pane.switch" },
            ]
            [pane]
            keymap = [{ on = ["q"], run = "cursor.up" }]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(r.push(parse_chord("q").unwrap()), run("cursor.up"));
        assert_eq!(r.push(parse_chord("tab").unwrap()), run("pane.switch"));
    }

    #[test]
    fn capas_multiples_se_pliegan_por_precedencia() {
        // Capas en precedencia ASCENDENTE: sistema, usuario.
        const COMANDOS: &[&str] = &[
            "app.quit",
            "cursor.up",
            "cursor.down",
            "cursor.top",
            "cursor.bottom",
        ];
        // ADR 0007: prepends de capas superiores primero; appends igual.
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#,
        )
        .unwrap();
        let sistema = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.up" }]
            append_keymap = [{ on = ["x"], run = "app.quit" }]
        "#,
        )
        .unwrap();
        let usuario = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [{ on = ["x"], run = "cursor.bottom" }]
        "#,
        )
        .unwrap();
        let eff = Effective::build_layered(&preset, &[sistema, usuario], COMANDOS).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.top"),
            "el prepend de la capa MÁS alta gana"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            run("cursor.bottom"),
            "entre appends también gana la capa más alta"
        );
    }

    #[test]
    fn el_contexto_viewer_se_fusiona_para_su_pantalla() {
        const COMANDOS: &[&str] = &["app.quit", "nav.enter", "cursor.top"];
        let preset = parse_keymap(
            r#"
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
            [pane]
            keymap = [{ on = ["enter"], run = "nav.enter" }]
            [viewer]
            keymap = [{ on = ["q"], run = "cursor.top" }]
        "#,
        )
        .unwrap();
        // En Browse, el q global manda y enter existe.
        let browse = Effective::build_for(&preset, &[], COMANDOS, Screen::Browse).unwrap();
        let mut r = Resolver::new(browse);
        assert_eq!(r.push(parse_chord("q").unwrap()), run("app.quit"));
        assert_eq!(r.push(parse_chord("enter").unwrap()), run("nav.enter"));
        // En Viewer, su q específico PISA al global y enter NO existe.
        let viewer = Effective::build_for(&preset, &[], COMANDOS, Screen::Viewer).unwrap();
        let mut r = Resolver::new(viewer);
        assert_eq!(r.push(parse_chord("q").unwrap()), run("cursor.top"));
        assert_eq!(r.push(parse_chord("enter").unwrap()), Resolution::Reset);
    }

    /// La ayuda se construye del keymap EFECTIVO: los bindings expuestos
    /// reflejan preset + capas EN ORDEN de precedencia, y un binding
    /// sombreado aparece UNA vez con el comando que gana (lo que la tecla
    /// hace de verdad, no lo que el preset dice).
    #[test]
    fn bindings_expuestos_reflejan_las_capas() {
        const COMANDOS: &[&str] = &["cursor.down", "cursor.up", "cursor.top"];
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [
                { on = ["j"], run = "cursor.down" },
                { on = ["k"], run = "cursor.up" },
            ]
        "#,
        )
        .unwrap();
        let user = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [{ on = ["g", "g"], run = "cursor.top" }]
        "#,
        )
        .unwrap();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&user), COMANDOS).unwrap();
        let b = eff.bindings();
        // Sombreado: "j" UNA sola vez y gana el prepend del usuario.
        let jotas: Vec<_> = b.iter().filter(|(seq, _)| seq == "j").collect();
        assert_eq!(jotas.len(), 1, "binding sombreado duplicado: {b:?}");
        assert_eq!(jotas[0].1, "cursor.top", "debe ganar la capa del usuario");
        // Orden de precedencia: prepend del usuario antes que el preset.
        let pos = |wanted: &str| b.iter().position(|(seq, _)| seq == wanted).unwrap();
        assert!(pos("j") < pos("k"), "prepend antes que preset: {b:?}");
        assert!(
            b.iter()
                .any(|(seq, cmd)| seq == "g g" && *cmd == "cursor.top"),
            "el append del usuario aparece en la ayuda: {b:?}"
        );
    }

    /// ALTA (security review M4 Lua): `./.norte/keymap.toml` carga SIN trust,
    /// así que un repo hostil podría rebindear una tecla común (`j`, `enter`)
    /// a un comando `lua:` del init.lua de USUARIO (sin sandbox, sin
    /// confirmación, con cwd = el repo hostil). Los bindings `lua:`
    /// originados en la capa de PROYECTO se DESCARTAN (contados para el
    /// aviso de barra); los rebinds de proyecto a builtins siguen
    /// funcionando; el mismo binding en una capa de usuario SÍ resuelve.
    #[test]
    fn lua_de_keymap_de_proyecto_se_descarta_con_aviso() {
        const COMANDOS: &[&str] = &["cursor.down", "cursor.up"];
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#,
        )
        .unwrap();
        let capa = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "lua:pwn" }]
        "#;

        // Capa de PROYECTO: el binding lua: se descarta — la tecla cae al
        // builtin del preset — y queda contado para el aviso.
        let mut proyecto = parse_keymap(capa).unwrap();
        proyecto.mark_project();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&proyecto), COMANDOS)
            .expect("descartar no es error de carga");
        assert_eq!(eff.discarded_lua_bindings(), 1, "contado para el aviso");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("cursor.down"),
            "la tecla cae al builtin, jamás al lua: del proyecto"
        );

        // El MISMO binding en capa de USUARIO (sin marcar): resuelve normal.
        let usuario = parse_keymap(capa).unwrap();
        let eff =
            Effective::build_layered(&preset, std::slice::from_ref(&usuario), COMANDOS).unwrap();
        assert_eq!(eff.discarded_lua_bindings(), 0);
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            run("lua:pwn"),
            "en capa de usuario el binding lua: es legítimo"
        );

        // Rebind de proyecto a un BUILTIN: sigue funcionando (el descarte es
        // SOLO de `lua:` — config de proyecto inocua no se rompe).
        let mut proyecto = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["x"], run = "cursor.up" }]
        "#,
        )
        .unwrap();
        proyecto.mark_project();
        let eff =
            Effective::build_layered(&preset, std::slice::from_ref(&proyecto), COMANDOS).unwrap();
        assert_eq!(eff.discarded_lua_bindings(), 0);
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("x").unwrap()), run("cursor.up"));
    }

    /// Nuevo (GUI-c T1): el motor NO conoce comandos concretos — valida
    /// contra la lista `known_commands` que le pasa el CALLER (cada
    /// frontend tiene su propio catálogo). Con "foo.bar" en la lista: OK;
    /// sin él, `UnknownCommand`.
    #[test]
    fn build_valida_contra_el_known_commands_dado() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "foo.bar" }]"#,
        )
        .unwrap();
        // Con "foo.bar" conocido: OK.
        assert!(Effective::build(&preset, None, &["foo.bar"]).is_ok());
        // Sin él: UnknownCommand (el motor NO conoce comandos concretos).
        assert!(matches!(
            Effective::build(&preset, None, &["otro.cmd"]),
            Err(KeymapError::UnknownCommand { .. })
        ));
    }

    /// Regresión GUI-c T2 review: una tecla que el FRONTEND no modela
    /// (p. ej. crossterm `BackTab`/`Media`, adaptada a `None`) debe romper
    /// cualquier secuencia multi-tecla en curso — el viejo `from_event`
    /// SIEMPRE empujaba al resolver (aunque fuera con un chord exótico que
    /// jamás casaba), lo que producía un `Miss` y limpiaba el pending. Un
    /// adaptador que devuelve `Option` y un caller que simplemente
    /// descarta el `None` deja el pending INTERNO intacto — `reset()` es
    /// el equivalente explícito al `Miss` que el adaptador ya no puede
    /// producir por sí solo.
    #[test]
    fn reset_rompe_la_secuencia_pendiente() {
        let kf = parse_keymap(
            r#"[pane]
keymap = [{ on = ["g", "g"], run = "cursor.top" }]"#,
        )
        .unwrap();
        let eff = Effective::build_for(&kf, &[], &["cursor.top"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert!(matches!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(_)
        ));
        r.reset();
        // Tras reset, un solo 'g' vuelve a estar pendiente (la secuencia
        // se rompió: si NO se hubiera roto, este segundo 'g' dispararía
        // Run("cursor.top") en vez de Pending(1)).
        assert!(matches!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(_)
        ));
    }

    /// USED TO pin `build_for_subset`: a PRESET binding to a command this
    /// frontend does not implement was skipped in silence, and a LAYER binding
    /// to the same name was an error — provenance decided the verdict. Since
    /// K1 the CATALOGUE decides: `app.help` is `Live`, so the binding SURVIVES
    /// as `NotHere` from either source, and only a name the vocabulary has
    /// never heard of is still fatal. The two halves of the old assertion are
    /// still here, both inverted.
    #[test]
    fn un_comando_ajeno_sobrevive_venga_del_preset_o_de_una_capa() {
        let preset = parse_keymap(
            "[pane]\nkeymap = [\n { on = [\"q\"], run = \"app.quit\" },\n { on = [\"f1\"], run = \"app.help\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Browse)
            .expect("preset con extras construye");
        let all = eff.bindings_all();
        assert_eq!(
            all.iter()
                .find(|(seq, _, _)| seq == "f1")
                .map(|(_, run, avail)| (*run, *avail)),
            Some(("app.help", Availability::NotHere)),
            "el binding ya no se filtra: sobrevive marcado — {all:?}"
        );
        // …y sigue sin EJECUTARSE: `bindings()` solo lista lo ejecutable.
        assert!(
            !eff.bindings().iter().any(|(seq, _)| seq == "f1"),
            "{all:?}"
        );
        // Una CAPA que bindea el mismo comando ajeno tampoco falla ya.
        let layer =
            parse_keymap("[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"app.help\" }]\n")
                .unwrap();
        assert!(
            Effective::build_for(&preset, &[layer], &known, Screen::Browse).is_ok(),
            "un comando del catálogo no es un typo, venga de donde venga"
        );
        // Lo que sí sigue muriendo: un nombre que el catálogo no conoce.
        let typo =
            parse_keymap("[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"app.hlep\" }]\n")
                .unwrap();
        assert!(matches!(
            Effective::build_for(&preset, &[typo], &known, Screen::Browse),
            Err(KeymapError::UnknownCommand { .. })
        ));
    }

    /// El nombre `lua:` se valida por CHARSET antes que nada: un `lua:` con
    /// nombre inválido (fuera de `[a-z0-9._-]{1,64}`) no tiene forma de
    /// colarse por la puerta del catálogo — un `lua:` jamás está en él.
    #[test]
    fn lua_invalido_sigue_siendo_error() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "lua:Bad Name" }]"#,
        )
        .unwrap();
        match Effective::build_for(&preset, &[], &[], Screen::Browse) {
            Err(KeymapError::UnknownCommand { .. }) => {}
            other => panic!("esperaba UnknownCommand, fue {other:?}"),
        }
    }

    /// `build_diagnostics` (#102): reports EVERY unknown-command finding in
    /// ONE walk — no per-typo rebuild, no retry cap. Three distinct made-up
    /// `run` names in a layer must all come back as `UnknownCommand`
    /// diagnostics from a single call.
    #[test]
    fn build_diagnostics_reporta_todos_los_desconocidos_en_una_pasada() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"q\"], run = \"app.quit\" }]\n").unwrap();
        let layer = parse_keymap(
            "[pane]\nappend_keymap = [\n { on = [\"x\"], run = \"typo.one\" },\n { on = [\"y\"], run = \"typo.two\" },\n { on = [\"z\"], run = \"typo.three\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let diags = Effective::build_diagnostics(&preset, &[layer], &known, Screen::Browse);
        let unknowns: Vec<&str> = diags
            .iter()
            .filter_map(|d| match d {
                KeymapDiagnostic::UnknownCommand { run } => Some(run.as_str()),
                KeymapDiagnostic::Structural { .. } => None,
            })
            .collect();
        assert_eq!(
            unknowns,
            ["typo.one", "typo.two", "typo.three"],
            "{diags:?}"
        );
    }

    /// A `lua:<name>` binding whose name fails the charset is a `Structural`
    /// diagnostic (never fixable by adding it to `known`), NOT a recoverable
    /// `UnknownCommand` — this is exactly the non-convergent case #102's
    /// one-pass builder resolves by construction.
    #[test]
    fn build_diagnostics_lua_charset_invalido_es_structural() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"x\"], run = \"lua:bad name!\" }]\n").unwrap();
        let diags = Effective::build_diagnostics(&preset, &[], &[], Screen::Browse);
        assert_eq!(diags.len(), 1, "{diags:?}");
        match &diags[0] {
            KeymapDiagnostic::Structural { message } => {
                assert!(message.contains("lua:bad name!"), "{message}");
            }
            d @ KeymapDiagnostic::UnknownCommand { .. } => {
                panic!("esperaba Structural, fue {d:?}")
            }
        }
    }

    /// A well-formed keymap yields NO diagnostics (the caller reports
    /// `keymap-ok`).
    #[test]
    fn build_diagnostics_keymap_valido_sin_hallazgos() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"q\"], run = \"app.quit\" }]\n").unwrap();
        let diags = Effective::build_diagnostics(&preset, &[], &["app.quit"], Screen::Browse);
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// INVERTED (K1 T3). It used to be `subset_prefijo_filtrado_no_bloquea_
    /// secuencia`, and it pinned the opposite: a filtered PRESET binding
    /// vanished before `check_prefix_free`, so `"g"` (preset, unimplemented)
    /// left `"g g"` (layer) resolving cleanly. That is a load-time property
    /// being decided by what this build happens to run, and the direction is
    /// now the other one — the shape of the map is fixed at load (ADR 0006),
    /// so an unavailable `"g"` still blocks `"g g"`. Do not "fix" it back:
    /// a `"g"` that swallows the first key of `"g g"` in one frontend and not
    /// in the other is exactly the drift K1 exists to kill.
    #[test]
    fn un_binding_no_disponible_si_bloquea_el_prefijo() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["g"], run = "pane.select-drive" }]"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"[pane]
append_keymap = [{ on = ["g", "g"], run = "cursor.top" }]"#,
        )
        .unwrap();
        let known = ["cursor.top"];
        match Effective::build_for(&preset, &[layer], &known, Screen::Browse) {
            Err(KeymapError::AmbiguousPrefix { .. }) => {}
            other => panic!("esperaba AmbiguousPrefix, fue {other:?}"),
        }
    }

    /// INVERTED (K1 T3). It used to be `subset_dedup_desenmascara_binding_
    /// global`, and it pinned a divergence that was deliberate at the time:
    /// the dedup (`seen.insert`, first-wins) only saw the bindings that
    /// SURVIVED the filter, so a `pane` binding this frontend could not run
    /// was dropped BEFORE the dedup and the `global` one underneath became
    /// active — the same key doing two different things depending on which
    /// frontend read the preset. Since K1 nothing is dropped: the specific
    /// context still wins, and the key that survives is the unavailable one,
    /// which will say so instead of quietly doing something else. Do not
    /// "fix" it back: falling through is how a Total Commander user gets a
    /// surprise instead of an answer.
    #[test]
    fn un_binding_no_disponible_sigue_ensombreciendo() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "pane.select-drive" }]
[global]
keymap = [{ on = ["x"], run = "app.quit" }]"#,
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Browse)
            .expect("pane.x no disponible, global.x conocido");
        let all = eff.bindings_all();
        let hits: Vec<_> = all.iter().filter(|(seq, _, _)| seq == "x").collect();
        assert_eq!(hits.len(), 1, "el dedup deja UNA por secuencia: {all:?}");
        assert_eq!(
            hits[0].1, "pane.select-drive",
            "gana el contexto específico"
        );
        assert!(matches!(hits[0].2, Availability::NotBuilt { .. }));
        // El `app.quit` de `[global]` sigue SOMBREADO: no aflora.
        assert!(
            !eff.bindings().iter().any(|(seq, _)| seq == "x"),
            "la tecla no ejecuta nada — dirá por qué: {all:?}"
        );
    }

    /// Un chord ilegible (`"megatecla"`) en un binding cuyo comando TAMBIÉN
    /// es desconocido: el parseo de la secuencia corre ANTES de consultar el
    /// catálogo (`raw.on.iter().map(parse_chord)`), así que `BadChord` gana —
    /// la config estructuralmente rota jamás se declara «no disponible».
    #[test]
    fn chord_malo_gana_al_comando_desconocido() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["megatecla"], run = "gui.unknown" }]"#,
        )
        .unwrap();
        match Effective::build_for(&preset, &[], &[], Screen::Browse) {
            Err(KeymapError::BadChord { .. }) => {}
            other => panic!("esperaba BadChord, fue {other:?}"),
        }
    }

    /// H1 (#24): el contexto `dialog` existe — un preset con [dialog]
    /// construye y resuelve para `Screen::Dialog`.
    #[test]
    fn dialog_context_se_parsea_y_construye() {
        let preset =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        let eff = Effective::build_for(&preset, &[], &["dialog.approve"], Screen::Dialog)
            .expect("construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
            run("dialog.approve")
        );
    }

    /// Una capa de usuario extiende [dialog] con prepend y GANA.
    #[test]
    fn capa_puede_extender_dialog() {
        let preset =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        let layer =
            parse_keymap("[dialog]\nprepend_keymap = [{ on = [\"y\"], run = \"dialog.deny\" }]\n")
                .unwrap();
        let eff = Effective::build_for(
            &preset,
            &[layer],
            &["dialog.approve", "dialog.deny"],
            Screen::Dialog,
        )
        .expect("construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
            run("dialog.deny")
        );
    }

    /// Capa con `keymap` completo en [dialog]: error, como en el resto.
    #[test]
    fn has_full_keymap_ve_dialog() {
        let layer =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        assert!(layer.has_full_keymap());
    }

    /// The table [`paint_chord`] documents, walked over what `Display`
    /// actually writes — the round trip is the point: every spelling here is
    /// produced by formatting a real [`Chord`], never hand-written, so the
    /// table cannot drift from the `Display` it is the counterpart of.
    #[test]
    fn paint_chord_spells_a_key_the_way_the_docs_do() {
        for (raw, painted) in [
            ("f1", "F1"),
            ("f12", "F12"),
            ("shift+f8", "Shift+F8"),
            ("ctrl+alt+f5", "Ctrl+Alt+F5"),
            // `cmd` joined the modifier table; `Display` writes it FIRST, so
            // this fixture is also the canonical-order pin.
            ("cmd+f5", "Cmd+F5"),
            ("cmd+ctrl+alt+shift+f5", "Cmd+Ctrl+Alt+Shift+F5"),
            ("enter", "Enter"),
            ("tab", "Tab"),
            ("esc", "Esc"),
            ("backspace", "Backspace"),
            ("space", "Space"),
            ("plus", "Plus"),
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("home", "Home"),
            ("end", "End"),
            ("pgup", "PgUp"),
            ("pgdn", "PgDn"),
            ("insert", "Insert"),
            ("delete", "Delete"),
        ] {
            assert_eq!(
                parse_chord(raw).expect("chord del catálogo").to_string(),
                raw,
                "el fixture tiene que ser lo que `Display` escribe de verdad"
            );
            assert_eq!(paint_chord(raw), painted, "{raw}");
        }
    }

    /// THE case that matters: a single printable character is painted
    /// EXACTLY as it is bound. Telling a reader to press `Y` is telling them
    /// to press Shift — and it is not only cosmetic, because `Char('Y')` is a
    /// different binding that `parse_chord` would resolve to a different key.
    /// Holds under a modifier too (`Ctrl+k`, never `Ctrl+K`).
    #[test]
    fn paint_chord_never_shifts_a_printable_key() {
        for chord in [
            "y",
            "n",
            "k",
            "G",
            "ctrl+k",
            "alt+p",
            "ctrl+alt+k",
            "ñ",
            "漢",
        ] {
            let painted = paint_chord(chord);
            let key = painted.rsplit('+').next().expect("siempre hay tecla");
            let bound = chord.rsplit('+').next().expect("siempre hay tecla");
            assert_eq!(key, bound, "{chord} → {painted}: la TECLA no se toca");
        }
        assert_eq!(paint_chord("ctrl+k"), "Ctrl+k");
        assert_eq!(
            paint_chord("G"),
            "G",
            "…y la mayúscula ligada sigue mayúscula"
        );
    }

    /// A multi-key SEQUENCE keeps its space join, and every key of it is
    /// spelled.
    #[test]
    fn paint_chord_spells_every_key_of_a_sequence() {
        assert_eq!(paint_chord("g g"), "g g");
        assert_eq!(paint_chord("g home"), "g Home");
        assert_eq!(paint_chord("ctrl+x f5"), "Ctrl+x F5");
    }

    /// Nothing unrecognised is invented: an `f` that is not a function key,
    /// a modifier spelled wrong, an empty string — all pass through.
    #[test]
    fn paint_chord_invents_nothing() {
        for raw in ["", "fx", "f", "megakey", "CTRL+k", "f5x"] {
            assert_eq!(
                paint_chord(raw),
                raw,
                "{raw:?} no se reconoce: pasa tal cual"
            );
        }
    }

    /// A preset that binds a Planned command LOADS, and the binding survives
    /// carrying its reason. Without this a faithful Total Commander preset
    /// cannot exist: a third of it names commands norte has not built.
    #[test]
    fn un_binding_a_comando_planned_sobrevive_marcado() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["alt+f1"], run = "pane.select-drive" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
        let all = eff.bindings_all();
        let (_, run, avail) = all
            .iter()
            .find(|(_, run, _)| *run == "pane.select-drive")
            .expect("el binding no puede desaparecer");
        assert_eq!(*run, "pane.select-drive");
        assert!(matches!(avail, Availability::NotBuilt { .. }), "{avail:?}");
    }

    /// A command the catalogue calls Live but THIS frontend does not implement
    /// is kept as `NotHere` instead of being filtered away in silence — the
    /// H3f bug.
    #[test]
    fn un_comando_live_que_este_frontend_no_implementa_es_not_here() {
        let preset = parse_keymap(
            r#"
[global]
keymap = [ { on = ["f1"], run = "app.help" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
        let all = eff.bindings_all();
        let (_, _, avail) = all
            .iter()
            .find(|(_, run, _)| *run == "app.help")
            .expect("no puede desaparecer");
        assert_eq!(*avail, Availability::NotHere);
    }

    /// `bindings()` keeps its old meaning — only what actually runs — so the
    /// help and the hints render exactly as before this change.
    #[test]
    fn bindings_solo_devuelve_lo_ejecutable() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [
    { on = ["f5"], run = "pane.copy" },
    { on = ["alt+f1"], run = "pane.select-drive" },
]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
        let runs: Vec<&str> = eff.bindings().into_iter().map(|(_, run)| run).collect();
        assert_eq!(runs, vec!["pane.copy"]);
    }

    /// A name absent from the CATALOGUE is a typo and still dies loudly, in a
    /// preset and in a user layer alike. "Not built yet" and "you misspelled
    /// it" stop being the same event; they must not become the same event
    /// again.
    #[test]
    fn un_nombre_fuera_del_catalogo_sigue_siendo_error() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["f5"], run = "pane.copyy" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::UnknownCommand { .. }), "{e:?}");
    }

    /// An unavailable binding SHADOWS a lower-precedence available one. If a
    /// preset puts `alt+f1` in `[pane]`, the key must say "drives are not
    /// built" rather than quietly falling through to whatever `[global]` had —
    /// falling through is how a Total Commander user gets a surprise instead
    /// of an answer.
    #[test]
    fn un_binding_no_disponible_ensombrece_al_de_global() {
        let preset = parse_keymap(
            r#"
[global]
keymap = [ { on = ["alt+f1"], run = "pane.refresh" } ]

[pane]
keymap = [ { on = ["alt+f1"], run = "pane.select-drive" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.refresh"], Screen::Browse).unwrap();
        let all = eff.bindings_all();
        let hits: Vec<_> = all.iter().filter(|(seq, _, _)| seq == "alt+f1").collect();
        assert_eq!(hits.len(), 1, "el dedup deja UNA por secuencia: {all:?}");
        assert_eq!(
            hits[0].1, "pane.select-drive",
            "gana el contexto específico"
        );
        assert!(matches!(hits[0].2, Availability::NotBuilt { .. }));
    }

    /// Unavailable bindings take part in the prefix-free check: the shape of
    /// the map is a load-time property (ADR 0006), independent of what runs.
    #[test]
    fn un_binding_no_disponible_sigue_contando_para_prefix_free() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [
    { on = ["g"], run = "pane.select-drive" },
    { on = ["g", "g"], run = "cursor.top" },
]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::AmbiguousPrefix { .. }), "{e:?}");
    }

    /// Pressing a key bound to something norte has not built returns a third
    /// outcome. `Reset` would be indistinguishable from an unbound key, which is
    /// precisely the silence this work exists to remove.
    #[test]
    fn una_tecla_no_disponible_resuelve_a_unavailable() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["alt+f1"], run = "pane.select-drive" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        let chord = parse_chord("alt+f1").unwrap();
        match r.push(chord) {
            Resolution::Unavailable { command, why } => {
                assert_eq!(command, "pane.select-drive");
                assert!(matches!(why, Availability::NotBuilt { .. }), "{why:?}");
            }
            other => panic!("esperaba Unavailable, salió {other:?}"),
        }
        assert!(r.pending().is_empty(), "la secuencia debe quedar limpia");
    }

    /// The message must NAME the command and, when the reason exists, carry it —
    /// a "not available" with no subject is the silence with extra steps.
    #[test]
    fn el_mensaje_de_no_disponible_nombra_el_comando_y_el_motivo() {
        let m = unavailable_message(
            "pane.select-drive",
            Availability::NotBuilt {
                reason: "keymap-reason-volume-enumeration",
                issue: 131,
            },
        );
        assert!(m.contains("pane.select-drive"), "{m}");
        assert!(m.contains("131"), "{m}");
        // The ID must have been TRANSLATED, not interpolated raw.
        assert!(!m.contains("keymap-reason-"), "{m}");

        let m = unavailable_message("pane.hotlist", Availability::NotHere);
        assert!(m.contains("pane.hotlist"), "{m}");
    }

    /// encoding-auditor MINOR 6: the assertion above runs in the AMBIENT
    /// locale (`Lang::from_env`), so exactly one of the two is pinned and
    /// which one depends on the developer's `LANG`. A locale that dropped
    /// `{ $issue }` would silently lose the issue number — undoing, at the
    /// last step, precisely what `todo_planned_tiene_motivo_e_issue` exists
    /// to guarantee. Pin both explicitly.
    #[test]
    fn el_mensaje_de_no_disponible_lleva_los_tres_argumentos_en_ambos_locales() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let m = norte_i18n::ta_in(
                lang,
                "keymap-unavailable-not-built",
                &[
                    ("command", "pane.select-drive"),
                    ("reason", "MOTIVO"),
                    ("issue", "131"),
                ],
            );
            assert!(m.contains("pane.select-drive"), "{lang:?}: {m}");
            assert!(m.contains("MOTIVO"), "{lang:?}: {m}");
            assert!(m.contains("131"), "{lang:?}: {m}");

            let m = norte_i18n::ta_in(
                lang,
                "keymap-unavailable-not-here",
                &[("command", "pane.hotlist")],
            );
            assert!(m.contains("pane.hotlist"), "{lang:?}: {m}");
        }
    }

    /// The ignored-count sentence must NAME both halves: which command refused
    /// the count and which number went nowhere. "A count was ignored" tells
    /// the user nothing they can act on.
    #[test]
    fn el_mensaje_de_contador_ignorado_nombra_comando_y_numero() {
        let m = count_ignored_message("app.quit", 3);
        assert!(m.contains("app.quit"), "{m}");
        assert!(m.contains('3'), "{m}");
    }

    /// Same reasoning as `..._en_ambos_locales` above: the assertion over the
    /// ambient locale pins exactly one of the two, and which one depends on
    /// the developer's `LANG`. A locale that dropped `{ $count }` would lose
    /// the number in half the world.
    #[test]
    fn el_mensaje_de_contador_ignorado_lleva_los_dos_argumentos_en_ambos_locales() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let m = norte_i18n::ta_in(
                lang,
                "keymap-count-ignored",
                &[("command", "app.quit"), ("count", "3")],
            );
            assert!(m.contains("app.quit"), "{lang:?}: {m}");
            assert!(m.contains('3'), "{lang:?}: {m}");
        }
    }

    /// encoding-auditor MINOR 5: `unavailable_message` interpolates its
    /// `command` into a terminal sentence WITHOUT masking, and the argument
    /// that this is safe is that the string is always byte-equal to a
    /// `CommandDef.name` — `catalogue::lookup` is byte-exact `&str` equality,
    /// so a hostile `run` can only ever miss and become a load error. That
    /// argument is real, and until now nothing held it in place. Pin both
    /// halves: the catalogue is ASCII, and a hostile `run` classifies as
    /// `UnknownCommand` rather than surfacing as `Unavailable`.
    #[test]
    fn un_run_hostil_es_error_de_carga_jamas_una_indisponibilidad() {
        for d in CATALOGUE {
            assert!(
                d.name.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'.' | b'-')),
                "{} no es ASCII seguro — el mensaje de indisponibilidad lo \
                 interpola SIN enmascarar y esa es la única razón por la que \
                 puede",
                d.name
            );
        }
        for h in norte_testkit::corpus::hostile_runs() {
            // Through `parse_keymap`, with the hazards written as `\uXXXX` —
            // a TOML basic string is exactly how such a name would ship.
            let escaped: String = h
                .run
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' {
                        c.to_string()
                    } else {
                        format!("\\u{:04X}", c as u32)
                    }
                })
                .collect();
            let kf = parse_keymap(&format!(
                "[pane]\nkeymap = [{{ on = [\"alt+f1\"], run = \"{escaped}\" }}]\n"
            ))
            .unwrap_or_else(|e| panic!("{}: el TOML debe parsear: {e}", h.id));
            let err = Effective::build_for(&kf, &[], &["pane.copy"], Screen::Browse)
                .expect_err(&format!("{}: debe fallar la carga — {}", h.id, h.why));
            assert!(
                matches!(err, KeymapError::UnknownCommand { .. }),
                "{}: {err:?}",
                h.id
            );
        }
        // K2a widened the argument. `count_ignored_message` interpolates a
        // command name unmasked too, and its reachable domain is STRICTLY
        // larger than `unavailable_message`'s: a `lua:` command never becomes
        // `Unavailable` (it is always `Availability::Here`) but a count over
        // one is ALWAYS `Ignored`, so a Lua name reaches the status bar
        // through this sentence and no other. It is safe because
        // `valid_lua_name` is ASCII by construction — pin that leg too, so
        // the day someone widens the Lua charset this fails instead of the
        // status line.
        for hostile in ["lua:aa\u{202E}bb", "lua:x\u{0007}y", "lua:ñ"] {
            let name = hostile.strip_prefix("lua:").expect("prefijo lua:");
            assert!(
                !valid_lua_name(name),
                "{hostile}: un nombre lua con hazards debe ser rechazado por el charset"
            );
        }
        let m = count_ignored_message("lua:mi-script.v2", 3);
        assert!(
            !m.chars().any(|c| c.is_control()
                || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}')),
            "el mensaje de contador ignorado no puede llevar hazards: {m:?}"
        );
    }

    /// The tripwire for the obvious next change. `unavailable_message` names
    /// the COMMAND but not the KEY, and "Alt+F1: not built yet" is the
    /// natural improvement someone will make in K3. The chord is exactly
    /// where the hostile bytes live — `parse_chord` accepts any lone
    /// codepoint as `Char`, `Chord`'s `Display` is raw on purpose, and
    /// `norte_i18n` runs with `set_use_isolating(false)` (a terminal wants no
    /// FSI/PDI cells), so there is no second line of defence: one RLO
    /// reorders the whole status line. Trivially true today; it fails the day
    /// the chord arrives unmasked, which is the entire point.
    #[test]
    fn el_mensaje_de_no_disponible_jamas_lleva_un_hazard_de_terminal() {
        for d in CATALOGUE {
            for why in [
                Availability::NotHere,
                Availability::NotBuilt {
                    reason: "keymap-reason-volume-enumeration",
                    issue: 131,
                },
            ] {
                let m = unavailable_message(d.name, why);
                assert!(
                    !m.chars().any(norte_encoding::is_terminal_hazard),
                    "{}: {:?}",
                    d.name,
                    m.escape_debug()
                );
            }
        }
    }

    /// The same tripwire for the SHORT form (K3b), which is now the one that
    /// reaches a pipe (`norte help keys`) and a JSON field, in both locales.
    #[test]
    fn el_mensaje_corto_tampoco_lleva_un_hazard_de_terminal() {
        for d in CATALOGUE {
            let why = match d.status {
                Status::Live => Availability::NotHere,
                Status::Planned { reason, issue } => Availability::NotBuilt { reason, issue },
            };
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let m = short_unavailable_message(why, lang);
                assert!(
                    !m.chars().any(norte_encoding::is_terminal_hazard),
                    "{} [{lang:?}]: {:?}",
                    d.name,
                    m.escape_debug()
                );
            }
        }
    }

    /// The short form SAYS the key is not built, in both locales. The wording
    /// is not decoration: `norte help keys` writes to a pipe and cannot dim a
    /// row, so `writing archives (#132)` on its own reads as a description of
    /// what the key does. A later edit back to the bare reason would restore
    /// exactly the confusion K3b removed, and would otherwise be green.
    #[test]
    fn el_mensaje_corto_dice_que_no_esta_construido_en_ambos_locales() {
        let why = Availability::NotBuilt {
            reason: "keymap-reason-archive-write",
            issue: 132,
        };
        for (lang, marker) in [
            (norte_i18n::Lang::En, "not built"),
            (norte_i18n::Lang::Es, "aún no construido"),
        ] {
            let m = short_unavailable_message(why, lang);
            assert!(m.contains(marker), "{lang:?}: {m}");
            assert!(m.contains("132"), "{lang:?}: {m}");
            assert!(
                !m.contains("keymap-reason-"),
                "the reason is a Fluent id and must be TRANSLATED: {lang:?}: {m}"
            );
        }
    }

    /// Masking runs FIRST and the cosmetics cannot undo it (encoding audit
    /// H1): every hostile chord of the canonical corpus — bindable from an
    /// untrusted project `./.norte/keymap.toml` — comes out as `U+FFFD`, with
    /// no hazard surviving, whether it is the whole chord or one key of a
    /// sequence under a modifier.
    #[test]
    fn paint_chord_masks_before_it_prettifies() {
        for hazard in norte_testkit::corpus::hostile_chords() {
            for raw in [
                hazard.token.to_string(),
                format!("ctrl+{}", hazard.token),
                format!("f5 {}", hazard.token),
            ] {
                let painted = paint_chord(&raw);
                assert!(
                    !painted.chars().any(norte_encoding::is_terminal_hazard),
                    "[{}] hazard crudo tras pintar {raw:?}: {painted:?}",
                    hazard.id
                );
                assert!(
                    painted.contains('\u{FFFD}'),
                    "[{}] el hazard debe quedar en U+FFFD: {painted:?}",
                    hazard.id
                );
            }
        }
    }

    // --- K2a: el contador numérico -------------------------------------

    /// `5j` runs the command five times. The count rides WITH the command; the
    /// frontend is what repeats, so no command signature changes.
    #[test]
    fn un_contador_llega_con_el_comando() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("5").unwrap()), Resolution::Counting(5));
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(5),
            }
        );
    }

    /// Digits accumulate left to right, and the count survives a multi-key
    /// sequence: `12gj` is twelve, not one then two.
    ///
    /// The sequence deliberately runs `cursor.down` and not `cursor.top`.
    /// K2a's plan wrote this fixture as vim's `12gg`, but the catalogue marks
    /// `cursor.top` as taking NO count — and it is right: the count REPEATS
    /// the dispatch, so twelve "go to the top" is still the top. `12gg`
    /// meaning "go to line 12" would need the count to reach the command,
    /// which is exactly the design this task rejected. A count over
    /// `cursor.top` is therefore [`Count::Ignored`], pinned by
    /// `un_contador_sobre_un_comando_sin_contador_no_se_traga`.
    #[test]
    fn los_digitos_se_acumulan_y_sobreviven_a_una_secuencia() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["g", "j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("1").unwrap()), Resolution::Counting(1));
        assert_eq!(r.push(parse_chord("2").unwrap()), Resolution::Counting(12));
        assert_eq!(r.push(parse_chord("g").unwrap()), Resolution::Pending(1));
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(12),
            }
        );
    }

    /// A digit typed MID-sequence is a key, not a count: with `g` pending,
    /// `5` must reach the lookup. Otherwise a preset could never bind a
    /// sequence whose second chord is a digit, and the count would silently
    /// eat it.
    #[test]
    fn un_digito_a_mitad_de_secuencia_es_una_tecla() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["g", "5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(r.push(parse_chord("2").unwrap()), Resolution::Counting(2));
        assert_eq!(r.push(parse_chord("g").unwrap()), Resolution::Pending(1));
        assert_eq!(
            r.push(parse_chord("5").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(2),
            }
        );
    }

    /// A count over a command the catalogue says takes none is NOT swallowed:
    /// the command runs once and the frontend is told the count was ignored.
    #[test]
    fn un_contador_sobre_un_comando_sin_contador_no_se_traga() {
        let preset = parse_keymap(
            r#"
counts = true

[global]
keymap = [ { on = ["q"], run = "app.quit" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["app.quit"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("3").unwrap());
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run {
                command: "app.quit".to_owned(),
                count: Count::Ignored(3),
            }
        );
    }

    /// Zero never STARTS a count — `0` stays a bindable key, which is what
    /// vim's "go to the first column" and mc's mask keys rely on. It does
    /// accumulate once a count is open: `10` is ten.
    #[test]
    fn el_cero_no_abre_un_contador_pero_si_acumula() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [
    { on = ["0"], run = "cursor.top" },
    { on = ["j"], run = "cursor.down" },
]
"#,
        )
        .unwrap();
        let known = ["cursor.top", "cursor.down"];
        let eff = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        // A bare 0 is the binding, not a count.
        assert_eq!(
            r.push(parse_chord("0").unwrap()),
            Resolution::Run {
                command: "cursor.top".to_owned(),
                count: Count::None
            }
        );
        // But 1 then 0 is ten.
        assert_eq!(r.push(parse_chord("1").unwrap()), Resolution::Counting(1));
        assert_eq!(r.push(parse_chord("0").unwrap()), Resolution::Counting(10));
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(10)
            }
        );
    }

    /// Four digits is the ceiling. A fifth is dropped rather than wrapping the
    /// accumulator — 99999 must not silently become something else.
    #[test]
    fn el_contador_topa_en_cuatro_digitos() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        for _ in 0..5 {
            r.push(parse_chord("9").unwrap());
        }
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::Repeat(9999)
            }
        );
    }

    /// Esc clears the count as well as the pending sequence. A count left
    /// stuck to the next keystroke is the worst failure this feature can have.
    #[test]
    fn esc_limpia_el_contador() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("7").unwrap());
        assert_eq!(r.push(parse_chord("esc").unwrap()), Resolution::Reset);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// An unbound key clears the count too — otherwise a typo leaves a number
    /// glued to whatever you press next.
    #[test]
    fn una_tecla_sin_binding_limpia_el_contador() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("4").unwrap());
        assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// A key the FRONTEND does not model (`Resolver::reset`, the explicit
    /// equivalent of a miss) clears the count for the same reason an unbound
    /// key does: a number must never outlive the keystroke that ended it.
    #[test]
    fn reset_limpia_el_contador() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        r.push(parse_chord("6").unwrap());
        assert_eq!(r.count(), Some(6));
        r.reset();
        assert_eq!(r.count(), None);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// Without the preset flag a digit is just a key: `orthodox` and `cua`
    /// must not grow counts behind their users' backs.
    #[test]
    fn sin_el_flag_del_preset_un_digito_es_una_tecla() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("5").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// A digit with a modifier was never a count: `ctrl+5` is an ordinary
    /// chord, even with counts on.
    #[test]
    fn un_digito_con_modificador_no_es_un_contador() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["ctrl+5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("ctrl+5").unwrap()),
            Resolution::Run {
                command: "cursor.down".to_owned(),
                count: Count::None
            }
        );
    }

    /// The count POLICY belongs to the preset: a user layer that could flip it
    /// on would silently change what every digit key means.
    #[test]
    fn una_capa_de_usuario_no_puede_encender_los_contadores() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"
counts = true

[pane]
prepend_keymap = [ { on = ["k"], run = "cursor.up" } ]
"#,
        )
        .unwrap();
        let known = ["cursor.down", "cursor.up"];
        let e = Effective::build_for(&preset, &[layer], &known, Screen::Browse).unwrap_err();
        assert!(
            matches!(e, KeymapError::WrongLayerKey { key: "counts", .. }),
            "{e:?}"
        );
    }

    /// With counts on, a digit key bound in the same context is a LOAD error,
    /// not silent precedence. Same spirit as prefix-free: the conflict
    /// surfaces when the file loads, not when a finger slips.
    #[test]
    fn un_digito_ligado_con_contadores_es_error_de_carga() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
        assert!(
            matches!(e, KeymapError::DigitBoundWithCounts { .. }),
            "{e:?}"
        );
    }

    /// `0` is exempt, because a count never starts with zero — binding it
    /// stays legal even with counts on.
    #[test]
    fn el_cero_sigue_siendo_ligable_con_contadores() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["0"], run = "cursor.top" } ]
"#,
        )
        .unwrap();
        Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("0 con contadores es legal");
    }

    /// "Digit" means ASCII 0-9 and nothing else, in BOTH paths — they share
    /// `digit_of`, and `char::to_digit(10)` is ASCII-only. So U+0665 ARABIC-
    /// INDIC DIGIT FIVE and U+FF15 FULLWIDTH FIVE are ordinary bindable keys
    /// that load fine AND never open a count. Pinned in both directions
    /// because the two answers must stay the same one: an "improvement" to
    /// `is_numeric()` in either path would make a key that loads as a binding
    /// and then resolves as a count, or the reverse.
    #[test]
    fn un_digito_no_ascii_es_una_tecla_normal_en_los_dos_caminos() {
        for exotico in ['\u{0665}', '\u{FF15}'] {
            let preset = parse_keymap(&format!(
                "counts = true\n\n[pane]\nkeymap = [ {{ on = [\"{exotico}\"], run = \"cursor.down\" }} ]\n"
            ))
            .unwrap();
            let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse)
                .unwrap_or_else(|e| panic!("{exotico:?} debe poder ligarse: {e:?}"));
            let mut r = Resolver::new(eff);
            assert_eq!(
                r.push(parse_chord(&exotico.to_string()).unwrap()),
                Resolution::Run {
                    command: "cursor.down".to_owned(),
                    count: Count::None,
                },
                "{exotico:?} no abre un contador: es una tecla"
            );
        }
    }

    /// A digit MID-sequence is an ordinary key: the rule looks at the first
    /// chord only, exactly like the accumulator, which never opens a count
    /// with a sequence in flight.
    #[test]
    fn un_digito_a_mitad_de_secuencia_sigue_siendo_ligable_con_contadores() {
        let preset = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["g", "5"], run = "cursor.top" } ]
"#,
        )
        .unwrap();
        Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("un dígito no inicial es una tecla más");
    }

    /// Specification §12: Tab switches panes and a preset may not take it.
    /// K2b imports four foreign keymaps, which is when this stops being
    /// theoretical.
    #[test]
    fn un_preset_no_puede_repinar_tab() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
    }

    /// Tab may not OPEN a sequence either. A preset that binds `["tab","j"]`
    /// and no bare `tab` would leave Tab sitting pending, which loses pane
    /// switching just as completely as rebinding it (specification §12).
    #[test]
    fn tab_tampoco_puede_abrir_una_secuencia() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab", "j"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let e = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
    }

    /// The prohibition is on the BROWSE screen only. Every bundled preset
    /// binds `tab` to `dialog.pane` inside `[dialog]`, and that is not pane
    /// switching — blanket-banning the key would break the dialogs we ship.
    #[test]
    fn tab_sigue_siendo_libre_en_el_contexto_de_dialogo() {
        let preset = parse_keymap(
            r#"
[dialog]
keymap = [ { on = ["tab"], run = "dialog.pane" } ]
"#,
        )
        .unwrap();
        Effective::build_for(&preset, &[], &["dialog.pane"], Screen::Dialog)
            .expect("tab en dialog es legal");
    }

    /// A user layer cannot take Tab either — the rule is about the effective
    /// map, not about who wrote the line.
    #[test]
    fn una_capa_de_usuario_tampoco_puede_tomar_tab() {
        let preset = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab"], run = "pane.switch" } ]
"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"
[pane]
prepend_keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let known = ["pane.switch", "cursor.down"];
        let e = Effective::build_for(&preset, &[layer], &known, Screen::Browse).unwrap_err();
        assert!(matches!(e, KeymapError::SacredKey { .. }), "{e:?}");
    }

    /// `norte doctor` must not be the one path that stays quiet: both new
    /// rules are reported by the diagnostic walk too, which is the contract
    /// [`check_binding`]'s rustdoc states for every load-time rule.
    #[test]
    fn el_diagnostico_tambien_reporta_las_dos_reglas_nuevas() {
        let digito = parse_keymap(
            r#"
counts = true

[pane]
keymap = [ { on = ["5"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let d = Effective::build_diagnostics(&digito, &[], &["cursor.down"], Screen::Browse);
        assert!(
            d.iter().any(|f| matches!(
                f,
                KeymapDiagnostic::Structural { message } if message.contains('5')
            )),
            "{d:?}"
        );

        let tab = parse_keymap(
            r#"
[pane]
keymap = [ { on = ["tab"], run = "cursor.down" } ]
"#,
        )
        .unwrap();
        let d = Effective::build_diagnostics(&tab, &[], &["cursor.down"], Screen::Browse);
        assert!(
            d.iter().any(|f| matches!(
                f,
                KeymapDiagnostic::Structural { message } if message.contains("tab")
            )),
            "{d:?}"
        );
    }

    /// The two loaders must denounce the SAME defect, which is the contract
    /// [`check_binding`]'s rustdoc states for every load-time rule: they share
    /// the checks precisely so they cannot drift.
    ///
    /// `el_diagnostico_tambien_reporta_las_dos_reglas_nuevas` proves less than
    /// it looks. It pins two keymaps and asks only whether SOMETHING
    /// `Structural` came back mentioning `5` or `tab` — a `build_diagnostics`
    /// that had lost `check_sacred` but kept `check_prefix_free` would still
    /// pass it if any other rule fired, and nothing at all pins the rules the
    /// two new ones were bolted next to. This walks every rule, one keymap per
    /// rule, and compares the RENDERED message against the error
    /// [`Effective::build_for`] returns for the same input: a rule wired into
    /// one path and not the other fails here, and so does a rule wired into
    /// both with different arguments (`check_digits_free(.., false)` in the
    /// diagnostic walk, say).
    #[test]
    fn los_dos_cargadores_denuncian_el_mismo_defecto() {
        const KNOWN: &[&str] = &["app.quit", "cursor.down", "cursor.top", "pane.switch"];
        // (nombre, preset, capa de usuario)
        let casos: &[(&str, &str, Option<&str>)] = &[
            (
                "tecla inválida",
                "[pane]\nkeymap = [{ on = [\"megatecla\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "secuencia vacía",
                "[pane]\nkeymap = [{ on = [], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "esc dentro de una secuencia",
                "[pane]\nkeymap = [{ on = [\"esc\", \"a\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "comando desconocido",
                "[pane]\nkeymap = [{ on = [\"x\"], run = \"typo.no-existe\" }]\n",
                None,
            ),
            (
                "nombre lua fuera del charset",
                "[pane]\nkeymap = [{ on = [\"x\"], run = \"lua:Nombre Malo\" }]\n",
                None,
            ),
            (
                "prefijo ambiguo",
                "[pane]\nkeymap = [\n { on = [\"z\"], run = \"cursor.down\" },\n { on = [\"z\", \"z\"], run = \"cursor.top\" },\n]\n",
                None,
            ),
            (
                "clave en la capa equivocada",
                "[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
                Some("[pane]\nkeymap = [{ on = [\"k\"], run = \"cursor.top\" }]\n"),
            ),
            (
                "la capa enciende los contadores",
                "[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
                Some(
                    "counts = true\n[pane]\nprepend_keymap = [{ on = [\"k\"], run = \"cursor.top\" }]\n",
                ),
            ),
            (
                "dígito ligado con contadores",
                "counts = true\n[pane]\nkeymap = [{ on = [\"5\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "tab repinada",
                "[pane]\nkeymap = [{ on = [\"tab\"], run = \"cursor.down\" }]\n",
                None,
            ),
            (
                "tab abriendo una secuencia",
                "[pane]\nkeymap = [{ on = [\"tab\", \"j\"], run = \"cursor.down\" }]\n",
                None,
            ),
        ];
        for (nombre, preset_src, layer_src) in casos {
            let preset = parse_keymap(preset_src).unwrap_or_else(|e| panic!("{nombre}: {e}"));
            let layers: Vec<KeymapFile> = layer_src
                .iter()
                .map(|s| parse_keymap(s).unwrap_or_else(|e| panic!("{nombre}: capa: {e}")))
                .collect();
            let e = Effective::build_for(&preset, &layers, KNOWN, Screen::Browse)
                .err()
                .unwrap_or_else(|| panic!("{nombre}: build_for lo aceptó"));
            let d = Effective::build_diagnostics(&preset, &layers, KNOWN, Screen::Browse);
            let mismo = d.iter().any(|f| match f {
                KeymapDiagnostic::Structural { message } => *message == e.to_string(),
                // El único hallazgo que NO se renderiza desde el error: el
                // nombre desconocido llano es recuperable, así que viaja
                // tipado. Se compara el `run`, que es lo que lo identifica.
                KeymapDiagnostic::UnknownCommand { run } => {
                    matches!(&e, KeymapError::UnknownCommand { run: r } if r == run)
                }
            });
            assert!(
                mismo,
                "{nombre}: build_for dijo {e:?}, el diagnóstico dijo {d:?}"
            );
        }

        // Y el otro lado del contrato: lo que `build_for` acepta no puede
        // dejar hallazgos. Las dos reglas de K2a tienen una forma LEGAL cada
        // una (el `0` ligado con contadores encendidos, `tab` en su comando
        // reservado) y un falso positivo aquí llenaría `norte doctor` de
        // ruido sobre un keymap sano.
        let limpio = parse_keymap(
            r#"
counts = true

[pane]
keymap = [
    { on = ["tab"], run = "pane.switch" },
    { on = ["0"], run = "cursor.top" },
    { on = ["g", "5"], run = "cursor.down" },
]
"#,
        )
        .expect("el keymap limpio parsea");
        Effective::build_for(&limpio, &[], KNOWN, Screen::Browse).expect("build_for lo acepta");
        let d = Effective::build_diagnostics(&limpio, &[], KNOWN, Screen::Browse);
        assert!(d.is_empty(), "falso positivo del diagnóstico: {d:?}");
    }

    /// The three bundled presets must survive both rules unchanged.
    #[test]
    fn los_presets_de_fabrica_pasan_las_dos_reglas_nuevas() {
        for name in presets::NAMES {
            let src = presets::source(name).expect("NAMES resuelve");
            let kf = parse_keymap(src).expect("preset parsea");
            for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                let known = preset_commands(screen);
                let known: Vec<&str> = known.iter().map(String::as_str).collect();
                Effective::build_for(&kf, &[], &known, screen)
                    .unwrap_or_else(|e| panic!("{name} en {screen:?}: {e:?}"));
            }
        }
    }

    /// WHICH presets count is a decision, not an implementation detail: `vim`
    /// does because vim does, `orthodox` and `cua` do not because their
    /// originals do not and turning it on would take `1`..`9` away from them.
    ///
    /// Nothing else in the suite notices if `counts = true` leaves
    /// `vim.toml` — no bundled preset binds a bare digit, so the load rule
    /// stays quiet either way and the only symptom would be that `5j` silently
    /// stops counting. K2b lands four more imported keymaps; this is the line
    /// that says which of them may flip the flag.
    #[test]
    fn solo_vim_trae_los_contadores_encendidos() {
        let known = preset_commands(Screen::Browse);
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        for name in presets::NAMES {
            let src = presets::source(name).expect("NAMES resuelve");
            let kf = parse_keymap(src).expect("preset parsea");
            let eff = Effective::build_for(&kf, &[], &known, Screen::Browse)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(eff.counts(), *name == "vim", "preset {name}");
        }
    }
}
