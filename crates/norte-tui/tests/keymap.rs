//! Tests del keymap engine (ADR 0006): parseo, fusión Yazi, prefix-free
//! al cargar y resolución determinista por trie.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_tui::keymap::{
    Chord, Effective, KeymapError, Resolution, Resolver, parse_chord, parse_keymap,
};

use norte_tui::keymap::COMMANDS as COMANDOS;

fn eff(preset: &str, user: Option<&str>) -> Result<Effective, KeymapError> {
    let preset = parse_keymap(preset)?;
    let user = user.map(parse_keymap).transpose()?;
    Effective::build(&preset, user.as_ref(), COMANDOS)
}

#[test]
fn parse_de_chords() {
    assert_eq!(
        parse_chord("f5").unwrap(),
        Chord::new(KeyModifiers::NONE, KeyCode::F(5))
    );
    assert_eq!(
        parse_chord("ctrl+c").unwrap(),
        Chord::new(KeyModifiers::CONTROL, KeyCode::Char('c'))
    );
    assert_eq!(
        parse_chord("alt+enter").unwrap(),
        Chord::new(KeyModifiers::ALT, KeyCode::Enter)
    );
    // Mayúscula: el char YA codifica shift.
    assert_eq!(
        parse_chord("G").unwrap(),
        Chord::new(KeyModifiers::NONE, KeyCode::Char('G'))
    );
    assert_eq!(
        parse_chord("shift+f5").unwrap(),
        Chord::new(KeyModifiers::SHIFT, KeyCode::F(5))
    );
    for s in ["", "ctrl+", "megatecla", "ctrl+ctrl+c", "f99"] {
        assert!(parse_chord(s).is_err(), "{s:?} debe fallar");
    }
}

#[test]
fn eventos_char_con_shift_se_normalizan() {
    // crossterm entrega Char('G') CON el modificador SHIFT: el chord
    // canónico lo descarta (el char ya lo codifica).
    let c = Chord::from_event(KeyModifiers::SHIFT, KeyCode::Char('G'));
    assert_eq!(c, Chord::new(KeyModifiers::NONE, KeyCode::Char('G')));
    // En teclas no-char, shift ES información.
    let f = Chord::from_event(KeyModifiers::SHIFT, KeyCode::F(5));
    assert_eq!(f, Chord::new(KeyModifiers::SHIFT, KeyCode::F(5)));
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
    // Un binding "shift+g" jamás matchearía (el evento canónico descarta
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
fn los_tres_presets_de_fabrica_cargan_y_cubren_lo_basico() {
    for (nombre, preset) in norte_tui::keymap::presets() {
        let eff = Effective::build(&preset, None, COMANDOS)
            .unwrap_or_else(|e| panic!("preset {nombre}: {e:?}"));
        let mut r = Resolver::new(eff.clone());
        // Todo preset debe poder salir y cambiar de pane.
        let quit_posible = ["q", "f10", "ctrl+q"].iter().any(|k| {
            let res = r.push(parse_chord(k).unwrap());
            res == Resolution::Run("app.quit".into())
        });
        assert!(quit_posible, "preset {nombre}: sin salida");
        assert_eq!(
            r.push(parse_chord("tab").unwrap()),
            Resolution::Run("pane.switch".into()),
            "preset {nombre}: Tab es sagrado (spec)"
        );
    }
}

#[test]
fn los_presets_ligan_las_operaciones_de_archivo() {
    for (nombre, preset) in norte_tui::keymap::presets() {
        let eff = Effective::build(&preset, None, COMANDOS)
            .unwrap_or_else(|e| panic!("preset {nombre}: {e:?}"));
        let mut r = Resolver::new(eff.clone());
        for (tecla, cmd) in [
            ("f5", "pane.copy"),
            ("f6", "pane.move"),
            ("f8", "pane.delete"),
            ("ctrl+k", "task.cancel"),
        ] {
            assert_eq!(
                r.push(parse_chord(tecla).unwrap()),
                Resolution::Run(cmd.into()),
                "preset {nombre}: {tecla}"
            );
        }
    }
}

#[test]
fn capas_multiples_se_pliegan_por_precedencia() {
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
    // Capas en precedencia ASCENDENTE: sistema, usuario.
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
    use norte_tui::keymap::Screen;
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

/// Extensibilidad de la ayuda: TODO comando tiene descripción en AMBOS
/// locales — un comando nuevo sin entrada help-cmd-* rompe aquí. La
/// decoración de la pantalla (título, hint, secciones) también.
#[test]
fn todo_comando_tiene_ayuda_traducida() {
    use norte_tui::keymap::help_id;
    let ids_decoracion = [
        "help-title".to_owned(),
        "help-hint".to_owned(),
        "help-section-browse".to_owned(),
        "help-section-viewer".to_owned(),
    ];
    let ids = COMANDOS.iter().map(|cmd| help_id(cmd));
    for id in ids.chain(ids_decoracion) {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let texto = norte_i18n::t_in(lang, &id);
            assert_ne!(texto, id, "{id}: sin traducción en {lang:?}");
        }
    }
}

/// La ayuda se construye del keymap EFECTIVO: los bindings expuestos
/// reflejan preset + capas EN ORDEN de precedencia, y un binding
/// sombreado aparece UNA vez con el comando que gana (lo que la tecla
/// hace de verdad, no lo que el preset dice).
#[test]
fn bindings_expuestos_reflejan_las_capas() {
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
