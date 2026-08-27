//! Los cinco selectores de lista con cursor: tema, conexiones, disposición y
//! columnas, más el `[ui].theme` que el primero aplica.
//!
//! Los cuatro son la misma cosa con distinto contenido —una lista con cursor
//! que resuelve la tecla contra el contexto `dialog` del keymap y la filtra
//! por el ALLOWLIST de su overlay— y los cuatro vivían en el root del binario
//! `ntc`, un crate DISTINTO de esta lib.
//!
//! Parecía haber un ciclo con [`crate::screens::settings`] —el picker de
//! columnas «persiste como `persist_setting`» y el overlay de ajustes «abre el
//! de tema»— y no lo hay: las dos referencias son menciones en comentarios, no
//! llamadas. Un ciclo entre módulos del MISMO crate sería legal en Rust de
//! todas formas, así que los dos ficheros salieron en un solo commit.
//!
//! El rustdoc de [`on_layout_picker_key`] estaba APILADO sobre
//! `on_connections_picker_key` en `main.rs`, dos doc-comments seguidos delante
//! de una sola función: un movimiento anterior dejó atrás la documentación de
//! la otra. Aquí vuelve a la suya, sin tocar una palabra.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_i18n::{t, ta};

use crate::app::{
    ALLOW_COLUMNS, ALLOW_PICKER, App, PickerAction, detail_for_bar, io_error_category,
    theme_error_category,
};
use crate::config;
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};

/// Resuelve `[ui].theme` (preset o ruta) y lo aplica al `App`; ante error
/// degrada al preset por defecto y avisa (ADR 0020). El frontend no revienta
/// por un tema malo.
pub fn apply_theme(app: &mut App, cfg: &config::LoadedConfig) {
    let depth = crate::theme::detect_depth();
    match crate::theme::resolve(cfg.common.ui_theme.as_deref(), depth) {
        Ok(theme) => app.theme = theme,
        Err(e) => {
            app.theme = crate::theme::TuiTheme::default();
            // Por categoría Fluent (#73): jamás el Display del OS ni el
            // diagnóstico crudo (el spec puede venir de un `./.norte` ajeno).
            app.message = Some(theme_error_category(&e));
        }
    }
}

/// Traduce las teclas del popup de tema a una acción de dominio (la lógica
/// vive en `App`, testeable) resolviendo contra el contexto `dialog` del
/// keymap (H1 T2, issue #24 — rebindeable). `ctrl+c` conserva su salida
/// global, hardcodeado ANTES de resolver, como los demás overlays. `F9`
/// cierra el picker como atajo ESPECÍFICO de este overlay (no es un binding
/// `dialog.*` del preset): se mantiene hardcodeado. Al confirmar, PERSISTE
/// la elección en el `norte.toml` del usuario (ADR 0020), sin bloquear el
/// runtime.
pub async fn on_theme_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if mods.is_empty() && code == KeyCode::F(9) {
        app.theme_picker_input(PickerAction::Cancel);
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Sin semántica de secuencia definida para overlays (T2), y lo mismo
        // para una tecla ligada a algo que esta build no corre (K1 T4):
        // ignorar y reiniciar el estado de resolución.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`) — una sola fuente para dispatch y
    // footer. El match sigue siendo exhaustivo por defensa en profundidad.
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este overlay: inerte
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return, // ya filtrado por ALLOW_PICKER; inalcanzable en la práctica
    };
    // El nombre a persistir se toma ANTES de que Confirm cierre el popup.
    let confirmed = (action == PickerAction::Confirm)
        .then(|| {
            app.theme_picker
                .as_ref()
                .and_then(|p| p.selected().map(String::from))
        })
        .flatten();
    app.theme_picker_input(action);
    if let Some(name) = confirmed {
        // I/O en spawn_blocking: el runtime jamás se bloquea (regla 2).
        //
        // Al directorio del PERFIL activo si lo hay, y no siempre al del
        // usuario: el perfil está por encima, así que un tema escrito abajo
        // queda TAPADO por el que fije el perfil. Se guardaba, la barra decía
        // «config recargada», y la pantalla no cambiaba de color (ADR 0079).
        let n = name.clone();
        let Some(dir) = app.config_write_dir() else {
            app.message = Some(t("msg-settings-no-config-dir"));
            return;
        };
        match tokio::task::spawn_blocking(move || config::persist_ui_theme_to(&dir, &n)).await {
            Ok(Ok(path)) => {
                // El path deriva de XDG_CONFIG_HOME/APPDATA (entorno):
                // saneado como cualquier detalle (#73).
                app.message = Some(ta(
                    "msg-theme-saved",
                    &[
                        ("name", &name),
                        ("path", &detail_for_bar(&path.display().to_string())),
                    ],
                ));
            }
            Ok(Err(e)) => {
                // El tema YA se aplicó (sesión); solo no se pudo guardar. A
                // la barra va la CATEGORÍA, jamás el Display del OS (#73).
                app.message = Some(ta(
                    "msg-theme-save-failed",
                    &[("error", &io_error_category(&e))],
                ));
            }
            // Un panic en el write es un bug nuestro: que no tumbe la TUI.
            Err(_) => {}
        }
    }
}

/// Teclas del selector de conexiones (#140): mismo reparto y mismo allowlist
/// que el de disposiciones. Devuelve la URL elegida, si se confirmó.
pub fn on_connections_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<String> {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    let chord = chord_from_crossterm(mods, code)?;
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return None;
        }
        Resolution::Reset => return None,
    };
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return None;
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return None,
    };
    app.connections_picker_input(action)
}

/// Teclas del selector de disposición: resuelve por keymap (pantalla
/// `dialog`) y filtra por [`ALLOW_PICKER`], el MISMO allowlist que el selector
/// de tema — los dos son una lista con cursor que no muta nada fuera de sí
/// misma, así que Enter sí dispara. `ctrl+c` conserva su salida global,
/// hardcodeado antes de resolver, y `F9` cierra como en el de tema.
///
/// No es `async` y no persiste nada: elegir una disposición vale para esta
/// sesión, y lo que la fija entre arranques es `[ui] layout` en tu config.
/// Guardarla al vuelo convertiría una prueba en un cambio permanente.
pub fn on_layout_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if mods.is_empty() && code == KeyCode::F(9) {
        app.layout_picker = None;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este overlay: inerte
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return, // ya filtrado por ALLOW_PICKER; inalcanzable en la práctica
    };
    app.layout_picker_input(action);
}

/// Teclas del selector de PERFILES (ADR 0079). Misma disciplina que el de
/// disposiciones: resuelve por keymap en la pantalla `dialog` y filtra por el
/// mismo allowlist, con `ctrl+c` conservando su salida global antes de nada.
pub fn on_profile_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este overlay: inerte
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return,
    };
    app.profile_picker_input(action);
}

/// Teclas del picker de columnas (#108 7a): resuelve por keymap (pantalla
/// `dialog`) y filtra por [`ALLOW_COLUMNS`] — misma disciplina única-fuente
/// que el resto de overlays (#24). `ctrl+c` conserva su salida global,
/// hardcodeado ANTES de resolver, como los demás overlays. Devuelve `true`
/// si un confirm cambió el set de attrs pintado (#117): el run loop
/// re-lista entonces (mismo camino que tras una mutación).
pub async fn on_columns_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> bool {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return false;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return false; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Sin semántica de secuencia definida para overlays (T2), y lo mismo
        // para una tecla ligada a algo que esta build no corre (K1 T4):
        // ignorar y reiniciar el estado de resolución.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return false;
        }
        Resolution::Reset => return false,
    };
    if !ALLOW_COLUMNS.contains(&cmd.as_str()) {
        return false; // fuera del allowlist de este overlay: inerte
    }
    let Some(p) = app.columns_picker.as_mut() else {
        return false;
    };
    match cmd.as_str() {
        "dialog.up" => p.up(),
        "dialog.down" => p.down(),
        "dialog.toggle-enabled" => p.toggle(),
        "dialog.move-up" => p.move_up(),
        "dialog.move-down" => p.move_down(),
        "dialog.sort" => p.sort_current(),
        "dialog.cycle-format" => p.cycle_format(),
        "dialog.cancel" => app.columns_picker = None,
        "dialog.confirm" => {
            let picked = p.finish();
            app.columns_picker = None;
            return apply_picked_columns(app, picked).await;
        }
        _ => {} // ya filtrado por ALLOW_COLUMNS; inalcanzable en la práctica
    }
    false
}

/// Los ids attr CONFIGURADOS de cada pane visible (#117): la huella que
/// decide si un cambio de columnas exige re-listar — los valores attr solo
/// llegan pidiéndolos en `fs.list`, así que un id nuevo con el listado
/// viejo pintaría blanco (ausencia) hasta el próximo cd. La huella ordenada
/// vive en el modelo (una única definición para ambos frontends).
#[must_use]
pub fn pane_attr_ids(app: &App) -> Vec<Vec<String>> {
    // #117-follow-up (review MAJOR-1): huella COMBINADA attr+plugin, única
    // definición en el modelo (`pane_fingerprint`) para ambos frontends —
    // un cambio SOLO de plugins también re-lista (el re-list respawnea el
    // fetch de valores; sin él la columna nueva quedaría en blanco).
    app.panes
        .iter()
        .map(|p| app.columns.pane_fingerprint(p.dir().scheme()))
        .collect()
}

/// Aplica el resultado del picker (#108 7a): sesión primero (settings en
/// memoria + re-sort de TODO pane, `apply_scheme_sort` es no-op donde el
/// spec no cambia), disco después (`config::persist_columns` en
/// `spawn_blocking` — regla 2). A la barra va la CATEGORÍA del error, jamás
/// el Display del SO (#73). Devuelve `true` si el set de attrs pintado de
/// algún pane visible cambió (#117): el caller re-lista entonces por el
/// mismo camino que tras una mutación.
async fn apply_picked_columns(
    app: &mut App,
    picked: norte_frontend::columns_picker::Picked,
) -> bool {
    let attrs_before = pane_attr_ids(app);
    app.columns
        .apply_picked(picked.scheme_target.as_deref(), &picked.ids, picked.sort);
    // #108 7b: los formatos ciclados también EN SESIÓN antes del disco —
    // mismo lockstep (`apply_format` toca el spec retenido que lee
    // `style_for`).
    for (id, fmt) in &picked.formats {
        app.columns.apply_format(id, fmt);
    }
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    let needs_refresh = pane_attr_ids(app) != attrs_before;
    // Al PERFIL activo si lo hay: un perfil está por encima de la capa del
    // usuario, así que escribir ahí lo que el perfil también fija lo deja
    // tapado — guardado y sin efecto (ADR 0079).
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return needs_refresh;
    };
    let ids = picked.ids.clone();
    let scheme = picked.scheme_target.clone();
    let sort = picked.sort;
    let formats = picked.formats.clone();
    let res = tokio::task::spawn_blocking(move || {
        // Todas las escrituras en UNA tarea de fondo, secuenciales sobre el
        // mismo fichero (#108 7b): la lista+sort y después cada formato
        // ciclado — un solo desenlace, un solo toast.
        config::persist_columns(
            &dir,
            scheme.as_deref(),
            &ids,
            config::PersistSort {
                column: match sort.column {
                    norte_frontend::SortColumn::Name => "name",
                    norte_frontend::SortColumn::Size => "size",
                    norte_frontend::SortColumn::Mtime => "mtime",
                    norte_frontend::SortColumn::Extension => "extension",
                },
                descending: sort.dir == norte_frontend::SortDir::Desc,
                dirs_first: sort.dirs_first,
            },
        )?;
        for (id, fmt) in &formats {
            config::persist_column_format(&dir, id, fmt)?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await;
    match res {
        Ok(Ok(())) => app.message = Some(t("msg-columns-saved")),
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Un panic en el write es un bug nuestro: que no tumbe la TUI (misma
        // disciplina que `persist_setting`) — se anuncia y queda traza.
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_columns no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
    needs_refresh
}
