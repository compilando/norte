//! Motor de keymap PURO compartido por los frontends (ADR 0006/0007): mapa
//! `(contexto, secuencia) → comando`, capas estilo Yazi, efectivo prefix-free
//! validado al cargar — la resolución es un scan lineal determinista sobre el
//! efectivo (≤ centenas de bindings), sin timeouts. Tecla NEUTRA (sin
//! crossterm/gpui): cada frontend convierte su evento nativo a [`Chord`] con
//! [`Chord::new`].

pub mod catalogue;
mod chord;
mod effective;
mod layer;
pub mod presets;
mod resolve;

pub use catalogue::{CATALOGUE, CommandDef, Status};
pub use chord::{Chord, KeyCode, Mods, paint_chord, parse_chord};
pub use effective::{Effective, valid_lua_name};
pub use layer::{KeymapFile, Screen, parse_keymap};
pub use resolve::{Resolution, Resolver};

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
    let specific: fn(&KeymapFile) -> &RawSection = match screen {
        Screen::Browse => |f| &f.pane,
        Screen::Viewer => |f| &f.viewer,
        Screen::Dialog => |f| &f.dialog,
    };
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
    /// `F(n > 12)` is deliberately EXCLUDED: `Display` renders ANY `F(n)` as
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
        for code in non_char {
            let c = Chord::new(Mods::default(), code);
            assert_eq!(parse_chord(&c.to_string()).unwrap(), c, "{code:?}");
        }
        for n in 1..=12u8 {
            let c = Chord::new(Mods::default(), KeyCode::F(n));
            assert_eq!(parse_chord(&c.to_string()).unwrap(), c, "F({n})");
        }
        for ch in [' ', '+', 'G', 'ñ'] {
            let c = Chord::new(Mods::default(), KeyCode::Char(ch));
            assert_eq!(parse_chord(&c.to_string()).unwrap(), c, "{ch:?}");
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
        assert_eq!(
            r.push(parse_chord("g").unwrap()),
            Resolution::Run("cursor.top".into())
        );
        // Tras ejecutar, el estado queda limpio.
        assert_eq!(
            r.push(parse_chord("G").unwrap()),
            Resolution::Run("cursor.bottom".into())
        );
        // Tecla sin binding: reset silencioso.
        assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
        // Prefijo pendiente + tecla que no continúa: reset (no ejecuta nada).
        r.push(parse_chord("g").unwrap());
        assert_eq!(r.push(parse_chord("q").unwrap()), Resolution::Reset);
        // q suelto (contexto global) sí corre.
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("app.quit".into())
        );
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
        assert_eq!(
            r.push(parse_chord("esc").unwrap()),
            Resolution::Run("app.quit".into())
        );
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
            Resolution::Run("cursor.up".into()),
            "pane.append gana a global.keymap (especificidad > capa)"
        );
        // …y un prepend de usuario en [global] NO pisa al keymap [pane].
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run("cursor.down".into()),
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
            Resolution::Run("lua:mi-comando.v2".into()),
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
            Resolution::Run("cursor.top".into()),
            "prepend PISA al preset"
        );
        assert_eq!(
            r.push(parse_chord("k").unwrap()),
            Resolution::Run("cursor.up".into()),
            "append NO pisa una secuencia existente"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Run("app.quit".into()),
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
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("cursor.up".into())
        );
        assert_eq!(
            r.push(parse_chord("tab").unwrap()),
            Resolution::Run("pane.switch".into())
        );
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
            Resolution::Run("cursor.top".into()),
            "el prepend de la capa MÁS alta gana"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Run("cursor.bottom".into()),
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
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("app.quit".into())
        );
        assert_eq!(
            r.push(parse_chord("enter").unwrap()),
            Resolution::Run("nav.enter".into())
        );
        // En Viewer, su q específico PISA al global y enter NO existe.
        let viewer = Effective::build_for(&preset, &[], COMANDOS, Screen::Viewer).unwrap();
        let mut r = Resolver::new(viewer);
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("cursor.top".into())
        );
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
            Resolution::Run("cursor.down".into()),
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
            Resolution::Run("lua:pwn".into()),
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
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Run("cursor.up".into())
        );
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

    /// `build_for_subset`: a PRESET binding to a command this frontend does
    /// not implement is skipped (the GUI implements a subset of the TUI's
    /// commands); a LAYER binding to an unknown command is still an error
    /// (a user typo must never die silently — ADR 0006).
    #[test]
    fn build_for_subset_filtra_preset_pero_capa_sigue_estricta() {
        let preset = parse_keymap(
            "[pane]\nkeymap = [\n { on = [\"q\"], run = \"app.quit\" },\n { on = [\"f1\"], run = \"app.help\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for_subset(&preset, &[], &known, Screen::Browse)
            .expect("preset con extras construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('q'))),
            Resolution::Run("app.quit".into())
        );
        // El binding filtrado no existe: F1 no tiene ningún binding —
        // Miss, no Prefix ni Run.
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::F(1))),
            Resolution::Reset
        );
        let layer =
            parse_keymap("[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"app.help\" }]\n")
                .unwrap();
        assert!(
            Effective::build_for_subset(&preset, &[layer], &known, Screen::Browse).is_err(),
            "capa con comando desconocido: error, no filtrado"
        );
    }

    /// El nombre `lua:` sigue validándose por CHARSET aunque el modo sea
    /// Lenient — el filtrado de `build_for_subset` es solo por
    /// `known_commands` ausente; un `lua:` con nombre inválido (fuera de
    /// `[a-z0-9._-]{1,64}`) no tiene forma de colarse. El error sale como
    /// `UnknownCommand` (mismo camino que en modo estricto: el check de
    /// charset vive ANTES del filtrado lenient).
    #[test]
    fn subset_lua_invalido_sigue_siendo_error() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "lua:Bad Name" }]"#,
        )
        .unwrap();
        match Effective::build_for_subset(&preset, &[], &[], Screen::Browse) {
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

    /// Un binding de PRESET filtrado (comando desconocido para este
    /// frontend) no puede bloquear una secuencia más larga que lo tenía
    /// como prefijo — pin de la afirmación "prefix-freeness corre sobre el
    /// set YA filtrado". El binding largo viene de una CAPA (no del
    /// preset): en modo estricto, "g" (preset) + "g g" (capa) sería
    /// `AmbiguousPrefix`; en Lenient, "g" se filtra antes del check y "g g"
    /// resuelve limpio.
    #[test]
    fn subset_prefijo_filtrado_no_bloquea_secuencia() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["g"], run = "gui.unknown" }]"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"[pane]
append_keymap = [{ on = ["g", "g"], run = "known.cmd" }]"#,
        )
        .unwrap();
        let known = ["known.cmd"];
        let eff = Effective::build_for_subset(&preset, &[layer], &known, Screen::Browse)
            .expect("el prefijo filtrado no debe producir AmbiguousPrefix");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(1),
            "único binding activo en g: prefijo válido de g g"
        );
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Run("known.cmd".into())
        );
    }

    /// Divergencia DELIBERADA entre Strict y Lenient, pineada a propósito:
    /// el dedup por secuencia (`seen.insert`, "el primero gana") solo ve
    /// los bindings que SOBREVIVEN al filtro. Con un binding de `pane` y
    /// otro de `global` en el MISMO chord, un frontend que conoce ambos
    /// comandos (Strict) ve ganar `pane` por especificidad de contexto —
    /// pero un frontend que NO implementa el comando de `pane` (Lenient) lo
    /// filtra ANTES del dedup, y el binding de `global` queda "desenmascarado"
    /// (deja de estar sombreado) y pasa a ser el activo. Es el precio de
    /// que cada frontend valide contra SU PROPIO catálogo: la tecla hace
    /// algo distinto según qué frontend la interprete, por diseño (ADR
    /// 0006 — el motor no conoce comandos concretos).
    #[test]
    fn subset_dedup_desenmascara_binding_global() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "gui.unknown" }]
[global]
keymap = [{ on = ["x"], run = "app.quit" }]"#,
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for_subset(&preset, &[], &known, Screen::Browse)
            .expect("pane.x filtrado, global.x conocido");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('x'))),
            Resolution::Run("app.quit".into()),
            "con pane.x filtrado, global.x deja de estar sombreado"
        );
    }

    /// Un chord ilegible (`"megatecla"`) en un binding de PRESET cuyo
    /// comando TAMBIÉN es desconocido: el parseo de la secuencia corre
    /// ANTES del filtrado lenient (`raw.on.iter().map(parse_chord)`), así
    /// que `BadChord` gana incluso en modo Lenient — el filtro solo
    /// silencia comandos desconocidos, jamás config estructuralmente rota.
    #[test]
    fn subset_chord_malo_en_preset_sigue_fallando() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["megatecla"], run = "gui.unknown" }]"#,
        )
        .unwrap();
        match Effective::build_for_subset(&preset, &[], &[], Screen::Browse) {
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
            Resolution::Run("dialog.approve".into())
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
            Resolution::Run("dialog.deny".into())
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
}
