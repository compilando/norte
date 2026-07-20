//! Keymap de la GUI (GUI-c): COMMANDS del contexto Browse, preset orthodox
//! embebido, carga de capas (sistema/usuario/proyecto) y el adaptador
//! nombre-de-tecla-GPUI → `Chord` neutro. El MOTOR es `norte_frontend::keymap`.

use norte_frontend::keymap::{
    Chord, Effective, KeyCode, KeymapError, KeymapFile, Mods, Screen, parse_keymap,
};
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
];

/// El preset orthodox de la GUI, parseado. Panics solo si el TOML embebido es
/// inválido (lo cubre un test).
fn orthodox() -> KeymapFile {
    parse_keymap(include_str!("keymap_presets/orthodox.toml"))
        .unwrap_or_else(|e| panic!("preset orthodox embebido inválido: {e}"))
}

/// Directorios de capa en precedencia ASCENDENTE (sistema → usuario → proyecto),
/// como la TUI: `/etc/norte` (o `%ProgramData%`), config dir XDG, `./.norte`.
/// `NORTE_CONFIG_DIR` fuerza la capa de usuario (tests/headless).
fn layer_dirs() -> Vec<(PathBuf, bool)> {
    // (dir, es_proyecto)
    let mut v = Vec::new();
    #[cfg(unix)]
    v.push((PathBuf::from("/etc/norte"), false));
    let user = std::env::var_os("NORTE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("norte")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/norte")));
    if let Some(u) = user {
        v.push((u, false));
    }
    v.push((PathBuf::from("./.norte"), true));
    v
}

/// Construye el `Effective` del contexto Browse: preset orthodox + capas
/// `keymap.toml` presentes. Un keymap.toml inválido NO tumba la GUI: devuelve
/// el error (el caller cae al preset con banner). Fail-SAFE, no fail-closed.
///
/// # Errors
/// El primer `KeymapError` de una capa (parse/chord/comando/prefijo).
pub fn build_effective() -> Result<Effective, KeymapError> {
    let preset = orthodox();
    let mut layers: Vec<KeymapFile> = Vec::new();
    for (dir, is_project) in layer_dirs() {
        let path = dir.join("keymap.toml");
        if let Ok(src) = std::fs::read_to_string(&path) {
            let mut kf = parse_keymap(&src)?;
            if is_project {
                kf.mark_project();
            }
            layers.push(kf);
        } // ausente/no legible: la capa no aporta.
    }
    Effective::build_for(&preset, &layers, COMMANDS, Screen::Browse)
}

/// El `Effective` construido SOLO con el preset orthodox, sin capas — el
/// fallback cuando una capa de usuario/proyecto está rota (`build_effective`
/// devolvió `Err`): la GUI sigue arrancando con las teclas por defecto.
///
/// # Panics
/// Nunca en la práctica: el preset embebido es fijo y lo cubre
/// `preset_orthodox_valido_y_construye`; un panic aquí sería un preset roto
/// que el propio test ya habría cazado antes de llegar a producción.
#[must_use]
pub fn build_effective_preset_only() -> Effective {
    Effective::build_for(&orthodox(), &[], COMMANDS, Screen::Browse)
        .expect("preset orthodox válido")
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

    #[test]
    fn preset_orthodox_valido_y_construye() {
        assert!(build_effective_from(&orthodox(), &[]).is_ok());
    }

    fn build_effective_from(p: &KeymapFile, l: &[KeymapFile]) -> Result<Effective, KeymapError> {
        Effective::build_for(p, l, COMMANDS, Screen::Browse)
    }

    #[test]
    fn build_effective_preset_only_no_panica() {
        // Cubre la invariante que documenta el `.expect` de la función.
        let _ = build_effective_preset_only();
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

    /// `build_effective` con `NORTE_CONFIG_DIR` apuntando a una capa que
    /// rebindea `j`/`k` a cursor.down/up (spec del plan, verificación
    /// manual Step 6, automatizada aquí): el `Effective` resultante resuelve
    /// esas teclas al comando de la capa, no al preset (que no las bindea).
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
        // SAFETY/concurrencia: proceso-por-test de nextest (ver `scratch_dir`).
        std::env::set_var("NORTE_CONFIG_DIR", &dir);
        let eff = build_effective().expect("capa válida: carga sin error");
        std::env::remove_var("NORTE_CONFIG_DIR");

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

    /// Una capa con un comando desconocido: `build_effective` devuelve
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
        std::env::set_var("NORTE_CONFIG_DIR", &dir);
        let result = build_effective();
        std::env::remove_var("NORTE_CONFIG_DIR");
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
        std::env::set_var("NORTE_CONFIG_DIR", &dir);
        let eff = build_effective().expect("lua: con nombre válido carga");
        std::env::remove_var("NORTE_CONFIG_DIR");

        let mut r = norte_frontend::keymap::Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('z'))),
            norte_frontend::keymap::Resolution::Run("lua:foo".into()),
            "el motor resuelve el binding — la GUI (sin host) lo ignora en \
             `run_command`, cuyo `_ => {{}}` cubre cualquier comando no \
             reconocido en runtime"
        );
    }
}
