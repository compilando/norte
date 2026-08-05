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
    // S4: overlay de ajustes a pantalla completa. Bindeado a F11 en LOS
    // TRES presets compartidos (`f9`/`f10`/`f12` ya estaban tomados) — sin
    // supplemento propio: el chord viene del catálogo compartido, igual que
    // `app.theme`/`app.extensions`.
    "app.settings",
    // G3c: command palette overlay + extension manager full view — the
    // shared presets already bind `ctrl+p`/`f12` to these (see
    // `orthodox.toml`); `gui_supplement` below moves `task.prev` OFF
    // `ctrl+p` so the shared binding reaches `app.palette` unshadowed.
    "app.palette",
    "app.extensions",
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
    "mark.all",
    "mark.invert",
    "mark.clear",
    "pane.copy",
    "pane.move",
    "pane.delete",
    // Renombrado in situ. El nombre y el chord (`shift+f6`) son los del
    // catálogo COMPARTIDO —los tres presets ya lo bindean para la TUI—, así
    // que no necesita supplemento: lo que faltaba era la implementación, no
    // el binding.
    "pane.rename",
    "task.cancel",
    "task.next",
    "task.prev",
    "task.dismiss",
    "pane.view",
    "pane.toggle-hidden",
    "pane.refresh",
    // #108 7c: column picker overlay — the shared presets already bind
    // `alt+c` to `pane.columns` in all three, so no supplement needed.
    "pane.columns",
    // M4-IA: prompt + plan revisable de rename IA. Los presets compartidos
    // no lo bindean (en la TUI se alcanza por paleta); la GUI le da `alt+i`
    // vía `gui_supplement` para pasar el pin de alcanzabilidad
    // (`todo_comando_gui_es_alcanzable_desde_el_preset_default`).
    "pane.ai-rename",
    // M4-IA-2: prompt + hits navegables de la búsqueda semántica. Igual que
    // `pane.ai-rename`, los presets compartidos no lo bindean — la GUI le da
    // `alt+s` vía `gui_supplement` (ver su comentario) para pasar el pin de
    // alcanzabilidad.
    "pane.semantic-search",
    // Plan de ratón (tarea 4): copia al portapapeles la ruta (forma WIRE) de
    // lo que la op tocaría — las marcas, o el cursor si no hay ninguna. Nace
    // con el menú contextual, pero NO es «del menú»: es un comando como los
    // demás (paleta + chord `alt+y` vía `gui_supplement`, libre en los tres
    // presets), y el menú lo despacha igual que lo despacha el teclado.
    "pane.copy-path",
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

/// Los nombres de preset que el catálogo compartido conoce (final review
/// MINOR 4: antes un `&[&str]` hardcodeado que espejaba a mano el
/// `["orthodox", "vim", "cua"]` de `norte-tui/src/keymap.rs::presets()` — el
/// catálogo ahora expone su PROPIO listado, `norte_frontend::keymap::
/// presets::NAMES`, verificado contra `source()` por un test en
/// `norte-frontend`; la GUI solo re-exporta, sin copiar el array). Fuente del
/// mensaje "disponibles" del aviso de preset desconocido (revisión C2/G0
/// IMPORTANT 2) y de [`is_known_preset`]. La TUI sigue con su propio mirror
/// (fuera de alcance de esta revisión, ver el comentario original que
/// citaba `presets()`).
pub const KNOWN_PRESETS: &[&str] = norte_frontend::keymap::presets::NAMES;

/// ¿`name` es uno de los presets embebidos? (revisión C2/G0 IMPORTANT 2): el
/// catálogo compartido documenta "nombre desconocido → cae al default +
/// avisa" pero `norte_frontend::keymap::presets::source` solo devuelve
/// `None` — el AVISO es responsabilidad del caller (`main.rs`, antes de
/// llamar a `build_effectives`, que ya degrada en silencio vía [`preset`]).
#[must_use]
pub fn is_known_preset(name: &str) -> bool {
    norte_frontend::keymap::presets::source(name).is_some()
}

/// Preset por nombre, tomado del catálogo COMPARTIDO
/// `norte_frontend::keymap::presets` (decisión de diseño C2/G0: el preset
/// compartido es canónico — la GUI adopta sus chords para ir alineada con la
/// TUI; su viejo `keymap_presets/orthodox.toml` privado, con drift a nivel de
/// chord, queda retirado). Un nombre desconocido cae al ORTHODOX compartido
/// (mismo contrato que antes: fuente embebida, jamás I/O) — el AVISO de esa
/// degradación lo emite el caller vía [`is_known_preset`], no esta función.
///
/// # Panics
/// Solo si el TOML embebido (constante en tiempo de compilación) fuera
/// inválido — lo cubre un test para los tres presets de fábrica. El pánico
/// culpa al preset REALMENTE resuelto (`resolved`), NUNCA al `name` pedido
/// por el usuario: con un `name` desconocido ya se resolvió a `"orthodox"`
/// ANTES de parsear, así que un TOML embebido roto sería el de orthodox, no
/// el del nombre inventado (revisión C2/G0 MINOR 6).
fn preset(name: &str) -> KeymapFile {
    let (resolved, src) = match norte_frontend::keymap::presets::source(name) {
        Some(src) => (name, src),
        None => ("orthodox", norte_frontend::keymap::presets::ORTHODOX),
    };
    parse_keymap(src).unwrap_or_else(|e| panic!("preset {resolved} embebido inválido: {e}"))
}

/// Supplemento de chords EXCLUSIVOS de la GUI (revisión C2/G0 CRITICAL 1):
/// los presets compartidos no conocen comandos que solo existen aquí
/// (`mark.toggle`, `task.next/prev/dismiss`) ni el chord `delete` extra para
/// `pane.delete` que traía el viejo preset privado retirado — sin este
/// supplemento esos comandos quedarían INALCANZABLES por teclado (regresión
/// real: existían en `keymap_presets/orthodox.toml`, borrado al adoptar el
/// preset compartido). Cada chord aquí replica EXACTO el que traía ese
/// fichero (`git show 80e234d^:crates/norte-gui/src/keymap_presets/
/// orthodox.toml`). Es una CAPA (`prepend_keymap`, no `keymap`): se fusiona
/// en [`build_effectives_layers`]/[`build_effectives_preset_only`] con la
/// precedencia MÁS BAJA (primer elemento del vector de capas — ver
/// `merge_ctx`, que da prioridad al ÚLTIMO), así que una capa de
/// usuario/proyecto que rebindee estas mismas teclas sigue ganando.
///
/// # Panics
/// Solo si el TOML embebido (constante) fuera inválido — cubierto por
/// [`todo_comando_gui_es_alcanzable_desde_el_preset_default`] (indirectamente,
/// vía `build_effectives`/`build_effectives_preset_only`, que la invocan
/// siempre).
///
/// `insert`→`mark.toggle` salió de aquí al entrar en los presets
/// compartidos (#103); el resto sigue siendo GUI-only. `alt+i` →
/// `pane.ai-rename` (M4-IA) es NUEVO (no venía del preset privado retirado):
/// los presets compartidos no bindean el comando (la TUI lo alcanza por
/// paleta) y el pin de alcanzabilidad de la GUI exige al menos un chord —
/// `alt+i` estaba libre en los tres presets de fábrica CUANDO se eligió.
/// **Ya no**: los gestos de panel bindean `alt+i` → `pane.mirror` en los
/// tres presets, y `alt+s` → `pane.swap` en `vim`. No rompe nada hoy
/// (`build_for_subset` filtra los comandos que la GUI no tiene, y este
/// supplemento gana al preset), pero deja `Alt+i` significando cosas
/// distintas en cada frontend —renombrado IA aquí, espejo de panel en la
/// TUI—, que va contra la premisa del catálogo compartido. Se anota y no se
/// cambia en la rama de los gestos: reasignar un chord de la GUI es decisión
/// de la GUI. Hay que resolverlo ANTES de que `pane.mirror` entre en el
/// `COMMANDS` de la GUI, que es cuando colisionan de verdad. Ningún gate lo
/// habría avisado: `norte-gui` vive fuera del workspace. `alt+s` →
/// `pane.semantic-search` (M4-IA-2) siguió el mismo criterio: libre en los
/// tres presets (que solo usan alt+c/alt+e/alt+down/alt+f7/alt+enter/alt+x)
/// y en este supplemento. NO `alt+shift+s`: la gramática del keymap
/// compartido rechaza `shift+<char>` (el char debe ir ya «shifteado», y un
/// `alt+S` dependería de la fidelidad de `key_char` bajo alt — frágil por
/// plataforma), así que el chord llano es el robusto, como `alt+i`. `alt+y`
/// → `pane.copy-path` (plan de ratón, tarea 4) sigue el mismo criterio: ni
/// los presets ni este supplemento lo usan, y el `ctrl+shift+c` de los
/// escritorios no es expresable en esta gramática (`shift+<char>`).
fn gui_supplement() -> KeymapFile {
    const TOML: &str = r#"
[pane]
prepend_keymap = [
    { on = ["ctrl+n"], run = "task.next" },
    { on = ["ctrl+b"], run = "task.prev" },
    { on = ["ctrl+l"], run = "task.dismiss" },
    { on = ["delete"], run = "pane.delete" },
    { on = ["alt+i"], run = "pane.ai-rename" },
    { on = ["alt+s"], run = "pane.semantic-search" },
    { on = ["alt+y"], run = "pane.copy-path" },
]
"#;
    parse_keymap(TOML).unwrap_or_else(|e| panic!("supplemento GUI embebido inválido: {e}"))
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
/// The GUI-only [`gui_supplement`] goes in FIRST (lowest precedence, see
/// `merge_ctx`: the LAST layer wins) so a user/project `keymap.toml` can
/// still rebind `insert`/`ctrl+n`/`ctrl+b`/`ctrl+l`/`delete` on top of it
/// (revisión C2/G0 CRITICAL 1). G3c: `task.prev` moved from `ctrl+p` to
/// `ctrl+b` — `gui_supplement` OUTRANKS the shared preset (it's a layer
/// merged on top of it), so it used to shadow the preset's `ctrl+p` →
/// `app.palette` binding entirely; freeing the chord is what makes the
/// shared binding reach the palette once `app.palette` joins [`COMMANDS`].
///
/// # Errors
/// The first `KeymapError` from any layer.
fn build_effectives_layers(
    layers: &Layers,
    preset_name: &str,
) -> Result<(Effective, Effective), KeymapError> {
    let preset = preset(preset_name);
    let mut kfs: Vec<KeymapFile> = vec![gui_supplement()];
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

/// Fallback: los dos `Effective` del preset `preset_name` + [`gui_supplement`]
/// (no puede fallar — test), sin capas de usuario/proyecto (que es justo lo
/// que se descarta cuando [`build_effectives`] falló). El supplemento SIGUE
/// aplicando aquí: si no, una capa de usuario rota tumbaría también
/// `mark.toggle`/`task.next`/`task.prev`/`task.dismiss` en el fallback
/// (revisión C2/G0 CRITICAL 1 — misma clase de regresión).
#[must_use]
pub fn build_effectives_preset_only(preset_name: &str) -> (Effective, Effective) {
    let preset = preset(preset_name);
    let supplement = [gui_supplement()];
    let cmds = all_commands();
    (
        Effective::build_for_subset(&preset, &supplement, &cmds, Screen::Browse)
            .expect("preset browse válido"),
        Effective::build_for_subset(&preset, &supplement, &cmds, Screen::Viewer)
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
        // Nombres de keysym que el fallback imprimible rechazaría por
        // multi-carácter (#103): son 4-8 chars, así que `(Some(c), None)` no
        // casa y el chord se perdería. DEFENSA, no bug vivo: en la revisión
        // pineada de gpui, `keystroke_from_xkb` ya mapea `Keysym::plus` →
        // `"+"` (y asterisk/minus/slash/question/colon igual) ANTES de caer
        // en `keysym_get_name`, así que hoy `key` nunca trae el nombre para
        // estas teclas — verificado en el checkout del rev pineado, no
        // inferido. Estos brazos existen para que un bump de gpui que
        // cambie esa tabla no deje MUDOS `mark.invert` y compañía solo en la
        // GUI, que vive fuera del workspace y no la cubre el gate normal.
        // `plus` es además la única grafía del chord (`+` es el separador de
        // modificadores en el keymap).
        "plus" => KeyCode::Char('+'),
        "asterisk" => KeyCode::Char('*'),
        "minus" => KeyCode::Char('-'),
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

    /// Pin PERMANENTE de la clase de regresión (revisión C2/G0 CRITICAL 1):
    /// adoptar el preset compartido dejó SIN chord a `mark.toggle`,
    /// `task.next`, `task.prev` y `task.dismiss` (el preset compartido no
    /// conoce esos comandos GUI-only) hasta que se añadió
    /// [`gui_supplement`]. Este test recorre TODO el catálogo de la GUI
    /// (`COMMANDS` + `VIEWER_COMMANDS`) contra el efectivo POR DEFECTO
    /// (preset orthodox + supplemento, sin capas de usuario) y exige que
    /// cada comando tenga AL MENOS un chord.
    ///
    /// Sin exenciones: cotejado a mano contra el viejo preset privado
    /// retirado (`git show 80e234d^:crates/norte-gui/src/keymap_presets/
    /// orthodox.toml`), la combinación preset compartido + supplemento
    /// cubre el catálogo COMPLETO — los 10 `VIEWER_COMMANDS` ya estaban
    /// enteros en el `[viewer]` del preset compartido (nada que suplir); de
    /// `COMMANDS`, solo `mark.toggle`/`task.next`/`task.prev`/
    /// `task.dismiss` faltaban, y los cuatro los repone el supplemento.
    #[test]
    fn todo_comando_gui_es_alcanzable_desde_el_preset_default() {
        // Los TRES presets de fábrica (no solo orthodox): un chord retirado
        // de vim/cua en el catálogo compartido también debe romper aquí.
        for preset_name in ["orthodox", "vim", "cua"] {
            let (browse, viewer) = build_effectives_from(preset_name, None)
                .expect("preset de fábrica + supplemento: construye");
            let browse_cmds: std::collections::HashSet<&str> =
                browse.bindings().iter().map(|(_, cmd)| *cmd).collect();
            let viewer_cmds: std::collections::HashSet<&str> =
                viewer.bindings().iter().map(|(_, cmd)| *cmd).collect();
            for cmd in COMMANDS {
                assert!(
                    browse_cmds.contains(cmd),
                    "comando Browse {cmd:?} sin NINGÚN chord en {preset_name}"
                );
            }
            for cmd in VIEWER_COMMANDS {
                assert!(
                    viewer_cmds.contains(cmd),
                    "comando Viewer {cmd:?} sin NINGÚN chord en {preset_name}"
                );
            }
        }
    }

    /// El supplemento reproduce EXACTO los 5 chords del viejo preset privado
    /// (mismo `on` — verificado contra `git show 80e234d^:.../orthodox.toml`
    /// en el comentario de [`gui_supplement`]), incluido `delete` como
    /// alt-chord de `pane.delete` (que YA alcanza por `f8` del preset
    /// compartido — este chord es paridad, no cobertura nueva).
    #[test]
    fn gui_supplement_reproduce_los_chords_del_viejo_preset_privado() {
        let (browse, _viewer) = build_effectives_from("orthodox", None).expect("construye");
        let mut r = norte_frontend::keymap::Resolver::new(browse);
        let cases = [
            (KeyCode::Insert, "mark.toggle"),
            (KeyCode::Delete, "pane.delete"),
        ];
        for (code, cmd) in cases {
            assert_eq!(
                r.push(Chord::new(Mods::default(), code)),
                norte_frontend::keymap::Resolution::Run(cmd.into()),
                "{code:?} debe resolver a {cmd:?}"
            );
        }
        let ctrl_cases = [
            (KeyCode::Char('n'), "task.next"),
            // G3c: moved from ctrl+p to ctrl+b — ctrl+p is the SHARED
            // preset's `app.palette` binding, which `gui_supplement`
            // (higher precedence than the preset, see `build_effectives_layers`'s
            // doc) used to shadow entirely.
            (KeyCode::Char('b'), "task.prev"),
            (KeyCode::Char('l'), "task.dismiss"),
        ];
        for (code, cmd) in ctrl_cases {
            let chord = Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                code,
            );
            assert_eq!(
                r.push(chord),
                norte_frontend::keymap::Resolution::Run(cmd.into()),
                "ctrl+{code:?} debe resolver a {cmd:?}"
            );
        }
    }

    /// G3c: freeing `ctrl+p` from `gui_supplement` (moved to `ctrl+b`, see
    /// the test above) lets the SHARED preset's own `ctrl+p` →
    /// `app.palette` binding reach the resolver unshadowed, now that
    /// `app.palette` joined [`COMMANDS`].
    #[test]
    fn ctrl_p_ya_no_lo_tapa_el_supplemento_y_llega_a_app_palette() {
        let (browse, _viewer) = build_effectives_from("orthodox", None).expect("construye");
        let mut r = norte_frontend::keymap::Resolver::new(browse);
        let chord = Chord::new(
            Mods {
                ctrl: true,
                ..Default::default()
            },
            KeyCode::Char('p'),
        );
        assert_eq!(
            r.push(chord),
            norte_frontend::keymap::Resolution::Run("app.palette".into())
        );
    }

    /// El supplemento tiene precedencia MÁS BAJA que una capa de
    /// usuario/proyecto (revisión C2/G0 CRITICAL 1): rebindear `insert` en
    /// una capa de usuario gana sobre `insert`→`mark.toggle` del
    /// supplemento.
    #[test]
    fn capa_de_usuario_puede_rebindear_encima_del_supplemento() {
        let dir = scratch_dir("supplement-override");
        std::fs::write(
            dir.join("keymap.toml"),
            r#"[pane]
prepend_keymap = [{ on = ["insert"], run = "cursor.up" }]
"#,
        )
        .unwrap();
        let (browse, _viewer) = build_effectives_from("orthodox", Some(dir.clone()))
            .expect("capa válida: carga sin error");
        let mut r = norte_frontend::keymap::Resolver::new(browse);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Insert)),
            norte_frontend::keymap::Resolution::Run("cursor.up".into()),
            "la capa de usuario rebindeó insert por encima del supplemento"
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

    /// #103 review: X11/Wayland keysym NAMES the printable fallback would
    /// reject outright (`"plus"`/`"asterisk"`/`"minus"` are multi-character,
    /// so `(Some(c), None)` never matches) — the mark preset bindings
    /// (`plus`/`*`/`-`) would go silently dead in the GUI whenever
    /// `key_char` is absent or empty. Covers each token BOTH ways: with
    /// `key_char` present (the printable fallback already resolves it, so
    /// this half passes with or without the fix) and absent/empty (only the
    /// named arm resolves it — this half is the RED case).
    #[test]
    fn gpui_chord_nombres_de_keysym_para_la_puntuacion_de_los_presets() {
        let cases = [("plus", '+'), ("asterisk", '*'), ("minus", '-')];
        for (key, ch) in cases {
            assert_eq!(
                gpui_chord(key, false, false, false, Some(&ch.to_string())),
                Some(Chord::new(Mods::default(), KeyCode::Char(ch))),
                "{key}: con key_char presente"
            );
            assert_eq!(
                gpui_chord(key, false, false, false, None),
                Some(Chord::new(Mods::default(), KeyCode::Char(ch))),
                "{key}: sin key_char"
            );
            assert_eq!(
                gpui_chord(key, false, false, false, Some("")),
                Some(Chord::new(Mods::default(), KeyCode::Char(ch))),
                "{key}: key_char vacío"
            );
        }
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
