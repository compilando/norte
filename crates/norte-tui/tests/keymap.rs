//! Tests de integración del keymap de la TUI (ADR 0006): parseo, fusión
//! Yazi, prefix-free al cargar y resolución determinista por trie. Ejercen
//! el motor COMPARTIDO de [`norte_frontend::keymap`] A TRAVÉS del re-export
//! de `norte_tui::keymap` (GUI-c T2) — no una copia local: si la extracción
//! rompiera algo, estos tests lo cazarían igual que antes.

use norte_tui::keymap::{COMMANDS as COMANDOS, KeyCode};
use norte_tui::keymap::{
    Chord, Count, Effective, KeymapError, Mods, Resolution, Resolver, parse_chord, parse_keymap,
};

fn eff(preset: &str, user: Option<&str>) -> Result<Effective, KeymapError> {
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

/// El chord canónico ([`Chord::new`], re-exportado desde
/// `norte_frontend::keymap`) descarta `shift` en teclas `Char` — el
/// carácter YA lo codifica (`'G'` vs `'g'`) — pero lo conserva en el resto
/// (p. ej. `shift+f5`). Este era el comportamiento de `Chord::from_event`
/// (crossterm) antes de GUI-c T2; ahora vive en el propio `Chord::new`
/// neutro y el adaptador `chord_from_crossterm` lo hereda gratis.
#[test]
fn chord_new_normaliza_shift_en_chars_pero_no_en_otras_teclas() {
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
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        }
    );
    // Tras ejecutar, el estado queda limpio.
    assert_eq!(
        r.push(parse_chord("G").unwrap()),
        Resolution::Run {
            command: "cursor.bottom".into(),
            count: Count::None
        }
    );
    // Tecla sin binding: reset silencioso.
    assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
    // Prefijo pendiente + tecla que no continúa: reset (no ejecuta nada).
    r.push(parse_chord("g").unwrap());
    assert_eq!(r.push(parse_chord("q").unwrap()), Resolution::Reset);
    // q suelto (contexto global) sí corre.
    assert_eq!(
        r.push(parse_chord("q").unwrap()),
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        }
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
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        }
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
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        },
        "pane.append gana a global.keymap (especificidad > capa)"
    );
    // …y un prepend de usuario en [global] NO pisa al keymap [pane].
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run {
            command: "cursor.down".into(),
            count: Count::None
        },
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

/// M4 Lua (T8): un binding a `lua:<nombre>` pasa la validación aunque el
/// nombre no esté en COMMANDS — el registro Lua es dinámico (runtime); un
/// comando lua no registrado NO es error de keymap (al invocar, la barra
/// avisa con `err-lua-unknown`). El NOMBRE sí se valida con el mismo charset
/// que `norte.command` (`[a-z0-9._-]{1,64}`): un binding a un nombre que
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
        Resolution::Run {
            command: "lua:mi-comando.v2".into(),
            count: Count::None
        },
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
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        },
        "prepend PISA al preset"
    );
    assert_eq!(
        r.push(parse_chord("k").unwrap()),
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        },
        "append NO pisa una secuencia existente"
    );
    assert_eq!(
        r.push(parse_chord("x").unwrap()),
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        },
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
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        }
    );
    assert_eq!(
        r.push(parse_chord("tab").unwrap()),
        Resolution::Run {
            command: "pane.switch".into(),
            count: Count::None
        }
    );
}

#[test]
fn los_tres_presets_de_fabrica_cargan_y_cubren_lo_basico() {
    for (nombre, preset) in norte_tui::keymap::presets() {
        let eff = Effective::build(&preset, None, COMANDOS)
            .unwrap_or_else(|e| panic!("preset {nombre}: {e:?}"));
        let mut r = Resolver::new(eff.clone());
        // Todo preset debe poder salir y cambiar de pane. `alt+f4` es el
        // quit real de Total Commander (K2b): su F10 significa "activar/
        // dejar el menú", no salir, así que NO lo comparte con q/f10/ctrl+q.
        let quit_posible = ["q", "f10", "ctrl+q", "alt+f4"].iter().any(|k| {
            let res = r.push(parse_chord(k).unwrap());
            res == Resolution::Run {
                command: "app.quit".into(),
                count: Count::None,
            }
        });
        assert!(quit_posible, "preset {nombre}: sin salida");
        assert_eq!(
            r.push(parse_chord("tab").unwrap()),
            Resolution::Run {
                command: "pane.switch".into(),
                count: Count::None
            },
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
        ] {
            assert_eq!(
                r.push(parse_chord(tecla).unwrap()),
                Resolution::Run {
                    command: cmd.into(),
                    count: Count::None
                },
                "preset {nombre}: {tecla}"
            );
        }
    }
    // `ctrl+k` → `task.cancel` es la convención PROPIA de norte (orthodox/
    // vim/cua): ni Total Commander ni Krusader documentan una tecla de
    // cancelar-tarea en sus fuentes (K2b rule 1), así que no se inventa una
    // ahí — este segundo bucle se queda en los tres presets nativos.
    for (nombre, preset) in norte_tui::keymap::presets()
        .into_iter()
        .filter(|(n, _)| matches!(*n, "orthodox" | "vim" | "cua"))
    {
        let eff = Effective::build(&preset, None, COMANDOS)
            .unwrap_or_else(|e| panic!("preset {nombre}: {e:?}"));
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("ctrl+k").unwrap()),
            Resolution::Run {
                command: "task.cancel".into(),
                count: Count::None
            },
            "preset {nombre}: ctrl+k"
        );
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
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        },
        "el prepend de la capa MÁS alta gana"
    );
    assert_eq!(
        r.push(parse_chord("x").unwrap()),
        Resolution::Run {
            command: "cursor.bottom".into(),
            count: Count::None
        },
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
        Resolution::Run {
            command: "app.quit".into(),
            count: Count::None
        }
    );
    assert_eq!(
        r.push(parse_chord("enter").unwrap()),
        Resolution::Run {
            command: "nav.enter".into(),
            count: Count::None
        }
    );
    // En Viewer, su q específico PISA al global y enter NO existe.
    let viewer = Effective::build_for(&preset, &[], COMANDOS, Screen::Viewer).unwrap();
    let mut r = Resolver::new(viewer);
    assert_eq!(
        r.push(parse_chord("q").unwrap()),
        Resolution::Run {
            command: "cursor.top".into(),
            count: Count::None
        }
    );
    assert_eq!(r.push(parse_chord("enter").unwrap()), Resolution::Reset);
}

/// Extensibilidad de la ayuda: TODO comando tiene descripción en AMBOS
/// locales — un comando nuevo sin entrada help-cmd-* rompe aquí. La
/// decoración de la pantalla (título, secciones) también.
///
/// `help-hint` YA NO está: el pie del overlay se GENERA del keymap efectivo
/// (`hints::DialogHints::help`) desde H3b, y la cadena estática que quedaba
/// anunciaba una tecla (`q`) que el enrutado por keymap ya no acepta.
#[test]
fn todo_comando_tiene_ayuda_traducida() {
    use norte_tui::keymap::help_id;
    let ids_decoracion = [
        "help-title".to_owned(),
        // H3b: la etiqueta de la entrada sintética `keys` de la lateral. Se
        // resuelve en `HelpView::new` y viaja al modelo como TÍTULO de fila:
        // sin entrada Fluent, la lateral pintaría `help-topic-keys` literal.
        "help-topic-keys".to_owned(),
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

/// H3b: TODA cabecera de grupo de la lateral de la ayuda tiene entrada Fluent
/// en AMBOS locales.
///
/// `draw_help` pinta `t(&format!("help-group-{tag}"))` con el tag que viene
/// del FRONT MATTER del corpus, y `norte_i18n::t` contesta un fallo de
/// búsqueda con el id: un tema archivado bajo un tag nuevo pintaría una banda
/// `help-group-advanced` literal en la lateral. Es el mismo defecto que esta
/// misma fase arregló un fichero más allá (el `label_id` de `help.rs`, fijado
/// por `the_cheatsheet_never_paints_a_fluent_id`), y la búsqueda gemela no
/// tenía guardia.
///
/// La suite de paridad de i18n NO lo cubre: solo afirma que EN y ES tienen el
/// MISMO conjunto de ids, así que un tag ausente en los dos pasa de largo.
///
/// Los tags se sacan del MODELO (`HelpState::rows`), no de una lista escrita
/// a mano: son exactamente las filas `Group` que el pintor recorre. H3h
/// escribe el corpus completo y este barrido crece con él sin tocarlo.
///
/// Las que el pintor NO pinta quedan fuera, y quién es quién lo dice él
/// (`ui::help_group_is_painted`), no una copia de la regla: la cabecera del
/// grupo sintético `keys` se suprime —es un grupo de uno cuya cabecera se
/// llamaría igual que su única fila— así que exigirle entrada Fluent sería
/// exigir una cadena que nadie busca. Si alguna vez un tema del corpus se
/// archiva bajo ese tag, su cabecera SÍ se pinta y este barrido vuelve a
/// pedirla.
#[test]
fn toda_cabecera_de_grupo_de_la_ayuda_tiene_etiqueta_traducida() {
    use norte_frontend::help::{HelpState, SidebarRow};

    let mut vistos = 0usize;
    for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
        // La etiqueta de `keys` no se está probando aquí (la cubre
        // `ids_decoracion`); da igual cuál sea mientras no esté vacía.
        let state = HelpState::new(lang, "Teclado".to_owned());
        for (i, row) in state.rows().iter().enumerate() {
            let SidebarRow::Group { tag } = row else {
                continue;
            };
            if !norte_tui::ui::help_group_is_painted(state.rows(), i) {
                continue;
            }
            vistos += 1;
            let id = format!("help-group-{tag}");
            assert_ne!(
                norte_i18n::t_in(lang, &id),
                id,
                "{id}: sin traducción en {lang:?} — la lateral pintaría el id \
                 Fluent como si fuera el nombre del grupo. Añade la entrada en \
                 i18n/{}.ftl",
                match lang {
                    norte_i18n::Lang::Es => "es",
                    norte_i18n::Lang::En => "en",
                }
            );
        }
    }
    // Anti-vacuidad: sin filas `Group` el bucle no afirma nada. Hoy hay 3
    // grupos PINTADOS por locale (`basics`, `doing`, `remote`; la cabecera de
    // la sintética `keys` no se pinta).
    assert!(
        vistos >= 6,
        "el barrido no vio cabeceras de grupo suficientes ({vistos}): el modelo \
         dejó de agrupar y este test pasaría en vacío"
    );
}

/// H1 T3 (#24): TODO comando de [`DIALOG_COMMANDS`] tiene etiqueta CORTA en
/// AMBOS locales — la fuente de los hints generados de los overlays
/// (`dialog_hints`/`DialogHints`). Mismo mangling que `help_id` (puntos→
/// guiones) pero para el sufijo tras `dialog.`, vía `dialog_hint_id`: un
/// comando nuevo en `DIALOG_COMMANDS` sin su `dialog-cmd-*` rompe aquí,
/// jamás en silencio en el footer.
#[test]
fn todo_dialog_command_tiene_etiqueta_traducida() {
    use norte_tui::keymap::{DIALOG_COMMANDS, dialog_hint_id};
    for cmd in DIALOG_COMMANDS {
        let id = dialog_hint_id(cmd);
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

/// ALTA (security review M4 Lua): `./.norte/keymap.toml` carga SIN trust,
/// así que un repo hostil podría rebindear una tecla común (`j`, `enter`) a
/// un comando `lua:` del init.lua de USUARIO (sin sandbox, sin confirmación,
/// con cwd = el repo hostil). Los bindings `lua:` originados en la capa de
/// PROYECTO se DESCARTAN (contados para el aviso de barra); los rebinds de
/// proyecto a builtins siguen funcionando; el mismo binding en una capa de
/// usuario SÍ resuelve.
#[test]
fn lua_de_keymap_de_proyecto_se_descarta_con_aviso() {
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
        Resolution::Run {
            command: "cursor.down".into(),
            count: Count::None
        },
        "la tecla cae al builtin, jamás al lua: del proyecto"
    );

    // El MISMO binding en capa de USUARIO (sin marcar): resuelve normal.
    let usuario = parse_keymap(capa).unwrap();
    let eff = Effective::build_layered(&preset, std::slice::from_ref(&usuario), COMANDOS).unwrap();
    assert_eq!(eff.discarded_lua_bindings(), 0);
    let mut r = Resolver::new(eff);
    assert_eq!(
        r.push(parse_chord("j").unwrap()),
        Resolution::Run {
            command: "lua:pwn".into(),
            count: Count::None
        },
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
    let eff = Effective::build_layered(&preset, std::slice::from_ref(&proyecto), COMANDOS).unwrap();
    assert_eq!(eff.discarded_lua_bindings(), 0);
    let mut r = Resolver::new(eff);
    assert_eq!(
        r.push(parse_chord("x").unwrap()),
        Resolution::Run {
            command: "cursor.up".into(),
            count: Count::None
        }
    );
}
