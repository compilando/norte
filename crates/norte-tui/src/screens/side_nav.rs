//! Los cuatro paneles de navegación laterales y el popup que los precede:
//! árbol, procesos, sidebar de sitios y el popup de historial/hotlist/
//! volúmenes.
//!
//! Los cuatro tienen la misma forma —resuelven la tecla contra el contexto
//! `dialog` del keymap, la filtran por el ALLOWLIST de su overlay, y devuelven
//! un [`Cd`] porque confirmar es navegar por el camino de `cd` normal— y los
//! cuatro vivían en el root del binario `ntc`, un crate DISTINTO de esta lib.
//!
//! `side_nav` y no `nav` porque [`crate::nav`] ya existe y es otra cosa (el
//! modelo del popup); esto es quien lee sus teclas.
//!
//! Los dos bloques de rustdoc de [`on_places_key`] y [`on_nav_popup_key`]
//! estaban APILADOS sobre `on_tree_key` en `main.rs`, tres doc-comments
//! seguidos delante de una sola función: un movimiento anterior dejó atrás la
//! documentación de las otras dos. Aquí vuelve cada bloque a su función, sin
//! tocar una palabra.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_i18n::ta;
use norte_proto::Error;

use crate::app::{
    ALLOW_NAV_POPUP, ALLOW_PLACES, App, NavPopup, NavPopupKind, PickerAction, Trail,
    detail_for_bar, error_category, error_message, io_error_category, volume_items,
};
use crate::config;
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};
use crate::navigate::{Cd, cd, cd_in};

/// Fetches `Backend::volumes` for `pane`'s side and opens/refreshes the
/// volumes popup (design §D). Opening from `pane.select-drive*` and
/// re-opening after the in-popup unfiltered toggle are the SAME operation —
/// a fresh frozen snapshot for the requested mode — so both call this. A
/// fetch error surfaces as the usual status message and leaves whatever
/// popup was already open alone, same pattern as `Command::AppExtensions`
/// on a failed `plugins_list`.
pub async fn open_drive_popup(app: &mut App, backend: &Backend, pane: usize, include_pseudo: bool) {
    match backend.volumes(include_pseudo).await {
        Ok(volumes) => {
            let enc = app.panes[pane].name_encoding();
            let items = volume_items(&volumes, enc);
            app.open_volumes_popup(pane, include_pseudo, items);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Teclas del árbol (#136): mismo reparto y mismo allowlist que el sidebar.
///
/// `⏎` sobre una rama la despliega o la pliega; `dialog.confirm` con la rama ya
/// abierta MANDA el listado ahí, que es para lo que se abre un árbol. `Esc` y
/// `Tab` sueltan el teclado y dejan el panel abierto — cerrarlo es `pane.tree`,
/// la misma SEGUNDA pulsación que el sidebar: abrir cualquiera de los dos ya
/// les da el teclado.
pub async fn on_tree_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled;
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    if !ALLOW_PLACES.contains(&cmd.as_str()) {
        return Cd::Cancelled;
    }
    // El cromo de la aplicación antes que nada: no es de este panel, y por eso
    // no lo decide este panel (`App::panel_chrome_command`, uno para los tres).
    if app.panel_chrome_command(&cmd) {
        return Cd::Cancelled;
    }
    match cmd.as_str() {
        "dialog.up" => {
            if let Some(t) = app.tree_mut() {
                t.up();
            }
        }
        "dialog.down" => {
            if let Some(t) = app.tree_mut() {
                t.down();
            }
        }
        "dialog.toggle-enabled" => {
            if let Some(t) = app.tree_mut() {
                t.toggle();
            }
        }
        // `Esc` suelta el teclado, y `Tab` también: la misma regla que el
        // sidebar y el panel de procesos. Ninguno de los dos CIERRA el árbol
        // —eso es `pane.tree`—, y abrir una columna lateral no puede costarte
        // la tecla con la que se cambia de panel toda la vida.
        "dialog.cancel" | "dialog.pane" | "pane.switch" => app.return_keys_to_panes(),
        // El anillo pasa al panel de AL LADO, que es lo que `Tab` no hace: la
        // tecla con la que se recorre la pantalla tiene que funcionar también
        // dentro del panel del que se quiere salir.
        "layout.focus-next" => app.layout_focus(1),
        "layout.focus-prev" => app.layout_focus(-1),
        // El ancho del árbol, por lo mismo que el del sidebar (#244 M1).
        "layout.grow" => app.layout_resize(1),
        "layout.shrink" => app.layout_resize(-1),
        "pane.tree" => app.toggle_tree(),
        // Y las de los otros paneles, igual que en el sidebar.
        "layout.places" => app.toggle_places(),
        "layout.preview" => app.toggle_preview(),
        "layout.processes" => app.toggle_processes(),
        "layout.metadata" => app.toggle_metadata(),
        "layout.log" => app.toggle_log(),
        "dialog.confirm" => {
            let dest = app.tree().and_then(crate::tree::Tree::selected);
            if let Some(dir) = dest {
                // Desplegar Y navegar: quien pulsa Enter sobre una rama quiere
                // ver qué hay dentro, y verlo en el listado es la respuesta
                // completa.
                if let Some(t) = app.tree_mut() {
                    t.expand();
                }
                return cd(app, backend, events, dir).await;
            }
        }
        _ => {}
    }
    Cd::Cancelled
}

/// Teclas del panel de procesos (#243): resuelve por keymap (pantalla
/// `dialog`) y filtra por [`crate::app::ALLOW_PROCESSES`] — misma disciplina de
/// única-fuente que el resto de paneles con teclado (#24).
///
/// Síncrona y sin backend: cancelar es soltarle el token a una task que este
/// proceso ya observa, no una llamada.
pub fn on_processes_key(app: &mut App, resolver: &mut Resolver, mods: KeyModifiers, code: KeyCode) {
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
    // El despacho vive en el `App` (biblioteca) para que un test pueda meter
    // una tecla de verdad por él; aquí queda la resolución, que es lo que este
    // binario tiene y el `App` no.
    if let Some(msg) = app.processes_command(&cmd) {
        app.message = Some(msg);
    }
}

/// Teclas del sidebar de sitios (L3), resueltas por el contexto `dialog`.
///
/// El sidebar no navega por su cuenta: Enter devuelve una ruta y el `cd` va al
/// LISTADO enfocado, por el mismo camino que cualquier otro. Es lo que hace
/// que abrirlo no cambie a dónde van las operaciones.
pub async fn on_places_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    if !ALLOW_PLACES.contains(&cmd.as_str()) {
        return Cd::Cancelled; // fuera del allowlist de este panel: inerte
    }
    // El cromo de la aplicación, antes que lo de este panel: mismo embudo que
    // el árbol y el panel de procesos.
    if app.panel_chrome_command(&cmd) {
        return Cd::Cancelled;
    }
    match cmd.as_str() {
        "dialog.up" => app.places_up(),
        "dialog.down" => app.places_down(),
        // `places_toggle_fold` deja pedidas las unidades si la sección quedó
        // desplegada; las sirve el bucle.
        "dialog.toggle-enabled" => app.places_toggle_fold(),
        // `Esc` suelta el teclado, y `Tab` también. Ninguno CIERRA el panel:
        // cerrarlo es `layout.places`.
        //
        // Lo de `Tab` no es simetría por gusto: sin él, abrir el sidebar
        // dejaba muerta la tecla con la que se cambia de panel toda la vida.
        // `pane.switch` no está en el vocabulario `dialog.*` y el panel se come
        // lo que no esté en su allowlist. Sale a los listados sin cambiar de
        // panel, así que el SIGUIENTE `Tab` hace lo de siempre y la tecla
        // significa una sola cosa: «a la región siguiente», con el sidebar
        // contando como región.
        "dialog.cancel" | "dialog.pane" | "pane.switch" => app.return_keys_to_panes(),
        // El anillo pasa al panel de AL LADO, que es lo que `Tab` no hace: la
        // tecla con la que se recorre la pantalla tiene que funcionar también
        // dentro del panel del que se quiere salir.
        "layout.focus-next" => app.layout_focus(1),
        "layout.focus-prev" => app.layout_focus(-1),
        // El ancho del sidebar, que es el ÚNICO camino por el que se puede
        // cambiar: el llamante de `layout_resize` pasa siempre un listado
        // visible, así que la rama de `Size::Fixed` no la alcanzaba nadie
        // (#244 M1).
        "layout.grow" => app.layout_resize(1),
        "layout.shrink" => app.layout_resize(-1),
        // Y `layout.places` con el teclado DENTRO cierra: es la SEGUNDA
        // pulsación, porque abrir este panel ya le da el teclado.
        "layout.places" => app.toggle_places(),
        // Las teclas de los otros paneles siguen abriendo lo suyo: estar en
        // una columna lateral no puede dejar sin efecto la que abre la de al
        // lado.
        "layout.preview" => app.toggle_preview(),
        "layout.processes" => app.toggle_processes(),
        "layout.metadata" => app.toggle_metadata(),
        "layout.log" => app.toggle_log(),
        "pane.tree" => app.toggle_tree(),
        // `⏎` sobre una CABECERA pliega o despliega su sección, como en el
        // árbol de al lado. Antes no hacía nada: `activate()` devuelve `None`
        // para una cabecera, así que Enter sobre «Unidades» era inerte y
        // plegar era Espacio y solo Espacio. Enter es el gesto que se prueba
        // primero sobre algo que se abre, y los dos paneles laterales deben
        // contestarlo igual.
        //
        // Sobre una unidad o un favorito sigue NAVEGANDO, que es lo que Enter
        // significa sobre una hoja.
        "dialog.confirm" => {
            if app.places_cursor_on_header() {
                app.places_toggle_fold();
            } else if let Some(path) = app.places_activate() {
                let pane = app.focus();
                return cd_in(app, backend, events, pane, path, Trail::Record).await;
            }
        }
        _ => {}
    }
    Cd::Cancelled
}

/// Sirve la petición de unidades que haya pendiente, si la hay.
///
/// El ÚNICO consumidor de [`App::places_wants_drives`]: lo drena el run loop
/// una vez por vuelta y el arranque una vez antes de entrar en él, para que el
/// primer frame ya salga con la lista puesta.
///
/// Existe porque `host.volumes` es I/O y quien enciende la bandera —abrir el
/// sidebar, desplegar su sección, montar una disposición que ya lo trae— no
/// siempre tiene un backend delante. Cuando cada uno de esos sitios pedía los
/// volúmenes por su cuenta, faltaban justo en los que nadie recordó.
pub async fn drain_places_drives(app: &mut App, backend: &Backend) {
    if std::mem::take(&mut app.places_wants_drives) {
        refresh_places_drives(app, backend).await;
    }
}

/// Pide los volúmenes al host y los deja en el sidebar.
///
/// Lo llama [`drain_places_drives`] y nadie más: un sidebar con reloj sería la
/// regla de suspensión del ADR 0058 rota
/// desde el primer frame, y `host.volumes` no es gratis (monta y consulta
/// espacio en cada filesystem).
///
/// Un fallo NO vacía la lista que hubiera: lo que se veía sigue siendo lo
/// último que el host dijo, y el error sale por la barra como cualquier otro.
pub async fn refresh_places_drives(app: &mut App, backend: &Backend) {
    let Some(id) = app.places_slot() else {
        return;
    };
    match backend.volumes(false).await {
        Ok(res) => {
            if let Some(state) = app.panes.places_mut(id) {
                state.set_drives(&res);
            }
        }
        Err(e) => {
            app.message = Some(ta(
                "gui-msg-volumes-failed",
                &[("error", &error_category(&e))],
            ));
        }
    }
}

/// Teclas del popup de navegación (historial `Alt+↓` / hotlist `Ctrl+D` /
/// volúmenes `Alt+F1`/`Alt+F2`, design §D); `ctrl+c` conserva su salida
/// global, hardcodeado ANTES de nada. Con `name_input` activo (el `a` de
/// hotlist abre un campo para el nombre del favorito) los
/// imprimibles/backspace se capturan como editor de texto RAW — H1 T2
/// decisión: NO es un comando `dialog.*`, es entrada libre, se queda
/// hardcodeado. Fuera de `name_input`, la tecla resuelve contra el contexto
/// `dialog` del keymap (H1 T2, issue #24); `add`/`remove` los filtra el
/// ALLOWLIST de este overlay a `kind == Hotlist` (el historial no tiene nada
/// que nombrar ni borrar — mismo criterio que antes de H1) y
/// `toggle-enabled` a `kind == Volumes` (el toggle "mostrar todo" del design
/// §D). Enter sobre un item válido NAVEGA por el flujo de cd normal, contra
/// [`crate::app::NavPopup::target_pane`] y no `app.focus()` — historial y
/// hotlist congelan el foco ahí, pero `-left`/`-right` congelan un LADO fijo
/// (design §D); si el cd desde el HISTORIAL falla con `NotFound`, la entrada
/// se retira (spec 2026-07-18) — la de hotlist y volúmenes NO (hotlist es
/// config del usuario y un volumen no se retira porque un cd puntual falle).
pub async fn on_nav_popup_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(popup) = &mut app.nav_popup else {
        return Cd::Cancelled;
    };
    let kind = popup.kind;
    // SHIFT pasa (mayúsculas llegan como Char+SHIFT); ctrl/alt no escriben.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if popup.name_input.is_some() {
        match code {
            KeyCode::Char(c) if plain => {
                if let Some(input) = &mut popup.name_input {
                    input.push(c);
                }
            }
            KeyCode::Backspace if plain => {
                if let Some(input) = &mut popup.name_input {
                    input.pop();
                }
            }
            KeyCode::Esc => popup.name_input = None,
            KeyCode::Enter => {
                let name = popup.name_input.take().unwrap_or_default();
                // Input vacío = cancela (plan T5): no hay favorito sin nombre.
                if !name.is_empty() {
                    hotlist_add(app, &name).await;
                }
            }
            _ => {}
        }
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Secuencia en curso, o tecla ligada a algo que esta build no corre
        // (K1 T4): ignorar y reiniciar el estado de resolución.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    // H1 T3: el MISMO allowlist que consume cada hint generado
    // (`hints::DialogHints::build`, campos `nav_list`/`nav_volumes`) — una
    // sola fuente para dispatch, aunque el hint IMPRESO es más estrecho por
    // kind. Cubre los tres kinds (History es un subconjunto: `add`/`remove`
    // los filtra el guard `kind == Hotlist` de más abajo, `toggle-enabled` el
    // guard `kind == Volumes`).
    if !ALLOW_NAV_POPUP.contains(&cmd.as_str()) {
        return Cd::Cancelled; // fuera del allowlist de este overlay: inerte
    }
    match cmd.as_str() {
        "dialog.up" => {
            app.nav_popup_input(PickerAction::Up);
        }
        "dialog.down" => {
            app.nav_popup_input(PickerAction::Down);
        }
        "dialog.cancel" => {
            app.nav_popup_input(PickerAction::Cancel);
        }
        "dialog.add" if kind == NavPopupKind::Hotlist => {
            app.nav_popup_open_name_input();
        }
        "dialog.remove" if kind == NavPopupKind::Hotlist => {
            if let Some(name) = app.nav_popup_selected_hotlist_name() {
                hotlist_remove(app, &name).await;
            }
        }
        // design §D: the in-popup unfiltered toggle. Same operation as
        // opening the popup, just with the flag flipped and the SAME target
        // pane — `open_drive_popup` re-fetches and replaces the snapshot.
        "dialog.toggle-enabled" if kind == NavPopupKind::Volumes => {
            let refresh = app
                .nav_popup
                .as_ref()
                .map(|p| (p.target_pane(), !p.include_pseudo()));
            if let Some((pane, want)) = refresh {
                open_drive_popup(app, backend, pane, want).await;
            }
        }
        "dialog.confirm" => {
            // The target pane is frozen on the popup, not `app.focus()`:
            // history/hotlist froze it AT the focus (so this is the same
            // value), but `-left`/`-right` froze a fixed SIDE (design §D).
            // Read it BEFORE `nav_popup_input` may close the popup below.
            let pane = app
                .nav_popup
                .as_ref()
                .map_or_else(|| app.focus(), NavPopup::target_pane);
            // Confirm sobre un item inválido/vacío es no-op (el popup sigue).
            if let Some(path) = app.nav_popup_input(PickerAction::Confirm) {
                let outcome = cd_in(app, backend, events, pane, path.clone(), Trail::Record).await;
                if kind == NavPopupKind::History && matches!(&outcome, Cd::Failed(Error::NotFound))
                {
                    // El dir ya no existe: fuera del historial. La barra ya
                    // muestra el error normal del cd fallido.
                    app.history[pane].remove(&path);
                }
                return outcome;
            }
        }
        _ => {} // fuera del allowlist de este overlay (o kind): inerte
    }
    Cd::Cancelled
}

/// `config::user_config_dir()` o el MISMO io `NotFound` que fabrica
/// `persist_ui_theme` sin entorno (CI pelada): la barra lo pinta como
/// `err-not-found` vía categoría (#73), clave existente y razonable.
fn user_config_dir_io() -> std::io::Result<std::path::PathBuf> {
    config::user_config_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "sin directorio de config de usuario",
        )
    })
}

/// Persiste el favorito `name` = cwd del pane con foco en el `norte.toml`
/// del USUARIO (`spawn_blocking`, regla 2 — `persist_hotlist_add` es
/// bloqueante por contrato). Solo si el disco fue bien se refresca la copia
/// en `App` (consistencia con disco) y sale `msg-hotlist-saved`; un fallo
/// io sale por categoría y la copia NO se toca.
async fn hotlist_add(app: &mut App, name: &str) {
    let target = app.focused().dir().clone();
    let wire = target.to_wire();
    let n = name.to_owned();
    // Al PERFIL activo si lo hay: los favoritos son de un espacio de trabajo,
    // y escribirlos en la capa del usuario mientras un perfil también los fija
    // los deja tapados (ADR 0079).
    let destino = app.config_write_dir();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = destino.map_or_else(user_config_dir_io, Ok)?;
        config::persist_hotlist_add(&dir, &n, &wire)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_saved(name, target);
            // El name lo tecleó el usuario, pero un PASTE puede colar
            // bidi/controles: por `detail_for_bar` como todo detalle (#73).
            app.message = Some(ta("msg-hotlist-saved", &[("name", &detail_for_bar(name))]));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Un panic al persistir es un bug NUESTRO: que reviente visible
        // (criterio del binario, mismo que `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Retira el favorito `name` del `norte.toml` de la capa que se esté editando
/// —el PERFIL activo si lo hay, si no la del usuario— (`spawn_blocking`, regla
/// 2). Mismo contrato de consistencia que [`hotlist_add`].
async fn hotlist_remove(app: &mut App, name: &str) {
    let n = name.to_owned();
    let destino = app.config_write_dir();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = destino.map_or_else(user_config_dir_io, Ok)?;
        config::persist_hotlist_remove(&dir, &n)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_removed(name);
            app.message = Some(ta(
                "msg-hotlist-removed",
                &[("name", &detail_for_bar(name))],
            ));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}
