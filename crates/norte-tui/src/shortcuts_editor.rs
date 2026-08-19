//! El editor de atajos: qué se puede ligar, a qué, y qué se escribe al disco.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, con sus
//! 419 líneas de test dentro de `main.rs`. Y no podía salir antes que
//! [`crate::paste::route_paste`]: esos tests comprueban que un pegado sobre el
//! campo que está capturando un chord no entra como texto, así que el editor y
//! el enrutador de pegado salen en ese orden y no en otro.
//!
//! [`Maps`] es el trío de mapas VIVOS. El editor lee sus filas y cada veredicto
//! de ahí y nunca de una copia tomada al abrir: un hot-reload reemplaza los
//! tres, y un veredicto leído de un mapa viejo es un veredicto sobre el teclado
//! de otra persona.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_i18n::{t, ta};

use crate::app::{App, KeymapsError, PAGE, Shortcuts, io_error_category};
use crate::config;
use crate::keymap::{
    COMMANDS, DIALOG_COMMANDS, Effective, RebindWrite, Screen, UnbindWrite, chord_from_crossterm,
    presets,
};

/// The three LIVE effective maps, borrowed from the resolvers that own them.
///
/// The shortcut editor reads its rows and every verdict off these, never off a
/// copy taken when it opened: a hot reload replaces all three (`reload_config`)
/// and a verdict read from a stale map is a verdict about somebody else's
/// keyboard.
pub struct Maps<'a> {
    /// El efectivo de la pantalla de navegación.
    pub browse: &'a Effective,
    /// El del visor.
    pub viewer: &'a Effective,
    /// El de los diálogos.
    pub dialog: &'a Effective,
}

impl Maps<'_> {
    /// The map of `screen` — the one a row of that screen was built from, and
    /// the one its verdict must be read off.
    fn of(&self, screen: Screen) -> &Effective {
        match screen {
            Screen::Browse => self.browse,
            Screen::Viewer => self.viewer,
            Screen::Dialog => self.dialog,
        }
    }
}

/// The command set a keymap for `screen` is VALIDATED against — what
/// [`build_keymaps`] passes to `Effective::build_for`, and what the shortcut
/// editor's dry run must pass too.
///
/// Wider than what the screen dispatches, deliberately, and only for
/// [`Screen::Dialog`]: that map merges `[global]` (ADR 0006/H1 T1), so a
/// binding like `ctrl+c → app.quit` would validate as `UnknownCommand` against
/// the dialog verbs alone and take the WHOLE layer down with it. The editor's
/// dry run runs the real loader, so it needs the real set — the narrower "what
/// may be bound here" question is `bindable_commands` (privada)'s.
#[must_use]
pub fn known_commands(screen: Screen) -> Vec<&'static str> {
    match screen {
        Screen::Dialog => COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect(),
        Screen::Browse | Screen::Viewer => COMMANDS.to_vec(),
    }
}

/// The commands the shortcut editor offers as UNBOUND rows for `screen` — what
/// this frontend actually dispatches there.
///
/// Not [`known_commands`], and the difference is the point of the list: that
/// set answers "would the layer load", this one answers "will the key do
/// something". The browse screen dispatches everything; the viewer owns the
/// keyboard while it is open and dispatches its own verbs plus the `app.*` ones
/// that reach it through `[global]` (the palette opens from the viewer); an
/// overlay dispatches its `dialog.*` allowlist. Offering `pane.copy` as a
/// bindable viewer command would answer "how do I press X" with a key that does
/// nothing there.
fn bindable_commands(screen: Screen) -> Vec<&'static str> {
    match screen {
        Screen::Viewer => COMMANDS
            .iter()
            .copied()
            .filter(|c| c.starts_with("viewer.") || c.starts_with("app."))
            .collect(),
        Screen::Dialog => DIALOG_COMMANDS.to_vec(),
        Screen::Browse => COMMANDS.to_vec(),
    }
}

/// The editor's rows for the three screens, in the order the help page uses
/// (browse, viewer, dialog) — rebuilt whole, never patched row by row, exactly
/// like `help_lines` and the palette's rows.
pub fn shortcut_rows(maps: &Maps<'_>) -> Vec<norte_frontend::shortcuts::ShortcutRow> {
    let lang = norte_i18n::active();
    let bindable: Vec<Vec<&'static str>> = [Screen::Browse, Screen::Viewer, Screen::Dialog]
        .into_iter()
        .map(bindable_commands)
        .collect();
    let screens: Vec<norte_frontend::shortcuts::ScreenKeys<'_>> =
        [Screen::Browse, Screen::Viewer, Screen::Dialog]
            .into_iter()
            .zip(&bindable)
            .map(|(screen, bindable)| norte_frontend::shortcuts::ScreenKeys {
                screen,
                eff: maps.of(screen),
                bindable,
            })
            .collect();
    norte_frontend::shortcuts::build_rows(&screens, lang)
}

/// La puerta, tal y como la llama ESTE frontend: el nombre del preset activo,
/// las capas cargadas y el set con el que se valida esa pantalla.
///
/// La puerta en sí vive en `norte_frontend::shortcuts::plan_rebind` — el corte
/// de las capas (`RebindSources::split_at`) no es del frontend, y una GUI que
/// lo rehiciera a mano es justo el lector que su documentación avisa que se va
/// a equivocar en silencio. Aquí solo queda lo que sí es de la TUI: de dónde
/// sale el nombre del preset y qué comandos valida cada pantalla.
fn plan_rebind(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    screen: Screen,
    seq: &[crate::keymap::Chord],
    command: &str,
) -> Result<RebindWrite, norte_frontend::shortcuts::PlanError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let known = known_commands(screen);
    norte_frontend::shortcuts::plan_rebind(
        preset_name,
        &cfg.keymap_layer_kinds,
        &cfg.keymap_layers,
        &known,
        screen,
        seq,
        command,
    )
}

/// The unbind's own door call — same cut, same preset lookup as
/// [`plan_rebind`], for the removal instead of the write. See that
/// function's doc for why the split is not this frontend's to redo.
fn plan_unbind(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    screen: Screen,
    seq: &[crate::keymap::Chord],
) -> Result<UnbindWrite, norte_frontend::shortcuts::PlanError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let known = known_commands(screen);
    norte_frontend::shortcuts::plan_unbind(
        preset_name,
        &cfg.keymap_layer_kinds,
        &cfg.keymap_layers,
        &known,
        screen,
        seq,
    )
}

/// Teclas del editor de atajos (K3c, `app.shortcuts`): mismo criterio que el
/// overlay de ajustes de arriba — teclas fijas, hardcodeadas aquí.
///
/// El modo CAPTURA es lo que no cabía como un brazo más de `on_settings_key`:
/// mientras está activo TODA tecla es el chord que se está capturando, no un
/// atajo de la pantalla. Solo `Esc` se queda fuera, porque es lo que cancela —
/// y por eso es el único chord que este editor no puede capturar, cosa que la
/// pantalla DICE en vez de dejar al lector pulsándolo.
///
/// `Enter` sí se puede capturar: en la fase de espera es una tecla como
/// cualquier otra, y solo confirma DESPUÉS, con un veredicto ya en pantalla.
pub async fn on_shortcuts_key(
    app: &mut App,
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
    maps: &Maps<'_>,
    mods: KeyModifiers,
    code: KeyCode,
) {
    // La salida de emergencia global, SALVO capturando: `ctrl+c` es un chord
    // que un converso de CUA quiere ligar (es su «copiar»), y en modo captura
    // el lector está pulsando teclas a ciegas por diseño — cerrar norte ahí
    // sería la peor lectura posible de una tecla que el editor pidió. Con la
    // captura abierta la salida es `esc`, que es lo que la pantalla dice.
    let capturing = app.shortcuts.as_ref().is_some_and(Shortcuts::is_capturing);
    if !capturing && mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(sc) = &mut app.shortcuts else {
        return;
    };
    match shortcuts_key(sc, maps, mods, code) {
        ShortcutsKeyOutcome::None => {}
        ShortcutsKeyOutcome::Close => app.shortcuts = None,
        ShortcutsKeyOutcome::Confirm => confirm_shortcut(app, cfg, cli_preset).await,
        ShortcutsKeyOutcome::Unbind => unbind_shortcut(app, cfg, cli_preset).await,
        ShortcutsKeyOutcome::NotBindable => app.message = Some(t("msg-shortcut-not-bindable")),
        ShortcutsKeyOutcome::RowIsGlobal => app.message = Some(t("shortcuts-row-global")),
    }
}

/// Qué pidió una tecla del editor de atajos — el cómputo PURO, dentro del
/// borrow de `app.shortcuts`, separado del I/O async igual que
/// [`SettingsKeyOutcome`] lo separa en el overlay de ajustes.
enum ShortcutsKeyOutcome {
    /// Consumida sin nada pendiente (navegación, filtro, captura).
    None,
    /// `Esc` fuera de captura: cierra la pantalla.
    Close,
    /// Escribir la captura, si la puerta la deja pasar.
    Confirm,
    /// Quitar el binding de la fila bajo el cursor.
    Unbind,
    /// La tecla capturada no la modela el keymap (Media, `CapsLock`…): no hay
    /// chord que capturar, y fingir uno sería ligar otra cosa.
    NotBindable,
    /// La fila bajo el cursor es de `[global]` (#141): ni rebind ni unbind
    /// pueden escribir ahí desde una fila que nombra una sola pantalla.
    RowIsGlobal,
}

/// El estado del editor tras una tecla. Puro y sincrónico: es donde vive la
/// regla de la captura, y es lo que los tests pueden conducir sin runtime.
fn shortcuts_key(
    sc: &mut Shortcuts,
    maps: &Maps<'_>,
    mods: KeyModifiers,
    code: KeyCode,
) -> ShortcutsKeyOutcome {
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if let Some(capture) = sc.capture() {
        let waiting = capture.is_waiting();
        let screen = capture.screen();
        match code {
            // Cancela SIEMPRE, en las dos fases, y por eso `esc` es el único
            // chord que no se puede capturar. `esc` PELADO: el contrato de la
            // pantalla es «esc cancela», no «cualquier cosa que acabe en esc»,
            // así que `shift+esc` y `alt+esc` siguen siendo chords ligables.
            KeyCode::Esc if plain => sc.cancel_capture(),
            KeyCode::Enter if !waiting => return ShortcutsKeyOutcome::Confirm,
            KeyCode::Backspace if !waiting => sc.recapture(),
            // Un codepoint peligroso no viene de una tecla: viene de un
            // PEGADO. Desde #143 la defensa PRIMARIA es `route_paste`, que
            // intercepta el `Event::Paste` entero ANTES de que llegue aquí
            // (mientras `waiting`, lo rechaza entero — un capture responde a
            // UNA tecla física, nunca a un pegado). Este brazo sigue vivo
            // como RESPALDO: un terminal o multiplexor que no honre
            // `\e[?2004h` sigue entregando el pegado como `Char`s sueltos,
            // uno por uno, y sin este guard `parse_chord` lo aceptaría y el
            // escritor lo dejaría crudo en el `keymap.toml` del usuario — un
            // fichero que ninguna pantalla de norte pinta crudo, pero que su
            // editor de texto sí. `un_codepoint_peligroso_pegado_no_se_captura`
            // prueba ESTA rama directamente (vía `Event::Key`), independiente
            // de `route_paste`, para que una regresión en la defensa primaria
            // no deje también sin cobertura la de respaldo.
            _ if waiting && hostile_key(code) => return ShortcutsKeyOutcome::NotBindable,
            _ if waiting => match chord_from_crossterm(mods, code) {
                // El mapa es el de LA FILA (`maps.of`), no el de la pantalla
                // que el lector estaba mirando: `Tab` está libre en el viewer
                // y reservado en el browser, y el veredicto tiene que hablar
                // del teclado que se va a editar.
                Some(chord) => sc.capture_chord(chord, maps.of(screen)),
                None => return ShortcutsKeyOutcome::NotBindable,
            },
            // Con un veredicto en pantalla, el resto de teclas no hacen nada:
            // confirmar, recapturar o cancelar son las tres salidas, y el pie
            // las nombra.
            _ => {}
        }
        return ShortcutsKeyOutcome::None;
    }
    match code {
        KeyCode::Char('u') if mods == KeyModifiers::CONTROL => return ShortcutsKeyOutcome::Unbind,
        KeyCode::Char(c) if plain => sc.push_char(c),
        KeyCode::Backspace if plain => sc.backspace(),
        KeyCode::Esc if plain => return ShortcutsKeyOutcome::Close,
        KeyCode::Up if plain => sc.up(),
        KeyCode::Down if plain => sc.down(),
        KeyCode::PageUp if plain => sc.page_up(PAGE),
        KeyCode::PageDown if plain => sc.page_down(PAGE),
        // `is_editable` false means [`ShortcutsState::begin_capture`] would
        // silently refuse anyway (#141) — checked here too so the reader
        // gets told WHY instead of nothing happening.
        KeyCode::Enter if plain && sc.selected().is_some_and(|r| !r.is_editable()) => {
            return ShortcutsKeyOutcome::RowIsGlobal;
        }
        KeyCode::Enter if plain => {
            sc.begin_capture();
        }
        _ => {}
    }
    ShortcutsKeyOutcome::None
}

/// ¿Es esta tecla un codepoint que no debe acabar crudo en un fichero de
/// configuración? Solo alcanzable por pegado — ninguna tecla física entrega un
/// RLO —, y por eso se RECHAZA en vez de enmascararse: enmascarar ligaría un
/// chord distinto del que el fichero diría.
fn hostile_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Char(c) if norte_encoding::is_terminal_hazard(c))
}

/// Confirma la captura: la puerta ([`plan_rebind`]) y, solo si pasa, el
/// escritor — en `spawn_blocking` (regla 2: `persist_keymap_bind` toma un lock
/// de fichero y hace I/O síncrona, y esto corre en el hilo de la UI).
///
/// Lo que llega al escritor es lo que devolvió la puerta, TAL CUAL: la sección,
/// la lista (`prepend_keymap` — un `append` no pisa al preset y no dispararía
/// nunca) y la ortografía de los chords. Re-renderizar aquí la secuencia
/// capturada reabriría justo el hueco que la puerta cierra.
///
/// El fichero escrito lo ve el watcher de `keymap.toml`, que dispara
/// `reload_config`: de ahí sale el efecto EN VIVO, y de ahí sale también el
/// refresco de esta pantalla.
async fn confirm_shortcut(app: &mut App, cfg: &config::LoadedConfig, cli_preset: Option<&str>) {
    let captured = app.shortcuts.as_ref().and_then(|sc| {
        sc.confirmable()
            .map(|(screen, command, seq)| (screen, command.to_owned(), seq.to_vec()))
    });
    // `None` = veredicto de rechazo (o nada capturado): no se escribe nada y la
    // captura sigue viva para que el lector pruebe otra tecla — pero el Enter
    // que acaba de pulsar no puede quedarse mudo: se repite el veredicto en la
    // barra, que es la razón por la que no se guardó.
    let Some((screen, command, seq)) = captured else {
        if let Some(v) = app
            .shortcuts
            .as_ref()
            .and_then(norte_frontend::shortcuts::ShortcutsState::capture)
            .and_then(norte_frontend::shortcuts::Capture::verdict)
        {
            app.message = Some(norte_frontend::shortcuts::verdict_message(
                v,
                norte_i18n::active(),
            ));
        }
        return;
    };
    let Some(dir) = config::user_config_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let write = match plan_rebind(cfg, cli_preset, screen, &seq, &command) {
        Ok(w) => w,
        Err(e) => {
            app.message = Some(norte_frontend::shortcuts::plan_error_message(
                &e,
                norte_i18n::active(),
            ));
            if let Some(sc) = &mut app.shortcuts {
                sc.cancel_capture();
            }
            return;
        }
    };
    let painted = crate::keymap::paint_chord(&write.chords.join(" "));
    let label = norte_frontend::whichkey::command_label(&command, norte_i18n::active());
    let res = tokio::task::spawn_blocking(move || {
        config::persist_keymap_bind(
            &dir,
            write.section,
            write.list,
            &write.chords,
            &write.command,
        )
    })
    .await;
    app.message = Some(match res {
        Ok(Ok(_)) => ta(
            "msg-shortcut-bound",
            &[("chord", &painted), ("command", &label)],
        ),
        Ok(Err(e)) => ta(
            "msg-settings-save-failed",
            &[("error", &io_error_category(&e))],
        ),
        // Un panic en el write es un bug nuestro: que no tumbe la TUI (misma
        // disciplina que `persist_setting`).
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_keymap_bind no terminó");
            t("msg-settings-save-crashed")
        }
    });
    if let Some(sc) = &mut app.shortcuts {
        sc.cancel_capture();
    }
}

/// Quita el binding de la fila bajo el cursor — la razón de que c1 escribiera
/// `persist_keymap_unbind`: un editor que solo añade es un editor que no
/// arregla un error.
///
/// Ahora pasa por la misma puerta que el bind ([`plan_unbind`] /
/// `unbind_dry_run`, #141): casa por secuencia PARSEADA, no por bytes, así
/// que un gemelo escrito a mano (`mod+p` por `ctrl+p`) se encuentra y se
/// escribe con SU propia ortografía; y el mensaje sale del mapa
/// RECONSTRUIDO — qué ejecuta la tecla AHORA — en vez de "quitado de tu
/// keymap.toml", que era cierto e inútil en cuanto otra capa seguía
/// ligándola. Una fila `[global]` ni siquiera llega a la puerta: se refleja
/// en la fila misma (`ShortcutRow::is_editable`) y se rechaza antes, con su
/// propio mensaje.
async fn unbind_shortcut(app: &mut App, cfg: &config::LoadedConfig, cli_preset: Option<&str>) {
    let Some(row) = app
        .shortcuts
        .as_ref()
        .and_then(norte_frontend::shortcuts::ShortcutsState::selected)
    else {
        return;
    };
    if !row.is_editable() {
        app.message = Some(t("shortcuts-row-global"));
        return;
    }
    if !row.is_bound() {
        app.message = Some(t("msg-shortcut-nothing-to-unbind"));
        return;
    }
    let screen = row.screen;
    let seq: Vec<crate::keymap::Chord> = row.seq.clone();
    let painted = row.chord.clone();
    let write = match plan_unbind(cfg, cli_preset, screen, &seq) {
        Ok(w) => w,
        Err(e) => {
            app.message = Some(norte_frontend::shortcuts::plan_error_message(
                &e,
                norte_i18n::active(),
            ));
            return;
        }
    };
    if matches!(write.outcome, crate::keymap::UnbindOutcome::NotBound) {
        // Nada que escribir: el propio door ya vio que esta capa no tenía la
        // secuencia (una fila de otra capa, o una lectura obsoleta).
        app.message = Some(t("msg-shortcut-nothing-to-unbind"));
        return;
    }
    let Some(dir) = config::user_config_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let section = write.section;
    let chords = write.chords.clone();
    let command = write.command.clone();
    let res = tokio::task::spawn_blocking(move || {
        config::persist_keymap_unbind(&dir, section, &chords, &command)
    })
    .await;
    app.message = Some(match res {
        // `w.changed` es la verdad del ESCRITOR (releída bajo su lock) sobre
        // si algo se quitó; `write.outcome` es la del DOOR, leída de la
        // config en memoria antes del `spawn_blocking`. Si el fichero cambió
        // justo en ese hueco (otro proceso, una edición a mano) `w.changed`
        // sigue siendo cierto — no se inventa un cambio que no ocurrió — pero
        // el TEXTO de `outcome` puede describir un mapa que ya no es el de
        // disco: la misma ventana que `rebind_dry_run` ya documenta para el
        // bind (el escritor toma el lock del fichero, esto no).
        Ok(Ok(w)) if w.changed => norte_frontend::shortcuts::unbind_outcome_message(
            &write.outcome,
            &painted,
            norte_i18n::active(),
        ),
        Ok(Ok(_)) => t("msg-shortcut-nothing-to-unbind"),
        Ok(Err(e)) => ta(
            "msg-settings-save-failed",
            &[("error", &io_error_category(&e))],
        ),
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_keymap_unbind no terminó");
            t("msg-settings-save-crashed")
        }
    });
}
/// Resuelve el preset (flag > config > default) y pliega las capas de
/// keymap (ADR 0007) para las TRES pantallas (browse, viewer, dialog — H1
/// T2). El error tipado ([`KeymapsError`], #73) vive en `crate::app`
/// junto a su categoría Fluent.
///
/// # Errors
///
/// [`KeymapsError`] si el preset pedido no existe o si una capa no se puede
/// plegar. Tipado y no `anyhow` a propósito: su categoría Fluent vive junto al
/// error, y el editor de atajos hace un ensayo con esta misma función para
/// decidir si una tecla se puede ligar — necesita el motivo, no un texto.
pub fn build_keymaps(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
) -> Result<(Effective, Effective, Effective), KeymapsError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let presets = presets();
    let (_, preset) = presets
        .iter()
        .find(|(n, _)| *n == preset_name)
        .ok_or_else(|| KeymapsError::UnknownPreset {
            name: preset_name.to_owned(),
            available: presets
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", "),
        })?;
    let invalid = |e: crate::keymap::KeymapError| KeymapsError::Invalid {
        detail: e.to_string(),
    };
    let browse_known = known_commands(Screen::Browse);
    let browse = Effective::build_for(preset, &cfg.keymap_layers, &browse_known, Screen::Browse)
        .map_err(invalid)?;
    let viewer_known = known_commands(Screen::Viewer);
    let viewer = Effective::build_for(preset, &cfg.keymap_layers, &viewer_known, Screen::Viewer)
        .map_err(invalid)?;
    // Screen::Dialog fusiona `[dialog] ∪ [global]` (ADR 0006/H1 T1):
    // `build_for_impl` valida TODO el efectivo fusionado contra
    // `known_commands`, así que un binding GLOBAL (p. ej. `ctrl+c →
    // app.quit`) se validaría como `UnknownCommand` si solo pasáramos
    // `DIALOG_COMMANDS`. La UNIÓN con `COMMANDS` es la opción simple (T1 lo
    // deja elegido): inofensiva porque cada overlay ALLOWLISTEA solo sus
    // `dialog.*` soportados (`app::dialog_action` y las resoluciones ad hoc
    // de este módulo) y descarta cualquier otro comando resuelto.
    //
    // K3c: esa unión vive ahora en `known_commands`, porque la puerta del
    // editor de atajos tiene que pasarle al cargador EXACTAMENTE el mismo set
    // que se lo pasó aquí — con uno más estrecho, el ensayo del rebind
    // rechazaría un binding global que carga perfectamente.
    let dialog_known = known_commands(Screen::Dialog);
    let dialog = Effective::build_for(preset, &cfg.keymap_layers, &dialog_known, Screen::Dialog)
        .map_err(invalid)?;
    Ok((browse, viewer, dialog))
}

/// K3c: el editor de atajos, conducido por el mismo camino que las teclas —
/// [`shortcuts_key`] — y llevado hasta el disco y de vuelta.
///
/// El test que importa es el de ida y vuelta completa: `reload_config` aplica
/// TODO o NADA, así que una escritura que produjese una capa inválida dejaría
/// el mapa viejo en su sitio, el editor diría «guardado» y la tecla nueva no
/// haría nada. Eso no se ve en ningún test que se quede en la puerta.
#[cfg(test)]
mod shortcuts_editor_tests {
    use super::{Maps, ShortcutsKeyOutcome, build_keymaps, plan_rebind, shortcut_rows};
    use crate::app::Pane;
    use crate::app::Shortcuts;
    use crate::config::{self, Layer, Layers};
    use crate::keymap::Screen;
    use crate::keymap::{Effective, parse_chord};
    use crate::overlays::close_stale_overlays;
    use crate::paste::route_paste;
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_frontend::shortcuts::PlanError;

    /// Un directorio de configuración vacío como capa de USUARIO: el primer
    /// rebind de una instalación nueva, que es el caso que `split_at` puede
    /// modelar mal en silencio.
    fn cfg_en(dir: &std::path::Path) -> (Layers, config::LoadedConfig) {
        let layers = Layers {
            dirs: vec![(dir.to_path_buf(), Layer::User)],
        };
        let cfg = config::load(&layers).expect("una capa vacía carga");
        (layers, cfg)
    }

    fn maps(cfg: &config::LoadedConfig) -> (Effective, Effective, Effective) {
        build_keymaps(cfg, None).expect("los tres mapas del preset activo")
    }

    fn chord(s: &str) -> crate::keymap::Chord {
        parse_chord(s).expect("chord")
    }

    fn app_vacia() -> super::App {
        let d = norte_proto::VPath::parse("file:///x").expect("wire de test");
        super::App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// Sitúa el cursor en la fila de `command` en `screen` y devuelve el
    /// editor listo para capturar.
    fn editor_en(
        browse: &Effective,
        viewer: &Effective,
        dialog: &Effective,
        screen: Screen,
        command: &str,
    ) -> Shortcuts {
        let rows = shortcut_rows(&Maps {
            browse,
            viewer,
            dialog,
        });
        let idx = rows
            .iter()
            .position(|r| r.screen == screen && r.command == command)
            .expect("la fila del comando existe");
        let mut sc = Shortcuts::new(rows);
        for _ in 0..idx {
            sc.down();
        }
        assert_eq!(
            sc.selected().map(|r| r.command.as_str()),
            Some(command),
            "el cursor está donde el test cree"
        );
        sc
    }

    /// EL camino entero: capturar, pasar la puerta, escribir, RECARGAR como lo
    /// hace el watcher, y comprobar que la tecla hace otra cosa.
    ///
    /// Sobre una tecla que el PRESET ya bindea, que es donde `append_keymap`
    /// habría cargado, validado y no disparado nunca: si la puerta devolviese
    /// la lista equivocada, este test seguiría escribiendo un fichero legal y
    /// la última línea fallaría.
    #[test]
    fn una_captura_confirmada_se_escribe_carga_y_la_tecla_cambia() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let f5 = chord("f5");
        let before = browse
            .bindings_all_seq()
            .into_iter()
            .find(|(seq, _, _)| *seq == [f5])
            .map(|(_, run, _)| run.to_owned())
            .expect("el preset activo bindea F5");
        assert_ne!(before, "pane.mkdir", "si no, el test no prueba nada");

        let mut sc = editor_en(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        assert!(sc.is_capturing());
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::F(5));
        let (screen, command, seq) = sc.confirmable().expect("F5 se puede reasignar");
        let command = command.to_owned();
        let seq = seq.to_vec();

        let w = plan_rebind(&cfg, None, screen, &seq, &command).expect("la puerta deja pasar");
        assert_eq!(
            w.list,
            config::KeymapList::Prepend,
            "un append no pisaría al preset"
        );
        config::persist_keymap_bind(dir.path(), w.section, w.list, &w.chords, &w.command)
            .expect("el escritor escribe");

        // Lo que hace el watcher: recargar la config y reconstruir los mapas.
        // `reload_config` es todo-o-nada, así que un fichero que no cargase se
        // vería aquí como un `Err` — y en la TUI, como un mapa viejo intacto.
        let (_, cfg2) = cfg_en(dir.path());
        drop(layers);
        let (browse2, _, _) = maps(&cfg2);
        assert!(
            browse2.single_chord_runs(f5, "pane.mkdir"),
            "la tecla nueva hace lo que el editor dijo"
        );

        // Y el desligado la devuelve al preset — el motivo de que c1 escribiera
        // `persist_keymap_unbind`: un editor que solo añade no arregla nada.
        // Los mismos argumentos que arma `unbind_shortcut` a partir de la fila.
        let row_seq: Vec<String> = seq.iter().map(ToString::to_string).collect();
        let removed = config::persist_keymap_unbind(dir.path(), w.section, &row_seq, &command)
            .expect("quita");
        assert!(removed.changed, "había algo que quitar");
        let (_, cfg3) = cfg_en(dir.path());
        let (browse3, _, _) = maps(&cfg3);
        assert!(
            browse3.single_chord_runs(f5, &before),
            "sin la capa del usuario vuelve a mandar el preset"
        );
    }

    /// A paste cannot bind a chord (#143): a capture answers ONE physical
    /// key, and a paste is never that — not even a one-character paste,
    /// which crossterm hands the router as `Event::Paste`, never as the
    /// `Event::Key` a keystroke would be. It gets the same outcome a
    /// hostile keystroke gets there (`hostile_key`, `msg-shortcut-not-
    /// bindable`), not a chord silently bound to whatever it pasted.
    #[test]
    fn a_paste_while_capturing_a_chord_is_rejected_not_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let sc = editor_en(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        let mut app = app_vacia();
        app.shortcuts = Some(sc);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        super::shortcuts_key(
            app.shortcuts.as_mut().expect("open"),
            &m,
            KeyModifiers::NONE,
            KeyCode::Enter,
        );
        assert!(app.shortcuts.as_ref().expect("open").is_capturing());

        route_paste(&mut app, "p");

        assert!(
            app.shortcuts.as_ref().expect("still open").is_capturing(),
            "the capture must still be waiting — a paste cannot have satisfied it"
        );
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-shortcut-not-bindable").as_str()),
            "same message a hostile keystroke gets there"
        );
    }

    /// Una tecla sagrada (§12) capturada NO es confirmable — y la puerta, si
    /// alguien la saltase, tampoco la deja pasar. Las dos mitades, porque el
    /// veredicto de la captura es una comodidad y la puerta es la garantía.
    #[test]
    fn una_tecla_sagrada_no_es_confirmable_ni_pasa_la_puerta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let mut sc = editor_en(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Tab);
        assert!(
            sc.capture().and_then(|c| c.verdict()).is_some(),
            "el veredicto se ve ANTES de confirmar"
        );
        assert!(sc.confirmable().is_none(), "Tab no se vende");
        assert!(matches!(
            plan_rebind(&cfg, None, Screen::Browse, &[chord("tab")], "pane.mkdir"),
            Err(PlanError::Door(_))
        ));
        // Y con un veredicto de rechazo en pantalla, Enter no pide escribir.
        assert!(matches!(
            super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter),
            ShortcutsKeyOutcome::Confirm
        ));
        assert!(
            sc.confirmable().is_none(),
            "y `confirm_shortcut` no tiene nada que escribir"
        );
    }

    /// `Esc` cancela la captura en las dos fases — por eso es el único chord
    /// que este editor no puede capturar, y por eso la pantalla lo dice.
    /// `Enter`, en cambio, SÍ se captura: en la fase de espera es una tecla
    /// como otra cualquiera y solo confirma después.
    #[test]
    fn esc_cancela_y_enter_si_se_puede_capturar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        let mut sc = editor_en(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Esc);
        assert!(!sc.is_capturing(), "esc cancela la espera");
        // Y con la captura cerrada, `Esc` cierra la pantalla.
        assert!(matches!(
            super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Esc),
            ShortcutsKeyOutcome::Close
        ));

        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        assert_eq!(
            sc.capture().map(|c| c.seq().to_vec()),
            Some(vec![chord("enter")]),
            "el primer Enter abre la captura y el segundo ES la tecla"
        );
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Esc);
        assert!(!sc.is_capturing(), "esc cancela también con veredicto");
    }

    /// `Ctrl+C` NO cierra norte mientras se captura: es un chord que un
    /// converso de CUA quiere ligar, y en modo captura el lector pulsa a
    /// ciegas porque el editor se lo ha pedido. Fuera de la captura sigue
    /// siendo la salida de emergencia de siempre.
    #[tokio::test]
    async fn ctrl_c_capturando_es_un_chord_y_no_una_salida() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        let mut app = app_vacia();
        app.shortcuts = Some(editor_en(
            &browse,
            &viewer,
            &dialog,
            Screen::Browse,
            "pane.mkdir",
        ));
        super::on_shortcuts_key(&mut app, &cfg, None, &m, KeyModifiers::NONE, KeyCode::Enter).await;
        super::on_shortcuts_key(
            &mut app,
            &cfg,
            None,
            &m,
            KeyModifiers::CONTROL,
            KeyCode::Char('c'),
        )
        .await;
        assert!(!app.quit, "capturando, ctrl+c es la tecla que se captura");
        assert_eq!(
            app.shortcuts
                .as_ref()
                .and_then(Shortcuts::capture)
                .map(|c| c.seq().to_vec()),
            Some(vec![chord("ctrl+c")])
        );
        // Cancelada la captura, vuelve a ser la salida global.
        super::on_shortcuts_key(&mut app, &cfg, None, &m, KeyModifiers::NONE, KeyCode::Esc).await;
        super::on_shortcuts_key(
            &mut app,
            &cfg,
            None,
            &m,
            KeyModifiers::CONTROL,
            KeyCode::Char('c'),
        )
        .await;
        assert!(app.quit);
    }

    /// Un codepoint peligroso solo puede llegar PEGADO (norte no activa
    /// bracketed paste), y no se captura: `parse_chord` lo aceptaría y el
    /// escritor lo dejaría crudo en el `keymap.toml` del usuario.
    #[test]
    fn un_codepoint_peligroso_pegado_no_se_captura() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let m = Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        };
        let mut sc = editor_en(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        super::shortcuts_key(&mut sc, &m, KeyModifiers::NONE, KeyCode::Enter);
        assert!(matches!(
            super::shortcuts_key(
                &mut sc,
                &m,
                KeyModifiers::NONE,
                // U+202E RIGHT-TO-LEFT OVERRIDE.
                KeyCode::Char('\u{202e}')
            ),
            ShortcutsKeyOutcome::NotBindable
        ));
        assert!(
            sc.capture().expect("sigue capturando").is_waiting(),
            "no se capturó nada"
        );
    }

    /// Un modal que llega SOLO (una aprobación de policy, una colisión) se
    /// queda el teclado: el editor deja de pedir una tecla a ciegas, y el
    /// brazo del modal lo retira entero.
    #[test]
    fn un_modal_que_llega_solo_abandona_la_captura() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let mut app = app_vacia();
        let mut sc = editor_en(&browse, &viewer, &dialog, Screen::Browse, "pane.mkdir");
        assert!(sc.begin_capture());
        app.shortcuts = Some(sc);
        app.pending_approvals
            .push_back(norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                paths_total: 0,
                ttl_ms: 60_000,
            });
        app.open_next_pending();
        assert!(app.modal.is_some());
        assert!(
            !app.shortcuts.as_ref().is_some_and(Shortcuts::is_capturing),
            "ya no se pide una tecla a ciegas"
        );
        close_stale_overlays(&mut app);
        assert!(app.shortcuts.is_none(), "y el brazo del modal lo retira");
    }

    /// El editor lista lo que la hoja de referencia no puede: un comando que
    /// ninguna tecla pulsa. Sin esa fila, «cómo pulso X» no tiene respuesta.
    #[test]
    fn un_comando_sin_tecla_tiene_fila() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_layers, cfg) = cfg_en(dir.path());
        let (browse, viewer, dialog) = maps(&cfg);
        let rows = shortcut_rows(&Maps {
            browse: &browse,
            viewer: &viewer,
            dialog: &dialog,
        });
        assert!(
            rows.iter().any(|r| !r.is_bound()),
            "el preset activo no bindea TODO lo que la TUI despacha"
        );
        for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
            assert!(
                rows.iter().any(|r| r.screen == screen),
                "{screen:?} tiene filas"
            );
        }
        // Y las filas del viewer no ofrecen comandos de pane: ligar `pane.copy`
        // ahí escribiría una tecla que no hace nada en el viewer.
        assert!(
            !rows
                .iter()
                .any(|r| r.screen == Screen::Viewer && !r.is_bound() && r.command == "pane.copy"),
            "el viewer no despacha comandos de pane"
        );
    }

    /// Cada mensaje de esta pantalla existe en los DOS locales: lo que se ve
    /// en la barra si falta una clave es el id crudo.
    #[test]
    fn las_claves_de_la_pantalla_existen_en_ambos_locales() {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            for id in [
                "shortcuts-title",
                "shortcuts-hint",
                "shortcuts-capture-hint",
                "shortcuts-capture-note",
                "shortcuts-confirm-hint",
                "shortcuts-no-key",
                "shortcuts-refused-preset",
                "msg-shortcut-bound",
                "msg-shortcut-unbound",
                "msg-shortcut-unbound-cleared",
                "msg-shortcut-nothing-to-unbind",
                "msg-shortcut-not-bindable",
                "shortcuts-row-global",
            ] {
                assert_ne!(norte_i18n::t_in(lang, id), id, "falta {id} en {lang:?}");
            }
        }
    }
}
