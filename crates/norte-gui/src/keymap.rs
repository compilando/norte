//! Keymap de la GUI (GUI-c): COMMANDS del contexto Browse, preset orthodox
//! embebido, carga de capas (sistema/usuario/proyecto) y el adaptador
//! nombre-de-tecla-GPUI → `Chord` neutro. El MOTOR es `norte_frontend::keymap`.

#[cfg(test)]
use norte_config::Layer;
use norte_config::Layers;
use norte_frontend::keymap::{
    Chord, Effective, KeyCode, KeymapError, KeymapFile, Mods, Screen, parse_keymap,
};
#[cfg(test)]
use std::path::PathBuf;

/// Comandos que la GUI sabe ejecutar (contexto Browse). Fuente ÚNICA de
/// validación del keymap; nombres alineados con la TUI donde coinciden.
pub const COMMANDS: &[&str] = &[
    "app.quit",
    "pane.switch",
    "cursor.up",
    "cursor.down",
    "cursor.top",
    "cursor.bottom",
    "cursor.page-up",
    "cursor.page-down",
    "nav.enter",
    "nav.parent",
    "mark.toggle",
    "pane.copy",
    "pane.move",
    "pane.delete",
    "task.cancel",
    "task.next",
    "task.prev",
    "task.dismiss",
    "pane.view",
];

/// Comandos del contexto Viewer (pantalla del visor F3).
pub const VIEWER_COMMANDS: &[&str] = &[
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
];

/// Preset por nombre, tomado del catálogo COMPARTIDO
/// `norte_frontend::keymap::presets` (decisión de diseño C2/G0: el preset
/// compartido es canónico — la GUI adopta sus chords para ir alineada con la
/// TUI; su viejo `keymap_presets/orthodox.toml` privado, con drift a nivel de
/// chord, queda retirado). Un nombre desconocido cae al ORTHODOX compartido
/// (mismo contrato que antes: fuente embebida, jamás I/O).
///
/// # Panics
/// Solo si el TOML embebido (constante en tiempo de compilación) fuera
/// inválido — lo cubre un test para los tres presets de fábrica.
fn preset(name: &str) -> KeymapFile {
    let src = norte_frontend::keymap::presets::source(name)
        .unwrap_or(norte_frontend::keymap::presets::ORTHODOX);
    parse_keymap(src).unwrap_or_else(|e| panic!("preset {name} embebido inválido: {e}"))
}

/// La unión de comandos de Browse + Viewer (para validar el keymap ENTERO —
/// `build_for` mezcla `global` con el contexto de la pantalla).
fn all_commands() -> Vec<&'static str> {
    COMMANDS.iter().chain(VIEWER_COMMANDS).copied().collect()
}

/// Build the two `Effective`s (Browse and Viewer) from `preset_name` +
/// layers.
///
/// Layer discovery now goes through the shared `norte_config::standard_layers`
/// (system → user → project, `NORTE_CONFIG_DIR` override included) instead of
/// the GUI's hand-rolled fork — see Task 9 of the M5 config-layer migration.
///
/// # Errors
/// The first `KeymapError` from any layer.
pub fn build_effectives(preset_name: &str) -> Result<(Effective, Effective), KeymapError> {
    build_effectives_layers(&norte_config::standard_layers(), preset_name)
}

/// Like [`build_effectives`] with an EXPLICIT user config dir (test
/// injection, kept for the existing tests): builds a minimal [`Layers`] of
/// (user, [`Layer::User`]) + (`./.norte`, [`Layer::Project`]).
///
/// `#[cfg(test)]`: since [`build_effectives`] now calls
/// `build_effectives_layers` directly (it no longer routes through this
/// seam), this function has no production caller — only the tests below use
/// it as an injection point. Gating it avoids a `dead_code` lint in this
/// binary crate (no lib target means `pub` alone doesn't count as reachable).
///
/// # Errors
/// The first `KeymapError` from any layer.
#[cfg(test)]
pub fn build_effectives_from(
    preset_name: &str,
    user_dir: Option<PathBuf>,
) -> Result<(Effective, Effective), KeymapError> {
    let mut dirs = Vec::new();
    if let Some(u) = user_dir {
        dirs.push((u, Layer::User));
    }
    dirs.push((PathBuf::from(".norte"), Layer::Project));
    build_effectives_layers(&Layers { dirs }, preset_name)
}

/// Shared implementation: loads each layer via
/// `norte_frontend::config::load_keymap_layer` (which also rejects a bare
/// `keymap = [...]` in a layer — ADR 0006/0035 — a check the old hand-rolled
/// GUI loader was missing) and merges them onto the shared preset via
/// [`Effective::build_for_subset`] — the GUI implements a SUBSET of the
/// commands the shared presets bind (e.g. `app.help`, `pane.hotlist`), so
/// preset bindings to commands the GUI lacks are skipped instead of failing
/// the whole load (design decision C2/G0: shared preset is canonical).
///
/// # Errors
/// The first `KeymapError` from any layer.
fn build_effectives_layers(
    layers: &Layers,
    preset_name: &str,
) -> Result<(Effective, Effective), KeymapError> {
    let preset = preset(preset_name);
    let mut kfs: Vec<KeymapFile> = Vec::new();
    // La GUI no expone diagnósticos de "qué ficheros se cargaron" (a
    // diferencia de la TUI): las fuentes se descartan tras el préstamo.
    let mut sources = Vec::new();
    for (dir, kind) in &layers.dirs {
        if let Some(kf) = norte_frontend::config::load_keymap_layer(dir, *kind, &mut sources)
            .map_err(|e| KeymapError::Toml(e.to_string()))?
        {
            kfs.push(kf);
        } // ausente: la capa no aporta; ilegible = error (banner + preset).
    }
    let cmds = all_commands();
    let browse = Effective::build_for_subset(&preset, &kfs, &cmds, Screen::Browse)?;
    let viewer = Effective::build_for_subset(&preset, &kfs, &cmds, Screen::Viewer)?;
    Ok((browse, viewer))
}

/// Fallback: los dos `Effective` SOLO del preset `preset_name` (no puede
/// fallar — test), sin capas de usuario/proyecto (que es justo lo que se
/// descarta cuando [`build_effectives`] falló).
#[must_use]
pub fn build_effectives_preset_only(preset_name: &str) -> (Effective, Effective) {
    let preset = preset(preset_name);
    let cmds = all_commands();
    (
        Effective::build_for_subset(&preset, &[], &cmds, Screen::Browse)
            .expect("preset browse válido"),
        Effective::build_for_subset(&preset, &[], &cmds, Screen::Viewer)
            .expect("preset viewer válido"),
    )
}

/// Adaptador: nombre de tecla GPUI (+mods +key_char) → `Chord` neutro. `None`
/// si la tecla no la modela el keymap (p. ej. teclas raras). Reusa el
/// vocabulario de nombres que entrega `gpui::Keystroke::key`.
#[must_use]
pub fn gpui_chord(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    key_char: Option<&str>,
) -> Option<Chord> {
    let code = match key {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "insert" => KeyCode::Insert,
        "delete" => KeyCode::Delete,
        f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = f[1..].parse().ok()?;
            if (1..=12).contains(&n) {
                KeyCode::F(n)
            } else {
                return None;
            }
        }
        _ => {
            // Un imprimible: prioriza key_char (fidelidad de layout/shift).
            let s = key_char.filter(|s| !s.is_empty()).unwrap_or(key);
            let mut it = s.chars();
            match (it.next(), it.next()) {
                (Some(c), None) if !c.is_control() => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Some(Chord::new(Mods { ctrl, alt, shift }, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Los TRES presets compartidos parsean y construyen (modo subset — la
    /// GUI no implementa el catálogo entero de la TUI/CLI, ver
    /// `build_for_subset`) para el contexto Browse de la GUI.
    #[test]
    fn los_tres_presets_compartidos_parsean_y_construyen_para_la_gui() {
        for name in ["orthodox", "vim", "cua"] {
            assert!(
                build_effective_from(&preset(name), &[]).is_ok(),
                "preset {name}: debe construir en modo subset para la GUI"
            );
        }
    }

    fn build_effective_from(p: &KeymapFile, l: &[KeymapFile]) -> Result<Effective, KeymapError> {
        Effective::build_for_subset(p, l, COMMANDS, Screen::Browse)
    }

    /// `build_effectives` (GUI-d T3): el preset orthodox construye AMBOS
    /// contextos (Browse + Viewer) sin error, y el visor resuelve F3 a
    /// `viewer.close` (el bind de cierre del preset).
    #[test]
    fn build_effectives_preset_ok_y_viewer_resuelve_f3() {
        let (browse, viewer) =
            build_effectives("orthodox").expect("preset orthodox: ambos contextos OK");
        let mut rb = norte_frontend::keymap::Resolver::new(browse);
        assert_eq!(
            rb.push(Chord::new(Mods::default(), KeyCode::F(3))),
            norte_frontend::keymap::Resolution::Run("pane.view".into()),
            "F3 en Browse abre el visor"
        );
        let mut rv = norte_frontend::keymap::Resolver::new(viewer);
        assert_eq!(
            rv.push(Chord::new(Mods::default(), KeyCode::F(3))),
            norte_frontend::keymap::Resolution::Run("viewer.close".into()),
            "F3 en Viewer cierra el visor"
        );
        assert_eq!(
            rv.push(Chord::new(Mods::default(), KeyCode::Esc)),
            norte_frontend::keymap::Resolution::Run("viewer.close".into())
        );
    }

    /// `build_effectives_preset_only` no puede fallar (test de la invariante
    /// documentada en su `.expect`).
    #[test]
    fn build_effectives_preset_only_no_panica() {
        let _ = build_effectives_preset_only("orthodox");
    }

    /// Diseño C2/G0 (preset compartido canónico): el preset `vim` construye
    /// para el subconjunto de comandos de la GUI y resuelve un binding REAL
    /// del `vim.toml` compartido — F3 en `[pane]` → `pane.view`
    /// (`crates/norte-frontend/presets/keymap/vim.toml`), un comando que la
    /// GUI sí implementa.
    #[test]
    fn preset_vim_construye_para_la_gui() {
        let (browse, _viewer) = build_effectives_from("vim", None)
            .expect("preset vim: construye para el subconjunto de comandos de la GUI");
        let mut r = norte_frontend::keymap::Resolver::new(browse);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::F(3))),
            norte_frontend::keymap::Resolution::Run("pane.view".into()),
            "F3 en el preset vim (pane) resuelve a pane.view"
        );
    }

    #[test]
    fn gpui_chord_nombres_y_char() {
        assert_eq!(
            gpui_chord("f5", false, false, false, None),
            Some(Chord::new(Mods::default(), KeyCode::F(5)))
        );
        assert_eq!(
            gpui_chord("up", false, false, false, None),
            Some(Chord::new(Mods::default(), KeyCode::Up))
        );
        assert_eq!(
            gpui_chord("k", true, false, false, Some("k")),
            Some(Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('k')
            ))
        );
        assert_eq!(gpui_chord("f99", false, false, false, None), None);
        // Un `key_char` compuesto (dead-key/IME multi-codepoint) → None, jamás
        // un `Char` parcial (é descompuesto = e + U+0301).
        assert_eq!(
            gpui_chord("e", false, false, false, Some("e\u{0301}")),
            None
        );
    }

    /// Un directorio de scratch único por test, para `NORTE_CONFIG_DIR`
    /// (nextest corre cada test en su propio proceso, así que mutar el env
    /// aquí es seguro — no hay carrera entre tests).
    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "norte-gui-keymap-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).expect("crear dir de scratch del test");
        d
    }

    /// `build_effectives` con `NORTE_CONFIG_DIR` apuntando a una capa que
    /// rebindea `j`/`k` a cursor.down/up (spec del plan, verificación
    /// manual Step 6, automatizada aquí): el `Effective` de Browse resultante
    /// resuelve esas teclas al comando de la capa, no al preset (que no las
    /// bindea).
    #[test]
    fn build_effective_carga_la_capa_de_usuario_via_norte_config_dir() {
        let dir = scratch_dir("rebind");
        std::fs::write(
            dir.join("keymap.toml"),
            r#"[pane]
prepend_keymap = [{ on = ["j"], run = "cursor.down" }, { on = ["k"], run = "cursor.up" }]
"#,
        )
        .unwrap();
        let (eff, _viewer) = build_effectives_from("orthodox", Some(dir.clone()))
            .expect("capa válida: carga sin error");

        let mut r = norte_frontend::keymap::Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('j'))),
            norte_frontend::keymap::Resolution::Run("cursor.down".into()),
            "la capa de usuario rebindeó j a cursor.down"
        );
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('k'))),
            norte_frontend::keymap::Resolution::Run("cursor.up".into())
        );
    }

    /// Una capa con un comando desconocido: `build_effectives` devuelve
    /// `Err` (el caller en `main.rs` cae al preset + banner, T3 Step 3) —
    /// jamás un binding muerto en silencio.
    #[test]
    fn build_effective_con_comando_desconocido_es_error() {
        let dir = scratch_dir("bad-cmd");
        std::fs::write(
            dir.join("keymap.toml"),
            r#"[pane]
prepend_keymap = [{ on = ["z"], run = "comando.inventado" }]
"#,
        )
        .unwrap();
        let result = build_effectives_from("orthodox", Some(dir.clone()));
        assert!(
            matches!(result, Err(KeymapError::UnknownCommand { .. })),
            "esperaba UnknownCommand, fue {result:?}"
        );
    }

    /// Un binding `lua:` en una capa carga sin error (el motor solo valida
    /// el charset del nombre, no que exista un host) pero `run_command` no
    /// tiene rama para `lua:*` — al resolver, no hace NADA (se "ignora" en
    /// runtime, tal como pide la verificación manual del plan: la GUI no
    /// trae host Lua).
    #[test]
    fn build_effective_con_binding_lua_carga_pero_no_ejecuta_nada() {
        let dir = scratch_dir("lua");
        std::fs::write(
            dir.join("keymap.toml"),
            r#"[pane]
prepend_keymap = [{ on = ["z"], run = "lua:foo" }]
"#,
        )
        .unwrap();
        let (eff, _viewer) = build_effectives_from("orthodox", Some(dir.clone()))
            .expect("lua: con nombre válido carga");

        let mut r = norte_frontend::keymap::Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('z'))),
            norte_frontend::keymap::Resolution::Run("lua:foo".into()),
            "el motor resuelve el binding — la GUI (sin host) lo ignora en \
             `run_command`, cuyo `_ => {{}}` cubre cualquier comando no \
             reconocido en runtime"
        );
    }

    /// ADR 0006/0035: a config layer must use prepend_keymap/append_keymap;
    /// the bare `keymap` form (preset-only) is now a load error in the GUI
    /// too (the old hand-rolled loader silently accepted it).
    #[test]
    fn capa_con_keymap_completo_es_error() {
        let dir = scratch_dir("full-keymap");
        std::fs::write(
            dir.join("keymap.toml"),
            "[pane]\nkeymap = [{ on = [\"z\"], run = \"app.quit\" }]\n",
        )
        .unwrap();
        let result = build_effectives_from("orthodox", Some(dir.clone()));
        // Pin the variant AND that the diagnostic names the culprit file —
        // the property norte_frontend::config::load_keymap_layer advertises.
        match result {
            Err(KeymapError::Toml(msg)) => {
                assert!(
                    msg.contains("keymap.toml"),
                    "culprit file in message: {msg}"
                );
            }
            other => panic!("expected Err(KeymapError::Toml(_)), got {other:?}"),
        }
    }
}
