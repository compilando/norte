//! The embedded factory presets: sources, the name catalogue, and the lookup
//! each frontend uses for its "known preset" list.
//!
//! Bundled keymap presets (ADR 0006), shared by every frontend. Each
//! frontend declares ITS OWN command set to
//! [`Effective::build_for`](super::Effective::build_for); a preset binding to
//! a command that frontend lacks is KEPT and tagged
//! [`Availability::NotHere`](super::Availability), never dropped, so the key
//! can say why it does nothing (K1).

/// The default orthodox preset.
pub const ORTHODOX: &str = include_str!("../../presets/keymap/orthodox.toml");
/// Vim-style preset.
pub const VIM: &str = include_str!("../../presets/keymap/vim.toml");
/// CUA preset.
pub const CUA: &str = include_str!("../../presets/keymap/cua.toml");
/// Total Commander-style preset (K2b).
pub const TOTAL_COMMANDER: &str = include_str!("../../presets/keymap/total-commander.toml");
/// Krusader-style preset (K2b).
pub const KRUSADER: &str = include_str!("../../presets/keymap/krusader.toml");
/// Far Manager-style preset (K2b). No numeric prefix in the original — Far
/// spends `Ctrl+1..Ctrl+0` on panel view modes — so, like every preset in
/// this module, it never sets `counts`.
pub const FAR: &str = include_str!("../../presets/keymap/far.toml");
/// Norton Commander-style preset (K2b). No first-hand source (`NC.HLP` is
/// internally compressed); the file transcribes the uncontroversial core —
/// see this file's own header comment.
pub const NORTON: &str = include_str!("../../presets/keymap/norton.toml");

/// Names of every embedded preset (final review MINOR 4: this catalog
/// used to be mirrored as a hardcoded `&[&str]` in each frontend that
/// needs a "known preset" list or an "available presets" banner message
/// — TUI, GUI — with no single source of truth. `source()` and `NAMES`
/// are now tested against each other below, so a preset added to one and
/// not the other fails CI instead of drifting silently.
pub const NAMES: &[&str] = &[
    "orthodox",
    "vim",
    "cua",
    "total-commander",
    "krusader",
    "far",
    "norton",
];

/// Preset source by name; `None` if unknown (caller falls back +
/// reports, same contract the TUI had).
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "orthodox" => Some(ORTHODOX),
        "vim" => Some(VIM),
        "cua" => Some(CUA),
        "total-commander" => Some(TOTAL_COMMANDER),
        "krusader" => Some(KRUSADER),
        "far" => Some(FAR),
        "norton" => Some(NORTON),
        _ => None,
    }
}

#[cfg(test)]
mod presets_catalog_tests {
    use super::{NAMES, source};

    /// `NAMES` es el catálogo — cada entrada debe resolver una fuente real
    /// (final review MINOR 4: sin esto, `NAMES` podría desincronizarse de
    /// `source()` sin que ningún test lo note).
    #[test]
    fn cada_nombre_de_names_resuelve_una_fuente() {
        for name in NAMES {
            assert!(
                source(name).is_some(),
                "NAMES declara {name:?} pero source({name:?}) es None"
            );
        }
    }

    /// Ancla el tamaño del catálogo: un preset nuevo debe tocar este test a
    /// propósito (y con él, el resto de frontends que consumen `NAMES`).
    /// Renombrado en K2b Task 2 (era `names_tiene_los_tres_presets_de_fabrica`):
    /// ya no son tres, y el nombre viejo mentiría sobre el tamaño real.
    #[test]
    fn names_tiene_los_presets_de_fabrica() {
        assert_eq!(NAMES.len(), 7);
    }

    /// Un preset embebido que no parsea no es un fallo ruidoso: los
    /// consumidores lo ignoran en silencio. `preset_commands` hace
    /// `let Ok(kf) = parse_keymap(src) else { continue }`, así que un typo en
    /// —por ejemplo— el `dialog_from` de un preset de K2b encogería el
    /// vocabulario que `norte doctor` y `ntc keys` tratan por conocido, y el
    /// síntoma serían avisos de «comando desconocido» en otro sitio. Que
    /// falle aquí, con el nombre del preset y el diagnóstico.
    #[test]
    fn todos_los_presets_de_fabrica_parsean() {
        for name in NAMES {
            let src = source(name).expect("NAMES resuelve");
            crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
        }
    }
}

/// K2b Task 4 — the gate that keeps every bundled preset honest, driven by
/// [`NAMES`] so a future eighth preset inherits every check for free.
///
/// The plan lists seven checks; two of them do NOT live here:
///
/// - **Check 1** (builds for `Screen::{Browse, Viewer, Dialog}` against a
///   FRONTEND's command set) needs a frontend's actual `COMMANDS` list, which
///   this crate cannot see — `norte-frontend` is upstream of every frontend,
///   not the other way round. Its TUI half is
///   `norte_tui::keymap::tests::todos_los_presets_construyen_las_tres_pantallas_del_tui`;
///   its graphical half died with the GPUI frontend (ADR 0065) and is owed by
///   whatever replaces it.
/// - **Check 6** ("only `vim` sets `counts`") already has a pinned test that
///   iterates `NAMES`/`source` exactly this way:
///   [`super::tests::solo_vim_trae_los_contadores_encendidos`] in this
///   module's parent (`keymap/mod.rs`), predating this task. Duplicating it
///   here would just be two tests that can drift from each other.
///
/// The other five (2–5, 7) are independent of any frontend's vocabulary —
/// they ask about the preset's OWN data (its bindings, its `[dialog]`
/// section, its header) — so they belong next to [`NAMES`], not downstream.
#[cfg(test)]
mod k2b_gate_tests {
    use super::{NAMES, source};
    use crate::keymap::{CATALOGUE, Effective, Screen, Status, parse_chord};

    /// Every `Live` command in the shared catalogue — an "omniscient
    /// frontend" that implements everything norte has actually built, so a
    /// `Live` binding always resolves and only `Planned` stays
    /// `NotBuilt`. Checks 2 and 3 need bindings to actually RUN (not just
    /// survive marked `NotHere`), and neither check is about a specific
    /// frontend's subset — that is check 1's job.
    fn live_commands() -> Vec<&'static str> {
        CATALOGUE
            .iter()
            .filter(|d| matches!(d.status, Status::Live))
            .map(|d| d.name)
            .collect()
    }

    /// Parses `name` and builds its effective keymap for `screen` against
    /// [`live_commands`]. Panics name the preset AND the screen — a bare
    /// `.unwrap()` on a `for` loop over seven presets would only say
    /// `SacredKey { .. }` and leave the reader grepping seven files.
    fn build(name: &str, screen: Screen, known: &[&str]) -> Effective {
        let src = source(name).expect("NAMES resuelve");
        let kf = crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
        Effective::build_for(&kf, &[], known, screen)
            .unwrap_or_else(|e| panic!("preset {name} en {screen:?}: {e}"))
    }

    /// Check 2: `tab` resolves to `pane.switch` in every bundled preset.
    ///
    /// "…and to nothing else" is NOT re-asserted here: [`Effective::build_for`]
    /// already enforces it structurally, at load, via `check_sacred` (ADR
    /// 0044) — a preset that bound `tab` to anything but `pane.switch` alone
    /// would have failed inside [`build`] above with `SacredKey`, before this
    /// test's assertion ever runs. What `check_sacred` does NOT guarantee is
    /// the positive half: it forbids the wrong binding, but nothing requires
    /// the right one to exist at all, and four newly-imported foreign layouts
    /// are the first real chance to get that half wrong (plan rule 2).
    #[test]
    fn tab_resuelve_a_pane_switch_en_todos_los_presets() {
        let known = live_commands();
        let tab = parse_chord("tab").expect("tab parsea");
        for name in NAMES {
            let eff = build(name, Screen::Browse, &known);
            assert!(
                eff.single_chord_runs(tab, "pane.switch"),
                "preset {name}: tab no resuelve a pane.switch"
            );
        }
    }

    /// Check 3: `viewer.close` and the six movers (rule 7 — "F3 is never a
    /// room with no door") are bound and RUNNABLE in `[viewer]`, for every
    /// bundled preset.
    const VIEWER_MOVERS: [&str; 6] = [
        "viewer.up",
        "viewer.down",
        "viewer.page-up",
        "viewer.page-down",
        "viewer.top",
        "viewer.bottom",
    ];

    #[test]
    fn viewer_close_y_los_seis_movers_estan_ligados_en_todos_los_presets() {
        let known = live_commands();
        for name in NAMES {
            let eff = build(name, Screen::Viewer, &known);
            let bound: Vec<&str> = eff.bindings().iter().map(|(_, cmd)| *cmd).collect();
            assert!(
                bound.contains(&"viewer.close"),
                "preset {name}: sin viewer.close en [viewer]"
            );
            for mover in VIEWER_MOVERS {
                assert!(
                    bound.contains(&mover),
                    "preset {name}: sin {mover} en [viewer]"
                );
            }
        }
    }

    /// Check 4: after `dialog_from` resolution (Task 1), every preset's
    /// `[dialog]` section is non-empty — the inheritance actually landed,
    /// not just parsed without error.
    ///
    /// Reads the parsed [`crate::keymap::KeymapFile`] directly rather than
    /// building an [`Effective`]: `Screen::Dialog` merges `[dialog]` with
    /// `[global]`, so a non-empty EFFECTIVE dialog screen would pass even if
    /// `[dialog]` itself came back empty (an `orthodox`-inherited `y` bound
    /// under `[global]` would never happen, but the merge makes that
    /// coincidence, not a property this test would actually be checking).
    /// `.keymap` alone (not `prepend_keymap`/`append_keymap`) is enough: a
    /// preset — the only kind of file that ever reaches `dialog_from` — is
    /// refused those two lists by `check_layer_keys`.
    #[test]
    fn dialog_no_vacio_tras_resolver_dialog_from_en_todos_los_presets() {
        for name in NAMES {
            let src = source(name).expect("NAMES resuelve");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            assert!(
                !kf.dialog.keymap.is_empty(),
                "preset {name}: [dialog] vacío tras resolver dialog_from"
            );
        }
    }

    /// The `run` names of every binding a preset's OWN `keymap` lists declare
    /// (`[global]`/`[pane]`/`[viewer]`/`[dialog]` — presets never use
    /// `prepend_keymap`/`append_keymap`, `check_layer_keys` refuses that).
    /// Field access only: `RawSection`/`RawBinding` are `pub(super)` to
    /// `layer.rs`'s parent (`keymap`), and this module is inside that same
    /// tree, so the fields are visible without naming either type.
    fn preset_runs(kf: &crate::keymap::KeymapFile) -> Vec<&str> {
        [&kf.global, &kf.pane, &kf.viewer, &kf.dialog]
            .into_iter()
            .flat_map(|section| section.keymap.iter())
            .map(|b| b.run.as_str())
            .collect()
    }

    /// `pane.sync-dirs` está ligado en TODO preset que ligue su gemelo
    /// `pane.compare-dirs`, y a `ctrl+y` en los cinco que lo ligan — cuatro
    /// tomándolo del chord de Krusader, que es el único gestor de referencia
    /// que le da uno.
    ///
    /// La lista de los que NO lo ligan se escribe A MANO, y ésa es la
    /// decisión: `far` y `norton` dejan los dos sin ligar por la regla de
    /// fidelidad que sus propios ficheros enuncian —Far tiene el
    /// sincronizador en un plugin y NC no lo tenía— y un test que exigiera
    /// «todos los presets» desharía esos dos comentarios sin discutirlos.
    /// Escrita aquí, quitar una fidelidad cuesta editar este test.
    #[test]
    fn sincronizar_esta_ligado_dondequiera_que_lo_este_comparar() {
        const SIN_LIGAR: [&str; 2] = ["far", "norton"];
        for name in NAMES {
            let src = source(name).expect("NAMES resuelve");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            let runs = preset_runs(&kf);
            let compara = runs.contains(&"pane.compare-dirs");
            let sincroniza = runs.contains(&"pane.sync-dirs");
            if SIN_LIGAR.contains(name) {
                assert!(
                    !compara && !sincroniza,
                    "preset {name}: ya no es de los que no ligan la familia — quita el nombre de SIN_LIGAR"
                );
                continue;
            }
            assert!(compara, "preset {name}: sin pane.compare-dirs");
            assert!(
                sincroniza,
                "preset {name}: liga comparar y NO sincronizar — la mitad que escribe se quedó sin tecla"
            );
        }
    }

    /// La tecla por defecto de `pane.sync-dirs` no es una de función con
    /// modificador.
    ///
    /// #159: bajo tmux NINGUNA llega — ni `Shift+F2` ni `Alt+F7` —, así que
    /// una tecla así sería un atajo documentado y muerto, y este repo ya envió
    /// uno. Se afirma sobre el chord CONCRETO en vez de sobre una propiedad
    /// del `Chord`, porque lo que hay que impedir es que alguien lo mueva a
    /// una tecla de función «porque queda simétrico con Shift+F2».
    #[test]
    fn la_tecla_de_sincronizar_no_es_de_funcion_con_modificador() {
        let known = live_commands();
        let ctrl_y = parse_chord("ctrl+y").expect("ctrl+y parsea");
        for name in ["orthodox", "cua", "vim", "total-commander", "krusader"] {
            let eff = build(name, Screen::Browse, &known);
            assert!(
                eff.single_chord_runs(ctrl_y, "pane.sync-dirs"),
                "preset {name}: ctrl+y no resuelve a pane.sync-dirs"
            );
        }
    }

    /// Check 5: every `run` name a preset binds is in the shared catalogue
    /// (Live or Planned) — the same rule [`Effective::build_for`]'s
    /// `UnknownCommand` already enforces, asserted directly against the raw
    /// data instead of through a build, so a failure names the preset AND
    /// the offending command instead of surfacing as one of several possible
    /// `KeymapError` variants from an unrelated screen/known-list
    /// combination.
    #[test]
    fn todo_run_de_cada_preset_esta_en_el_catalogo_compartido() {
        for name in NAMES {
            let src = source(name).expect("NAMES resuelve");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for run in preset_runs(&kf) {
                let known = run
                    .strip_prefix("lua:")
                    .is_some_and(crate::keymap::valid_lua_name)
                    || crate::keymap::catalogue::lookup(run).is_some();
                assert!(known, "preset {name}: comando desconocido {run:?}");
            }
        }
    }

    /// Las superficies PROPIAS de norte están atadas en los SIETE presets.
    ///
    /// Son las que ningún gestor de referencia tenía, así que no hay nada que
    /// transcribir y hay que elegirles tecla a mano — y por eso se olvidan.
    /// `app.theme` se quedó sin atar en los cuatro presets importados: el
    /// ÚNICO de la familia que se cayó, y en los cuatro a la vez. Al tema solo
    /// se llegaba por menú o paleta.
    ///
    /// Es la forma de #228 —presets que dejan comandos del núcleo sin tecla—
    /// aplicada a la familia entera en vez de a un comando suelto.
    #[test]
    fn las_superficies_propias_estan_atadas_en_los_siete_presets() {
        let propias = [
            "app.theme",
            "app.settings",
            "app.extensions",
            "app.palette",
            "app.menu",
        ];
        let mut faltan: Vec<String> = Vec::new();
        for nombre in NAMES {
            let src = source(nombre).expect("NAMES resuelve");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {nombre}: {e}"));
            let atados = preset_runs(&kf);
            for cmd in propias {
                if !atados.contains(&cmd) {
                    faltan.push(format!("{nombre}: {cmd}"));
                }
            }
        }
        assert!(
            faltan.is_empty(),
            "superficies de norte sin tecla — solo se llega a ellas por menú o \
             paleta:\n  {}",
            faltan.join("\n  ")
        );
    }

    /// Todo panel que pueda quedarse el TECLADO se abre con una tecla, y la
    /// pantalla se recorre entera con otra.
    ///
    /// La misma forma que el test de arriba, sobre la otra familia que ningún
    /// gestor de referencia tenía: los paneles laterales. `layout.processes`
    /// estaba sin atar en los siete —al único sitio que dice qué está
    /// copiando norte se llegaba solo por el menú— y `layout.focus-next`
    /// también, así que con el sidebar y el visor delante había que acordarse
    /// de la tecla de cada panel para moverse entre ellos: `tab` alterna los
    /// dos listados y nada más.
    ///
    /// `layout.focus-prev` NO está en la lista: en krusader su acorde es el
    /// de «Sync panels» y se queda sin atar a propósito (ver el fichero). El
    /// anillo da la vuelta, así que hacia delante se llega igual.
    #[test]
    fn los_paneles_con_teclado_se_abren_y_se_recorren_en_los_siete_presets() {
        let paneles = [
            "layout.places",
            "layout.preview",
            "layout.processes",
            "layout.focus-next",
        ];
        let mut faltan: Vec<String> = Vec::new();
        for nombre in NAMES {
            let src = source(nombre).expect("NAMES resuelve");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {nombre}: {e}"));
            let atados = preset_runs(&kf);
            for cmd in paneles {
                if !atados.contains(&cmd) {
                    faltan.push(format!("{nombre}: {cmd}"));
                }
            }
        }
        assert!(
            faltan.is_empty(),
            "paneles sin tecla — o no se abren, o no se puede salir de ellos \
             sin ratón:\n  {}",
            faltan.join("\n  ")
        );
    }

    /// Ningún preset ata un acorde que un terminal NO PUEDE entregar.
    ///
    /// `ctrl+<letra mayúscula>` es esa forma. [`crate::keymap::parse_chord`]
    /// la guarda como `Char('P')`, y un terminal manda para Ctrl+P y para
    /// Ctrl+Shift+P el MISMO byte (0x10), que llega como `Char('p')` en
    /// minúscula. Distinguirlos exige el protocolo de teclado de Kitty, y el
    /// adaptador de la TUI (`norte_tui::keymap::chord_from_crossterm`)
    /// documenta que norte NO lo activa —la misma razón por la que `mod+` es
    /// Ctrl en todas las plataformas—. Así que el binding existe, el catálogo
    /// lo anuncia, la ayuda lo imprime, y la tecla no hace NADA.
    ///
    /// Salió de la paleta de `krusader`: `ctrl+P` para abrirla, con un
    /// comentario explicando que la mayúscula ES el shift. Lo es en la
    /// gramática de acordes; no lo es en el cable. Y como `ctrl+p` en
    /// minúscula sí está atado ahí a `pane.split-file`, quien buscaba la
    /// paleta se encontraba un diálogo de partir ficheros.
    ///
    /// El test vive AQUÍ y no en la TUI porque los presets son de este crate,
    /// y la regla es sobre lo que un preset puede prometer.
    #[test]
    fn ningun_preset_ata_un_acorde_que_el_terminal_no_entrega() {
        let mut muertos: Vec<String> = Vec::new();
        for nombre in NAMES {
            let src = source(nombre).expect("NAMES resuelve");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {nombre}: {e}"));
            for b in [&kf.global, &kf.pane, &kf.viewer, &kf.dialog]
                .into_iter()
                .flat_map(|s| s.keymap.iter())
            {
                for txt in &b.on {
                    if crate::keymap::parse_chord(txt).is_err() {
                        continue; // lo que no parsea ya lo caza otro check
                    }
                    // Sobre el TEXTO del binding y no sobre el `Chord`: es
                    // donde vive la regla —lo que un preset escribió— y el
                    // tipo no expone sus campos fuera de su módulo.
                    let mut partes: Vec<&str> = txt.split('+').collect();
                    let Some(tecla) = partes.pop() else { continue };
                    let con_ctrl = partes
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case("ctrl") || m.eq_ignore_ascii_case("mod"));
                    let letra_sola = tecla.chars().count() == 1
                        && tecla.chars().next().is_some_and(|c| c.is_ascii_uppercase());
                    if con_ctrl && letra_sola {
                        muertos.push(format!("{nombre}: «{txt}» → {}", b.run));
                    }
                }
            }
        }
        assert!(
            muertos.is_empty(),
            "acordes que ningún terminal entrega sin el protocolo de Kitty, \
             que norte no activa:\n  {}",
            muertos.join("\n  ")
        );
    }

    /// Los cuatro comandos de perfil existen en el catálogo compartido y
    /// NINGÚN preset los ata.
    ///
    /// #228 fue el hueco contrario —presets que dejaban comandos del núcleo
    /// sin ninguna tecla— y la lección de aquello no es «ata todo»: atar
    /// cuatro teclas nuevas en siete presets sin que nadie lo haya pedido
    /// decide por el lector qué tecla es un perfil, encima de teclas que en
    /// su gestor de toda la vida significan otra cosa. Se llega por la
    /// paleta y por el menú, y quien quiera un acorde se lo ata él.
    #[test]
    fn los_comandos_de_perfil_existen_y_no_los_ata_ningun_preset() {
        let perfil = [
            "profile.pick",
            "profile.next",
            "profile.prev",
            "profile.save-as",
        ];
        for cmd in perfil {
            assert!(
                crate::keymap::catalogue::lookup(cmd).is_some(),
                "{cmd} no está en el catálogo compartido"
            );
        }
        for name in NAMES {
            let src = source(name).expect("NAMES resuelve");
            let kf =
                crate::keymap::parse_keymap(src).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            for run in preset_runs(&kf) {
                assert!(
                    !perfil.contains(&run),
                    "preset {name} ata {run:?}: los perfiles llegan sin acorde"
                );
            }
        }
    }

    /// Check 7, scoped to the FOUR imported presets (spec "K2b — the four
    /// presets": "Each file records the program, its version, the source,
    /// and the date it was transcribed, so that when it ages the staleness
    /// is dated rather than unknown" — plan rule 8's mandatory header
    /// block). `orthodox`/`vim`/`cua` predate this convention and are not
    /// transcriptions of one external document with a version to record —
    /// there is no `<Program> <version>` to name for "mc clásico" or
    /// "vim-like" the way there is for "Total Commander 11.58" — so the same
    /// header shape does not fit them, and asserting it there would either
    /// fail on files this task did not touch or force a fabricated
    /// "transcribed from" that names no real source. Same rescoping the plan
    /// already did the other direction (K2b Task 2's `pane_gesture_chords_*`
    /// and friends, scoped to the three NATIVE presets and explaining why in
    /// their own doc comments).
    const TRANSCRIBED_IMPORTS: [&str; 4] = ["total-commander", "krusader", "far", "norton"];

    /// `true` if `s` contains a substring shaped like `YYYY-MM-DD`
    /// (ASCII digits and hyphens only — no calendar validation, this is a
    /// staleness DATE STAMP, not a parser). Manual scan instead of a `regex`
    /// dependency for one ten-byte pattern checked over four short strings.
    fn has_iso_date(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() >= 10
            && (0..=b.len() - 10).any(|i| {
                let w = &b[i..i + 10];
                w[..4].iter().all(u8::is_ascii_digit)
                    && w[4] == b'-'
                    && w[5..7].iter().all(u8::is_ascii_digit)
                    && w[7] == b'-'
                    && w[8..10].iter().all(u8::is_ascii_digit)
            })
    }

    #[test]
    fn los_presets_importados_fechan_su_transcripcion_en_la_primera_linea() {
        for name in TRANSCRIBED_IMPORTS {
            let src = source(name).expect("NAMES resuelve");
            let first = src.lines().next().unwrap_or_default();
            assert!(
                first.starts_with('#'),
                "preset {name}: la primera línea no es un comentario: {first:?}"
            );
            assert!(
                first.contains("transcribed"),
                "preset {name}: la primera línea no nombra una transcripción: {first:?}"
            );
            assert!(
                has_iso_date(first),
                "preset {name}: la primera línea no trae una fecha ISO: {first:?}"
            );
        }
    }
}
