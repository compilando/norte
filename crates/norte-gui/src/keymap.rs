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
    // H3f: the three shared presets have bound `f1` → `app.help` since H3a,
    // but this table did not list it — and the engine's lenient filter
    // dropped the binding, so F1 did nothing, silently. Since K1 there is no
    // lenient filter: a binding this frontend cannot run survives as
    // `Availability::NotHere` and says so when pressed.
    // No supplement needed: the chord comes from the shared catalogue, like
    // `app.palette`/`app.extensions`.
    "app.help",
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
/// IMPORTANT 2) y de [`is_known_preset`]. K2b Task 2: `norte-tui/src/keymap.rs
/// ::presets()` ahora también itera `NAMES`/`source()` en vez de mirror su
/// propio array — un preset nuevo toca un solo sitio (aquí y en
/// `presets::NAMES`), no cada frontend.
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

/// Fluent id for a command's short help text: `help-cmd-<dashed>` — the same
/// derivation the TUI's `help`/`palette` modules use over the shared
/// `help-cmd-*` catalogue.
///
/// Here rather than in one of its two callers (`palette_view`, `help_view`, and
/// the keyboard cheatsheet they share) because the derivation is a fact about
/// the KEYMAP vocabulary: two copies could drift, and a drifted id resolves to
/// nothing, which `norte_i18n::t` answers with the id itself — a raw
/// `help-cmd-…` painted at the reader.
#[must_use]
pub fn help_id(cmd: &str) -> String {
    format!("help-cmd-{}", cmd.replace('.', "-"))
}

/// ¿Esta pulsación significa `command` en `eff`?
///
/// Contra el keymap VIVO, y ahí está la gracia: quien pregunta es el overlay
/// abierto (¿esta tecla me cierra?) o la rama del modal (¿esta tecla abre la
/// ayuda?), y las dos quieren la tecla que el lector tiene AHORA, no la que
/// tenía cuando se congeló un resolver.
///
/// Contesta a TODOS los chords ligados al comando, no al primero: el preset
/// **vim** liga `app.help` a `f1` y a `?`, y quedarse con uno dejaba al otro
/// abriendo una página que no podía cerrar.
///
/// Una SECUENCIA (`g h`) contesta `false`. Estos sitios no llevan estado de
/// secuencia —el resolver del pane sí, ellos no—, así que la respuesta honesta
/// es que el primer chord de una secuencia no es la secuencia. Un binding que
/// esta build NO puede ejecutar tampoco casa: resuelve (y el resolver dice por
/// qué la tecla no hace nada), pero no enciende un item de menú.
///
/// K2a T4 (deuda de K1): esto corre en CADA evento de teclado, y hasta aquí
/// pedía `eff.bindings()` —que RENDERIZA cada secuencia con `Display`— para
/// volver a parsear el texto con `parse_chord`: ~3 asignaciones por binding,
/// ~140 por pulsación con los presets de hoy, ~450 con los cuatro de K2b. La
/// pregunta la contesta ahora [`Effective::single_chord_runs`] comparando
/// `Chord`s, sin renderizar nada. Misma respuesta, incluidos los dos casos
/// sutiles (secuencia y no-disponible), que el filtro de texto acertaba
/// pasando por `bindings()`.
#[must_use]
pub fn means_command(
    eff: &Effective,
    command: &str,
    key: &str,
    mods: Mods,
    key_char: Option<&str>,
) -> bool {
    gpui_chord(key, mods, key_char).is_some_and(|pressed| eff.single_chord_runs(pressed, command))
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
/// (un comando que la GUI no tiene no se ejecuta —queda `NotHere`— y este
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
    # K2b Task 2: `total-commander`/`krusader` do not bind these four — their
    # sources have no key for them (rule 1: norte's own app-level overlays,
    # not a Total Commander or Krusader concept, so nothing to transcribe),
    # unlike orthodox/vim/cua where they are the preset's OWN chords
    # (f9/f11/f12/ctrl+p, ctrl+k). Same fallback shape as `pane.ai-rename`
    # above: free in all five bundled presets, so it only ADDS a second door
    # for orthodox/vim/cua and is the ONLY door for the two imports.
    { on = ["ctrl+k"], run = "task.cancel" },
    { on = ["ctrl+,"], run = "app.settings" },
    { on = ["ctrl+e"], run = "app.extensions" },
    { on = ["ctrl+j"], run = "app.palette" },
    # `alt+.` is already how orthodox/vim/cua/krusader bind this — TC's own
    # KEYBOARD.TXT has no documented key for it at all (real TC toggles
    # hidden files from a Configuration menu, not a hotkey this file lists),
    # so this line is a no-op everywhere except total-commander.
    { on = ["alt+."], run = "pane.toggle-hidden" },
    # `alt+c` is already orthodox/vim/cua/total-commander's own chord for
    # this (`shift+f1` in TC's own source); krusader has no columns-view
    # concept documented at all, so this is its only door.
    { on = ["alt+c"], run = "pane.columns" },
    # K2b Task 3: `far`/`norton` bind `mark.all`/`mark.clear` nowhere. Both
    # sources' only "select/deselect everything" keys are Shift+Gray+/-,
    # which `parse_chord` cannot express (it rejects `shift+<char>` for ANY
    # single-character key, not only letters, and Gray+/-/* are
    # single-character keys) — see the two presets' own header comments.
    # `ctrl+a` — the chord orthodox/vim/cua/total-commander/krusader all use
    # for `mark.all` — is already Far's own `pane.properties` ("Set file
    # attributes") and stays that in the preset (rule 1: approximating is
    # worse than omitting), so it cannot double as the fallback either.
    #
    # `ctrl+g` was the first choice and is wrong: Far's own source binds it
    # to "Apply command to selected files" (far-funccmd.txt) and `far.toml`'s
    # own header lists Ctrl+G as OMITTED rather than approximated — this
    # supplement would have silently approximated it one layer up, breaking
    # the promise the preset file next to it makes. `alt+g`/`alt+G` are not
    # mentioned by any of the four bundled sources' own documentation at all
    # (not just unbound in the seven `.toml` files — checked against
    # `far-funccmd.txt`/`far-panelcmd.txt`/`tc-11.58-KEYBOARD.TXT`/
    # `krusader-keys.txt` directly), so nothing real gets shadowed.
    { on = ["alt+g"], run = "mark.all" },
    { on = ["alt+G"], run = "mark.clear" },
    # K2b Task 3: `far`/`norton` bind `pane.rename` nowhere. Both sources'
    # F6 is "rename OR move" as one combined action (Far: "Rename or move
    # file under cursor", same wording for the plain and Shift+F6 forms —
    # neither is rename-only the way total-commander's own Shift+F6 or
    # krusader's F2 is), so it is bound to `pane.move` and no key is left
    # to approximate a separate rename with.
    #
    # `ctrl+m` was the first choice and is wrong for the same reason as
    # `ctrl+g` above, three times over: Far's own source binds it to
    # "Restore previous selection" (far-panelcmd.txt, and `far.toml`'s
    # header lists it as omitted, not approximated); Total Commander's own
    # source binds it to the Multi-Rename Tool, a batch feature with no
    # single-file analogue (`tc-11.58-KEYBOARD.TXT`, and `total-commander
    # .toml`'s own header omits it for exactly that reason); Krusader's own
    # source binds it to "Open media list" (`krusader-keys.txt`). `alt+h` is
    # not mentioned by any of the four sources' own documentation.
    { on = ["alt+h"], run = "pane.rename" },
]

[viewer]
# Same K2b fallback, for the viewer screen: neither total-commander nor
# krusader documents its own internal viewer's key scheme (Lister/KrViewer
# are both named-but-undetailed in their sources), so their `[viewer]`
# sections only carry rule 7's mandatory close + six movers. `e`/`E`/`x` here
# are the same chords orthodox/vim/cua already use for these three, so this
# is a no-op for the three native presets and the only door for the two
# imports.
prepend_keymap = [
    { on = ["e"], run = "viewer.encoding" },
    { on = ["E"], run = "viewer.encoding-auto" },
    { on = ["x"], run = "viewer.hex" },
]
"#;
    parse_keymap(TOML).unwrap_or_else(|e| panic!("supplemento GUI embebido inválido: {e}"))
}

/// Los comandos que ESTA pantalla despacha de verdad.
///
/// rust-reviewer MAJOR-1: antes esto era `all_commands()`, la UNIÓN de
/// `COMMANDS` y `VIEWER_COMMANDS`, y se pasaba a los DOS `build_for`. Como
/// `build_for` mezcla `[global]` con el contexto de la pantalla, el efectivo
/// del visor lleva `f9→app.theme`, `f11→app.settings`, `f12→app.extensions`,
/// `ctrl+p→app.palette` y `tab→pane.switch` — y la unión contiene esos
/// nombres, así que salían `Availability::Here`. Pero el despacho del visor
/// es `apply_viewer_command`, que solo tiene brazos `viewer.*` y termina en
/// `_ => {}`: abrir el visor con F3 y pulsar F11 no hacía NADA y nada lo
/// decía. Es exactamente la clase de bug (F1 muerta) que K1 existe para
/// matar, sobreviviendo dentro del propio K1.
///
/// Cada pantalla se valida ahora contra el set que ELLA despacha, de modo que
/// esos bindings de `[global]` salen `NotHere` y la rama `Unavailable` deja
/// de ser inalcanzable por mentira.
fn screen_commands(screen: Screen) -> &'static [&'static str] {
    match screen {
        Screen::Viewer => VIEWER_COMMANDS,
        // La GUI no construye un efectivo de `Screen::Dialog` (no tiene
        // contexto de diálogo como keymap), pero el match debe ser total.
        Screen::Browse | Screen::Dialog => COMMANDS,
    }
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
/// [`Effective::build_for`] — the GUI implements a SUBSET of the commands the
/// shared presets bind (e.g. `pane.hotlist`), so preset bindings to commands
/// the GUI lacks are KEPT, tagged `Availability::NotHere`, instead of failing
/// the whole load (design decision C2/G0: shared preset is canonical) or, as
/// until K1, being dropped in silence.
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
    let browse = Effective::build_for(
        &preset,
        &kfs,
        screen_commands(Screen::Browse),
        Screen::Browse,
    )?;
    let viewer = Effective::build_for(
        &preset,
        &kfs,
        screen_commands(Screen::Viewer),
        Screen::Viewer,
    )?;
    Ok((browse, viewer))
}

/// Fallback: los dos `Effective` del preset `preset_name` + [`gui_supplement`],
/// sin capas de usuario/proyecto (que es justo lo que se descarta cuando
/// [`build_effectives`] falló). El supplemento SIGUE aplicando aquí: si no, una
/// capa de usuario rota tumbaría también
/// `mark.toggle`/`task.next`/`task.prev`/`task.dismiss` en el fallback
/// (revisión C2/G0 CRITICAL 1 — misma clase de regresión).
///
/// # Panics
///
/// Nunca con las entradas que existen: sus DOS entradas —el TOML del preset
/// resuelto y el del supplemento— son constantes compiladas, y un `preset_name`
/// desconocido ya cayó a `orthodox` dentro de [`preset`] ANTES de parsear.
/// Aquí no entra NADA del usuario: las capas, que son lo único que un usuario
/// escribe, ya no pasan por este camino — llegan por [`build_effectives`], que
/// devuelve `Err`, y por [`build_effectives_with`], que desde K2a T4 también.
///
/// El invariante que hace segura la `expect`, entonces, es exactamente uno:
/// **cada preset de fábrica construye, con el supplemento encima, para el
/// SUBCONJUNTO de comandos de la GUI, en las dos pantallas**. No es gratis —
/// desde K1 (ADR 0043, decisión 4) los bindings no disponibles participan en el
/// chequeo prefix-free, así que un preset con una secuencia cuyo prefijo choque
/// con un binding que esta build no ejecuta lo rompería— y por eso lo fija
/// `tests::build_effectives_preset_only_no_panica` sobre [`KNOWN_PRESETS`]
/// entero, no solo sobre orthodox. **K2b:** cada preset nuevo entra en ese test
/// o esta `expect` deja de ser honesta.
///
/// Y una advertencia sobre DÓNDE vive ese test (rust-reviewer, K2a T5):
/// `norte-gui` está fuera de `just ci` (GPUI enciende
/// `serde_json/preserve_order` y contaminaría los goldens del core), así que el
/// invariante solo lo comprueba `just gui-ci`. Quien añada un preset tiene que
/// correrlo: el gate principal se quedaría verde con esta `expect` ya rota.
#[must_use]
pub fn build_effectives_preset_only(preset_name: &str) -> (Effective, Effective) {
    // El nombre va en el mensaje: un crash que dice CUÁL preset es un
    // diagnóstico; uno que solo dice «un preset de fábrica» es un informe de
    // crash que hay que reproducir para entender.
    build_effectives_with(preset_name, &[]).unwrap_or_else(|e| {
        panic!("preset de fábrica {preset_name:?} + supplemento (constantes compiladas): {e}")
    })
}

/// [`build_effectives_preset_only`] MÁS las capas que se le pasen, sobre el
/// supplemento.
///
/// Existe para los tests que necesitan un keymap con algo REASIGNADO — la
/// mitad de la ayuda que se cierra con la tecla del lector, por ejemplo, no se
/// puede probar con el preset de fábrica, donde esa tecla ya es `F1`. Los
/// helpers que componen las capas (`preset`, `gui_supplement`,
/// `screen_commands`)
/// son privados y así siguen: lo que se expone es la composición ya hecha, que
/// es la que tiene el orden correcto.
///
/// # Errors
///
/// El primer [`KeymapError`] de cualquiera de las dos pantallas.
///
/// K1 rust-reviewer MAJOR-5, saldado aquí (K2a T4): esto terminaba en dos
/// `.expect`, y el invariante en el que se apoyaban NO era el que decía. Antes
/// era «un comando que la GUI no tiene se filtra y desaparece»; desde K1 (ADR
/// 0043, decisión 4) los bindings no disponibles PARTICIPAN en el chequeo
/// prefix-free, así que una secuencia cuyo prefijo choque con un binding que
/// este frontend no puede ejecutar es un `AmbiguousPrefix` duro. K2a añadió dos
/// vías más, y las dos las abre el USUARIO: con `vim` (`counts = true`) una
/// capa que ligue `1`-`9` es `DigitBoundWithCounts`, y una que tome `tab` es
/// `SacredKey`. Un panic aquí sustituiría el banner de arranque por un crash,
/// que es justo lo contrario de lo que esta función existe para hacer.
pub fn build_effectives_with(
    preset_name: &str,
    layers: &[KeymapFile],
) -> Result<(Effective, Effective), KeymapError> {
    let preset = preset(preset_name);
    let mut kfs = vec![gui_supplement()];
    kfs.extend_from_slice(layers);
    let browse = Effective::build_for(
        &preset,
        &kfs,
        screen_commands(Screen::Browse),
        Screen::Browse,
    )?;
    let viewer = Effective::build_for(
        &preset,
        &kfs,
        screen_commands(Screen::Viewer),
        Screen::Viewer,
    )?;
    Ok((browse, viewer))
}

/// Adaptador: nombre de tecla GPUI (+mods +key_char) → `Chord` neutro. `None`
/// si la tecla no la modela el keymap (p. ej. teclas raras). Reusa el
/// vocabulario de nombres que entrega `gpui::Keystroke::key`.
///
/// `mods` llega ya traducido desde `gpui::Modifiers` (ver `keymap_mods` en
/// `main.rs`), `platform` (⌘/Super) incluido: la GUI SÍ puede observarlo, a
/// diferencia de la TUI. Que viaje en un `Mods` en vez de en cuatro `bool`
/// sueltos es deliberado — un adaptador que crece un modificador no debe
/// crecer también un parámetro posicional más que confundir con el de al
/// lado.
#[must_use]
pub fn gpui_chord(key: &str, mods: Mods, key_char: Option<&str>) -> Option<Chord> {
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
    Some(Chord::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Todos los presets compartidos (`KNOWN_PRESETS`, no un array
    /// hardcodeado de tres — el nombre es de cuando eran tres) parsean y
    /// construyen para el contexto Browse de la GUI: la GUI no implementa el
    /// catálogo entero de la TUI/CLI, y desde K1 lo que no implementa
    /// sobrevive marcado (`Availability::NotHere`) en vez de tumbar la carga.
    #[test]
    fn los_tres_presets_compartidos_parsean_y_construyen_para_la_gui() {
        for &name in KNOWN_PRESETS {
            assert!(
                build_effective_from(&preset(name), &[]).is_ok(),
                "preset {name}: debe construir en modo subset para la GUI"
            );
        }
    }

    /// K2b Task 4, check 1 — HALF B of two. Every bundled preset
    /// (`KNOWN_PRESETS`, the shared `presets::NAMES`) builds for all three
    /// `Screen`s against the GUI's OWN vocabulary. See the TUI's twin pin
    /// (`norte_tui::keymap::tests::todos_los_presets_construyen_las_tres_pantallas_del_tui`)
    /// for why this check is split across two crates rather than living in
    /// `norte-frontend`: only a frontend that owns a `COMMANDS` list can
    /// check a preset against it.
    ///
    /// `screen_commands` already carries the exhaustive `Screen` match (it
    /// maps `Dialog` to `COMMANDS`, same as `Browse`, purely so the match
    /// compiles — the GUI has no `[dialog]` overlay CONTEXT, its overlays
    /// dispatch in code, and production never builds a `Dialog` effective).
    /// Reusing it here instead of re-deriving the mapping means this test
    /// exercises the SAME function `build_effectives_layers` does, not a
    /// parallel guess at what Dialog "should" mean for the GUI.
    #[test]
    fn todos_los_presets_construyen_las_tres_pantallas_de_la_gui() {
        for &name in KNOWN_PRESETS {
            let kf = preset(name);
            for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                Effective::build_for(&kf, &[], screen_commands(screen), screen)
                    .unwrap_or_else(|e| panic!("preset {name} en {screen:?}: {e}"));
            }
        }
    }

    /// Same pin as the TUI's: the GUI declares a SUBSET of the shared vocabulary,
    /// never its own. `COMMANDS` and `VIEWER_COMMANDS` are that subset.
    #[test]
    fn todo_comando_de_la_gui_esta_en_el_catalogo_compartido() {
        use norte_frontend::keymap::catalogue::{Status, lookup};
        for name in COMMANDS.iter().chain(VIEWER_COMMANDS.iter()) {
            let def = lookup(name)
                .unwrap_or_else(|| panic!("{name} lo implementa la GUI y no está en CATALOGUE"));
            assert_eq!(
                def.status,
                Status::Live,
                "{name} lo implementa la GUI pero el catálogo lo declara Planned"
            );
        }
    }

    /// rust-reviewer MAJOR-1: every name a screen's effective reports as
    /// `Here` must have an arm in THAT screen's dispatcher. The viewer
    /// dispatches `viewer.*` and nothing else, so a `[global]` binding
    /// visible from the viewer (F9/F11/F12/Ctrl+P/Tab) must NOT come out
    /// runnable. Before this pin they did, and the keys were dead.
    #[test]
    fn el_visor_no_declara_ejecutable_lo_que_no_despacha() {
        for &preset_name in KNOWN_PRESETS {
            let (_browse, viewer) = build_effectives_from(preset_name, None).expect("construye");
            for (seq, cmd) in viewer.bindings() {
                assert!(
                    VIEWER_COMMANDS.contains(&cmd),
                    "{preset_name}: el visor declara {cmd:?} ({seq}) ejecutable y \
                     `apply_viewer_command` no tiene brazo para él"
                );
            }
            // Y lo recíproco: siguen estando, marcados.
            let all = viewer.bindings_all();
            assert!(
                all.len() > viewer.bindings().len(),
                "{preset_name}: los bindings de [global] deben SOBREVIVIR marcados, \
                 no desaparecer — ese era el bug de K1"
            );
        }
    }

    fn build_effective_from(p: &KeymapFile, l: &[KeymapFile]) -> Result<Effective, KeymapError> {
        Effective::build_for(p, l, COMMANDS, Screen::Browse)
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
            norte_frontend::keymap::Resolution::Run {
                command: "pane.view".into(),
                count: norte_frontend::keymap::Count::None
            },
            "F3 en Browse abre el visor"
        );
        let mut rv = norte_frontend::keymap::Resolver::new(viewer);
        assert_eq!(
            rv.push(Chord::new(Mods::default(), KeyCode::F(3))),
            norte_frontend::keymap::Resolution::Run {
                command: "viewer.close".into(),
                count: norte_frontend::keymap::Count::None
            },
            "F3 en Viewer cierra el visor"
        );
        assert_eq!(
            rv.push(Chord::new(Mods::default(), KeyCode::Esc)),
            norte_frontend::keymap::Resolution::Run {
                command: "viewer.close".into(),
                count: norte_frontend::keymap::Count::None
            }
        );
    }

    /// El invariante COMPLETO de la `.expect` de
    /// [`build_effectives_preset_only`], que es lo único que queda entre el
    /// fallback de arranque y un crash.
    ///
    /// Antes de K2a T4 este test decía `build_effectives_preset_only("orthodox")`
    /// y nada más: pinchaba UN preset y confiaba en que los otros dos se
    /// parecían. Eso bastaba mientras la `.expect` estuviera dentro de
    /// `build_effectives_with`, donde el fallo que se temía llegaba por las
    /// CAPAS del usuario; ahora las capas devuelven `Err` y lo único que puede
    /// romper este camino es el contenido de un preset de fábrica contra el
    /// subconjunto de comandos de la GUI. Así que el test recorre
    /// [`KNOWN_PRESETS`] entero —el listado del catálogo compartido, no una
    /// copia: un preset que K2b añada entra aquí solo— más un nombre
    /// desconocido, que es la otra entrada real (cae a orthodox dentro de
    /// `preset`).
    ///
    /// Comprueba primero el `Result` y después llama al fallback: si un preset
    /// rompe, el fallo dice CUÁL y con qué error, en vez de un panic con el
    /// texto de la `expect`.
    #[test]
    fn build_effectives_preset_only_no_panica() {
        for name in KNOWN_PRESETS.iter().copied().chain(["no-existe-este"]) {
            let built = build_effectives_with(name, &[]);
            assert!(
                built.is_ok(),
                "preset {name} + supplemento debe construir en las dos pantallas: {:?}",
                built.err()
            );
            let _ = build_effectives_preset_only(name);
        }
    }

    /// La ruta de RECUPERACIÓN no puede entrar en pánico con la entrada de la
    /// que existe para recuperarse: una capa que deja el mapa efectivo ambiguo
    /// es la errata de un usuario, no un bug de norte (K1 rust-reviewer
    /// MAJOR-5).
    #[test]
    fn una_capa_ambigua_devuelve_error_en_vez_de_entrar_en_panico() {
        let layer = norte_frontend::keymap::parse_keymap(
            r#"
[pane]
prepend_keymap = [
    { on = ["z"], run = "cursor.down" },
    { on = ["z", "z"], run = "cursor.up" },
]
"#,
        )
        .expect("la capa parsea");
        let e = build_effectives_with("orthodox", &[layer]).unwrap_err();
        assert!(
            matches!(e, KeymapError::AmbiguousPrefix { .. }),
            "esperaba AmbiguousPrefix, fue {e:?}"
        );
    }

    /// La segunda vía viva, y la nueva: `vim` trae `counts = true` desde K2a
    /// T2, así que una capa de usuario que ligue un dígito es un error de
    /// CARGA (`DigitBoundWithCounts`). Es entrada de usuario llegando a la
    /// ruta de recuperación — exactamente lo que no puede terminar en `.expect`.
    #[test]
    fn una_capa_con_digito_sobre_vim_devuelve_error_en_vez_de_entrar_en_panico() {
        let layer = norte_frontend::keymap::parse_keymap(
            r#"
[pane]
prepend_keymap = [{ on = ["5"], run = "cursor.down" }]
"#,
        )
        .expect("la capa parsea");
        let e = build_effectives_with("vim", &[layer]).unwrap_err();
        assert!(
            matches!(e, KeymapError::DigitBoundWithCounts { .. }),
            "esperaba DigitBoundWithCounts, fue {e:?}"
        );
        // Y sobre un preset SIN contadores la misma capa es legal: lo que
        // falla es la combinación, no el dígito.
        let layer = norte_frontend::keymap::parse_keymap(
            r#"
[pane]
prepend_keymap = [{ on = ["5"], run = "cursor.down" }]
"#,
        )
        .expect("la capa parsea");
        assert!(build_effectives_with("orthodox", &[layer]).is_ok());
    }

    /// `means_command` contesta lo MISMO tras dejar de renderizar y reparsear
    /// cada binding (K2a T4), incluidos los dos casos que hacían sutil al
    /// filtro de texto: una secuencia no casa con una pulsación suelta, y un
    /// binding no disponible tampoco casa.
    #[test]
    fn means_command_ignora_secuencias_y_no_disponibles() {
        let (vim, _) = build_effectives_with("vim", &[]).expect("vim construye");
        // `g g` es una secuencia de dos chords: pulsar `g` sola no es
        // `cursor.top`.
        assert!(
            !means_command(&vim, "cursor.top", "g", Mods::default(), Some("g")),
            "el primer chord de una secuencia no es la secuencia"
        );
        // Y el caso positivo, para que lo de arriba no pase por vacío.
        assert!(means_command(
            &vim,
            "pane.view",
            "f3",
            Mods::default(),
            None
        ));
        assert!(!means_command(
            &vim,
            "pane.view",
            "f4",
            Mods::default(),
            None
        ));
        // Una tecla que el adaptador GPUI no modela contesta `false` sin
        // mirar el keymap.
        assert!(!means_command(
            &vim,
            "pane.view",
            "f99",
            Mods::default(),
            None
        ));

        // `ctrl+d` → `pane.hotlist` lo liga `orthodox` y la GUI NO lo
        // implementa: sobrevive marcado (`NotHere`), y un binding que no corre
        // no es un atajo que casa — si lo fuera, un item de menú se encendería
        // con una tecla que no hace nada.
        let (ortho, _) = build_effectives_with("orthodox", &[]).expect("orthodox construye");
        assert!(
            ortho
                .bindings_all()
                .iter()
                .any(|(seq, cmd, _)| *cmd == "pane.hotlist" && seq == "ctrl+d"),
            "el binding debe SEGUIR ahí, marcado — si no, este test no prueba nada"
        );
        assert!(
            !means_command(&ortho, "pane.hotlist", "d", ctrl(), Some("d")),
            "un binding no disponible no casa"
        );
    }

    /// H3f: the bug this closes is one of OMISSION — the presets bound `f1`
    /// and the GUI filtered the binding out, so the key did nothing and
    /// nothing said so. Pinned in all three presets, because the drop
    /// happened in the engine's lenient filter and would happen again for any
    /// of them. Since K1 the filter is gone and the binding would survive as
    /// `NotHere` — but `COMMANDS` listing it is still what makes F1 RUN, and
    /// `bindings()` is still only what runs, so the pin stands unchanged.
    #[test]
    fn app_help_esta_en_commands_y_los_presets_lo_alcanzan() {
        assert!(
            COMMANDS.contains(&"app.help"),
            "F1 was being dropped silently"
        );
        for preset in KNOWN_PRESETS {
            let (browse, _) = build_effectives_preset_only(preset);
            assert!(
                browse.bindings().iter().any(|(_, cmd)| *cmd == "app.help"),
                "{preset}: the shared preset binds f1 → app.help, and COMMANDS is what lets it through"
            );
        }
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
            norte_frontend::keymap::Resolution::Run {
                command: "pane.view".into(),
                count: norte_frontend::keymap::Count::None
            },
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
        // Los presets de fábrica (no solo orthodox): un chord retirado
        // de cualquiera en el catálogo compartido también debe romper aquí.
        for &preset_name in KNOWN_PRESETS {
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
                norte_frontend::keymap::Resolution::Run {
                    command: cmd.into(),
                    count: norte_frontend::keymap::Count::None
                },
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
                norte_frontend::keymap::Resolution::Run {
                    command: cmd.into(),
                    count: norte_frontend::keymap::Count::None
                },
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
            norte_frontend::keymap::Resolution::Run {
                command: "app.palette".into(),
                count: norte_frontend::keymap::Count::None
            }
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
            norte_frontend::keymap::Resolution::Run {
                command: "cursor.up".into(),
                count: norte_frontend::keymap::Count::None
            },
            "la capa de usuario rebindeó insert por encima del supplemento"
        );
    }

    /// `Mods` con solo `ctrl`, para no repetir el `..Default::default()` en
    /// cada caso de los tests de abajo.
    fn ctrl() -> Mods {
        Mods {
            ctrl: true,
            ..Mods::default()
        }
    }

    #[test]
    fn gpui_chord_nombres_y_char() {
        assert_eq!(
            gpui_chord("f5", Mods::default(), None),
            Some(Chord::new(Mods::default(), KeyCode::F(5)))
        );
        assert_eq!(
            gpui_chord("up", Mods::default(), None),
            Some(Chord::new(Mods::default(), KeyCode::Up))
        );
        assert_eq!(
            gpui_chord("k", ctrl(), Some("k")),
            Some(Chord::new(ctrl(), KeyCode::Char('k')))
        );
        assert_eq!(gpui_chord("f99", Mods::default(), None), None);
        // Un `key_char` compuesto (dead-key/IME multi-codepoint) → None, jamás
        // un `Char` parcial (é descompuesto = e + U+0301).
        assert_eq!(gpui_chord("e", Mods::default(), Some("e\u{0301}")), None);
    }

    /// K1 T5. La GUI SÍ observa ⌘ (`gpui::Modifiers::platform`), y desde que
    /// `Mods` tiene un sitio donde ponerlo, `on_key` ya no tiene que abortar
    /// al verlo: `Cmd+q` produce el chord `cmd+q`, NO la `q` desnuda que
    /// habría disparado `app.quit` (MINOR 2 de la revisión T3 — el bail-out
    /// que este campo sustituye). Sin `set_mod_key` de por medio: el bit
    /// físico se observa, no se interpreta.
    #[test]
    fn cmd_q_no_colapsa_a_la_q_desnuda() {
        let cmd = Mods {
            cmd: true,
            ..Mods::default()
        };
        let chord = gpui_chord("q", cmd, Some("q")).expect("⌘+q se modela");
        assert_eq!(chord, Chord::new(cmd, KeyCode::Char('q')));
        assert_ne!(
            chord,
            Chord::new(Mods::default(), KeyCode::Char('q')),
            "la q desnuda dispararía app.quit: ese era el bug"
        );
        assert_eq!(chord.to_string(), "cmd+q");
        // …y NO casa el binding de `q` del preset, que es lo que importaba.
        let (browse, _) = build_effectives_from("orthodox", None).expect("construye");
        let mut r = norte_frontend::keymap::Resolver::new(browse);
        assert_eq!(
            r.push(chord),
            norte_frontend::keymap::Resolution::Reset,
            "hoy ningún preset liga cmd+: la tecla no hace nada, en vez de salir"
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
                gpui_chord(key, Mods::default(), Some(&ch.to_string())),
                Some(Chord::new(Mods::default(), KeyCode::Char(ch))),
                "{key}: con key_char presente"
            );
            assert_eq!(
                gpui_chord(key, Mods::default(), None),
                Some(Chord::new(Mods::default(), KeyCode::Char(ch))),
                "{key}: sin key_char"
            );
            assert_eq!(
                gpui_chord(key, Mods::default(), Some("")),
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
            norte_frontend::keymap::Resolution::Run {
                command: "cursor.down".into(),
                count: norte_frontend::keymap::Count::None
            },
            "la capa de usuario rebindeó j a cursor.down"
        );
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('k'))),
            norte_frontend::keymap::Resolution::Run {
                command: "cursor.up".into(),
                count: norte_frontend::keymap::Count::None
            }
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
            norte_frontend::keymap::Resolution::Run {
                command: "lua:foo".into(),
                count: norte_frontend::keymap::Count::None
            },
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
