//! norte-gui — SPIKE M5 (hito 1) → GUI-a Task 4: **dual-pane navegable**.
//!
//! Sobre el scaffold del spike (ventana GPUI que listaba UN dir real del
//! daemon) GUI-a construye un file manager ortodoxo mínimo: DOS panes, foco
//! conmutable, cursor, `cd`/enter/parent, quick search por tipeo, teclado y
//! ratón — todo READ-ONLY (`fs.list`). La lógica de pane (cursor + quick
//! search) NO vive aquí: es [`norte_frontend::PaneState`], el mismo modelo puro
//! que consumirá la TUI (GUI-a T3). La GUI solo: (1) pinta el estado de los dos
//! panes con GPUI, (2) traduce input a mutaciones del `PaneState`, (3) lanza los
//! `fs.list` contra el daemon y cruza el resultado al hilo de render.
//!
//! # API de GPUI usada (rev f14fea9)
//!
//! - **Input de teclado**: `div().track_focus(&handle).on_key_down(cx.listener(
//!   Self::on_key))`. El root pide foco en `new` (`window.focus(&handle, cx)`),
//!   así los `KeyDownEvent` llegan al root. `KeyDownEvent.keystroke.key` es el
//!   nombre de la tecla (`"up"`, `"tab"`, `"escape"`, `"a"`…, ver
//!   `input::key_to_action`); `key_char` el carácter realmente tecleado
//!   (fidelidad de layout/shift). Descubierto en
//!   `crates/gpui/examples/{focus_visible,input}.rs`.
//! - **Ratón**: `div().on_mouse_down(MouseButton::Left, cx.listener(...))` con
//!   `MouseDownEvent.click_count` (1 = foco+cursor, 2 = `cd`). `on_scroll_wheel`
//!   con `ScrollWheelEvent.delta` mueve el cursor. Se usa `on_mouse_down` (no
//!   `on_click`) para no exigir un `.id()` estable por fila.
//! - **async → UI**: `cx.spawn` + `this.update` + `cx.notify()`, con un
//!   `tokio::oneshot` que cruza desde el hilo tokio del `RemoteBackend` (ver
//!   `backend_task.rs`).
#![forbid(unsafe_code)]

use gpui::{
    App, Bounds, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, ScrollDelta, ScrollWheelEvent, SharedString, Styled, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use std::path::PathBuf;

use norte_frontend::{PaneState, nav::Mode};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_theme::{FileKind, Role, Theme};

mod backend_task;
mod input;
mod theme_map;

use backend_task::PaneListOutcome;
use input::{Action, key_to_action};

/// Badge local que prefija un nombre alterado en el display (regla 1 / spec §6:
/// display siempre lossy y MARCADO). No hay `ui.rs` de la TUI aquí, así que la
/// GUI define su propio marcador; el criterio de «hostil» sí es el compartido
/// ([`norte_frontend::display_name`] devuelve el bool).
const HOSTILE_BADGE: &str = "⚠";

/// Salto de página (↑↓ de 10 en 10) — orden de magnitud de una pantalla del
/// spike; el fill/scroll fino es optimización posterior.
const PAGE: usize = 10;

// Colores del chrome del dual-pane (constantes locales; el color por TIPO de
// archivo sí sale del tema, ver `entry_color`). El theming completo del chrome
// por rol es fuera de alcance del spike.
const BG: u32 = 0x121212;
const FG: u32 = 0xffffff;
const PANE_BG: u32 = 0x1e1e1e;
const PANE_BG_FOCUS: u32 = 0x252526;
const HEADER_BG: u32 = 0x2d2d2d;
const BORDER_FOCUS: u32 = 0x3b82f6;
const BORDER_UNFOCUS: u32 = 0x3a3a3a;
const SEL_BG: u32 = 0x264f78;
const ERR_FG: u32 = 0xf87171;
const QUICK_FG: u32 = 0xfbbf24;

/// El *root view*: dos panes navegables, cuál tiene el foco, el tema cacheado y
/// el socket del daemon (para relistar en cada `cd`).
struct NorteGui {
    /// Los dos panes (modelo puro compartido con la TUI).
    panes: [PaneState; 2],
    /// Pane con el foco (0|1): recibe el input de teclado.
    focus: usize,
    /// Texto del quick search por pane, PARALELO a `PaneState` solo para
    /// pintar la línea `/{query}` al pie: `PaneState` expone los índices
    /// visibles pero no el texto de la consulta, así que la GUI lo espeja al
    /// dirigir `quick_char`/`quick_backspace` (fuente de verdad = el pane; esto
    /// es solo su reflejo para render).
    query: [String; 2],
    /// Último error de carga por pane (banner), o `None` si el listado está OK.
    errors: [Option<String>; 2],
    /// Contador de generación por pane: cada `cd` (incluido el `begin_loading`)
    /// lo incrementa y captura el valor. Un `fs.list` en vuelo lleva su
    /// generación; al llegar solo se aplica si sigue vigente. Así un cd viejo
    /// (A) que termina TARDE no pisa el listado de un cd nuevo (B) lanzado en el
    /// mismo pane — robusto incluso ante A→B→A, que un simple compare de `dir`
    /// no distingue (ver `generation_is_current`).
    generation: [u64; 2],
    /// Tema cacheado UNA vez (parsea TOML; no es gratis por-frame).
    theme: Theme,
    /// Socket del daemon; cada `cd` abre una conexión fresca (ver
    /// `backend_task`).
    socket: PathBuf,
    /// Handle de foco del root: sin él los `KeyDownEvent` no llegan.
    focus_handle: FocusHandle,
}

impl NorteGui {
    /// Construye el view con los dos panes en el mismo directorio inicial
    /// (`NORTE_DIR` o el `cwd`) y lanza sus dos cargas. Toma el foco de la
    /// ventana para recibir teclado. Si la config no resuelve, nace con ambos
    /// panes en error (sin lanzar cargas), nunca panic.
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = Theme::preset_default();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        match backend_task::LoadConfig::from_env() {
            Ok(cfg) => {
                let backend_task::LoadConfig { socket, dir } = cfg;
                let mut gui = Self {
                    panes: [
                        PaneState::new(dir.clone(), Vec::new()),
                        PaneState::new(dir.clone(), Vec::new()),
                    ],
                    focus: 0,
                    query: [String::new(), String::new()],
                    errors: [None, None],
                    generation: [0, 0],
                    theme,
                    socket,
                    focus_handle,
                };
                gui.cd(0, dir.clone(), cx);
                gui.cd(1, dir, cx);
                gui
            }
            Err(e) => {
                // INVARIANTE: "file:///" es un VPath raíz siempre válido; solo
                // es un placeholder para pintar el banner de error.
                let placeholder = VPath::parse("file:///").expect("'file:///' es un VPath válido");
                Self {
                    panes: [
                        PaneState::new(placeholder.clone(), Vec::new()),
                        PaneState::new(placeholder, Vec::new()),
                    ],
                    focus: 0,
                    query: [String::new(), String::new()],
                    errors: [
                        Some(format!("config inválida: {e}")),
                        Some(format!("config inválida: {e}")),
                    ],
                    generation: [0, 0],
                    theme,
                    socket: PathBuf::new(),
                    focus_handle,
                }
            }
        }
    }

    /// Cambia el directorio de `pane` a `dir`: lo marca como cargando y lanza el
    /// `fs.list` en segundo plano; el resultado llega por `oneshot` y se aplica
    /// en el hilo de render (ordenado con `sort_entries`). Un error deja el pane
    /// vacío con banner, jamás panic.
    fn cd(&mut self, pane: usize, dir: VPath, cx: &mut Context<Self>) {
        // Nueva generación para ESTE cd: invalida cualquier list en vuelo del
        // pane (un resultado anterior llegará con generación vieja y se
        // descartará en el apply).
        self.generation[pane] = self.generation[pane].wrapping_add(1);
        let generation = self.generation[pane];

        self.panes[pane].begin_loading(dir.clone());
        self.errors[pane] = None;
        self.query[pane].clear();

        let (tx, rx) = tokio::sync::oneshot::channel();
        backend_task::spawn_list(self.socket.clone(), dir, pane, generation, tx);

        cx.spawn(async move |this, cx| {
            let result = rx.await;
            this.update(cx, |view, cx| {
                let Ok(PaneListOutcome {
                    pane,
                    generation,
                    dir,
                    outcome,
                }) = result
                else {
                    // El emisor se soltó sin enviar (hilo abortado): raro.
                    return;
                };
                // GUARD de generación: si el pane ya está en un cd más nuevo,
                // este resultado es stale y se DESCARTA (no pisa el listado
                // vigente ni resetea cursor/filtro).
                if !generation_is_current(view.generation[pane], generation) {
                    if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
                        eprintln!(
                            "[norte-gui] pane {pane}: resultado stale (gen {generation} != {}) descartado",
                            view.generation[pane],
                        );
                    }
                    return;
                }
                match outcome {
                    Ok(entries) => {
                        let mut entries = entries;
                        norte_frontend::sort_entries(&mut entries);
                        view.panes[pane].set_listing(dir, entries);
                        view.errors[pane] = None;
                        view.query[pane].clear();
                        if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
                            eprintln!(
                                "[norte-gui] pane {pane} aplicó listado: {} entradas, cursor={}",
                                view.panes[pane].entries().len(),
                                view.panes[pane].cursor(),
                            );
                        }
                    }
                    Err(msg) => {
                        // Limpia el estado de carga (dir vacío) + banner.
                        view.panes[pane].set_listing(dir, Vec::new());
                        view.errors[pane] = Some(msg);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Maneja una tecla en el pane con foco (ver `input::key_to_action` para el
    /// mapeo puro; aquí solo la EJECUCIÓN, que sí depende del estado vivo del
    /// pane).
    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let f = self.focus;
        let quick_active = self.panes[f].quick_visible().is_some();
        let mods = ks.modifiers;

        // Un imprimible con Ctrl/Alt/Super es un atajo, no filtro: descártalo.
        let action = match key_to_action(&ks.key, quick_active) {
            Action::Char(_) if mods.control || mods.alt || mods.platform => Action::None,
            other => other,
        };

        match action {
            Action::None => return,
            Action::Tab => self.focus = 1 - self.focus,
            Action::Up => {
                if quick_active {
                    self.panes[f].quick_up();
                } else {
                    self.panes[f].cursor_up();
                }
            }
            Action::Down => {
                if quick_active {
                    self.panes[f].quick_down();
                } else {
                    self.panes[f].cursor_down();
                }
            }
            // Home/End/Page saltan el cursor REAL: sin efecto con el filtro
            // abierto (la selección vive en el quick, que solo tiene ↑↓).
            Action::Home => {
                if !quick_active {
                    self.panes[f].home();
                }
            }
            Action::End => {
                if !quick_active {
                    self.panes[f].end();
                }
            }
            Action::PageUp => {
                if !quick_active {
                    self.panes[f].page_up(PAGE);
                }
            }
            Action::PageDown => {
                if !quick_active {
                    self.panes[f].page_down(PAGE);
                }
            }
            Action::Char(c) => {
                // Fidelidad: el carácter REALMENTE tecleado (respeta shift y
                // layout) va en `key_char`; si no está, cae al de `key`.
                let ch = single_char(ks.key_char.as_deref()).unwrap_or(c);
                if !quick_active {
                    self.panes[f].quick_start(Mode::Filter);
                    self.query[f].clear();
                }
                self.panes[f].quick_char(ch);
                self.query[f].push(ch);
            }
            Action::Backspace => {
                if quick_active {
                    self.panes[f].quick_backspace();
                    self.query[f].pop();
                } else if let Some(parent) = self.panes[f].dir().parent() {
                    self.cd(f, parent, cx);
                }
            }
            Action::Esc => {
                self.panes[f].quick_cancel();
                self.query[f].clear();
            }
            Action::Enter => {
                let target = {
                    let pane = &mut self.panes[f];
                    if pane.quick_visible().is_some() {
                        pane.quick_confirm();
                    }
                    pane.selected()
                        .filter(|e| e.kind == EntryKind::Dir)
                        .map(|e| e.path.clone())
                };
                self.query[f].clear();
                if let Some(dir) = target {
                    self.cd(f, dir, cx);
                }
            }
        }
        if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
            let nf = self.focus;
            let pane = &self.panes[nf];
            let sel = pane
                .selected()
                .and_then(|e| e.path.file_name())
                .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
                .unwrap_or_default();
            eprintln!(
                "[norte-gui] key={:?} -> action={:?} | focus={nf} dir={} cursor={} quick={:?} sel={sel:?}",
                ks.key,
                action,
                pane.dir(),
                pane.cursor(),
                self.query[nf],
            );
        }
        cx.notify();
    }

    /// Click en una fila: foco a ese pane + cursor a esa fila; doble-click sobre
    /// un directorio hace `cd`.
    fn on_row_click(
        &mut self,
        pane: usize,
        idx: usize,
        dir_target: Option<VPath>,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        self.focus = pane;
        // Un click cancela cualquier filtro (el cursor real vuelve a mandar) y
        // se posa en `idx`: sin `set_cursor` en PaneState, se emula home + page.
        self.panes[pane].quick_cancel();
        self.query[pane].clear();
        self.panes[pane].home();
        self.panes[pane].page_down(idx);
        if click_count >= 2 {
            if let Some(dir) = dir_target {
                self.cd(pane, dir, cx);
            }
        }
        cx.notify();
    }

    /// Rueda del ratón sobre un pane: le da el foco y mueve el cursor.
    fn on_pane_scroll(&mut self, pane: usize, delta: ScrollDelta, cx: &mut Context<Self>) {
        self.focus = pane;
        let y = scroll_y(delta);
        if y > 0.0 {
            self.panes[pane].cursor_up();
        } else if y < 0.0 {
            self.panes[pane].cursor_down();
        }
        cx.notify();
    }

    /// Pinta una columna (un pane).
    fn render_pane(&self, i: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = &self.panes[i];
        let focused = self.focus == i;

        let (path_txt, path_hostile) = norte_frontend::path_display(pane.dir());
        let header = if path_hostile {
            format!("{HOSTILE_BADGE} {path_txt}")
        } else {
            path_txt
        };

        // La entrada resaltada = la seleccionada (respeta el filtro). Se compara
        // por path (único dentro de un listado) para no depender del índice.
        let sel_path = pane.selected().map(|e| e.path.clone());

        // Filas visibles: solo `quick_visible()` si filtrando, si no todo.
        let rows: Vec<_> = match pane.quick_visible() {
            Some(idxs) => idxs
                .iter()
                .map(|&j| {
                    let e = &pane.entries()[j];
                    let hl = sel_path.as_ref() == Some(&e.path);
                    self.render_row(i, j, e, hl, cx)
                })
                .collect(),
            None => pane
                .entries()
                .iter()
                .enumerate()
                .map(|(j, e)| {
                    let hl = sel_path.as_ref() == Some(&e.path);
                    self.render_row(i, j, e, hl, cx)
                })
                .collect(),
        };

        let mut col = div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_2()
            .border_color(if focused {
                rgb(BORDER_FOCUS)
            } else {
                rgb(BORDER_UNFOCUS)
            })
            .bg(if focused {
                rgb(PANE_BG_FOCUS)
            } else {
                rgb(PANE_BG)
            });

        // Cabecera: el path saneado del dir.
        col = col.child(
            div()
                .px(px(4.0))
                .py(px(2.0))
                .bg(rgb(HEADER_BG))
                .truncate()
                .child(SharedString::from(header)),
        );

        // Estado transitorio: cargando / error / vacío.
        if pane.loading() {
            col = col.child(div().px(px(4.0)).child(SharedString::from("cargando…")));
        } else if let Some(err) = &self.errors[i] {
            col = col.child(
                div()
                    .px(px(4.0))
                    .text_color(rgb(ERR_FG))
                    .child(SharedString::from(format!("error: {err}"))),
            );
        } else if pane.entries().is_empty() {
            col = col.child(
                div()
                    .px(px(4.0))
                    .child(SharedString::from("(directorio vacío)")),
            );
        }

        // Lista de entradas.
        col = col.child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .overflow_hidden()
                .children(rows),
        );

        // Línea `/{query}` al pie si el quick search está activo. La query la
        // tecleó el usuario, pero con IME/paste puede llegar con bidi/invisibles
        // crudos (review encoding BAJA): se sanea AL PINTAR, una sola vez, con
        // el mismo criterio que `display_name` (`is_terminal_hazard`) — nunca
        // se guarda saneada porque el filtro compara contra el nombre real.
        if pane.quick_visible().is_some() {
            let query_display: String = self.query[i]
                .chars()
                .map(|c| {
                    if norte_encoding::is_terminal_hazard(c) {
                        '\u{FFFD}'
                    } else {
                        c
                    }
                })
                .collect();
            col = col.child(
                div()
                    .px(px(4.0))
                    .py(px(1.0))
                    .bg(rgb(HEADER_BG))
                    .text_color(rgb(QUICK_FG))
                    .child(SharedString::from(format!("/{query_display}"))),
            );
        }

        // Rueda + click en zona vacía de la columna dan foco a este pane.
        col.on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _w, cx| {
            this.on_pane_scroll(i, ev.delta, cx);
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                this.focus = i;
                cx.notify();
            }),
        )
    }

    /// Pinta una fila: badge hostil + nombre saneado + indicador de tipo,
    /// coloreado por tipo de archivo; fondo resaltado si es la selección.
    fn render_row(
        &self,
        pane: usize,
        idx: usize,
        entry: &Entry,
        highlighted: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bytes = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
        let label = row_label(bytes, entry.kind);
        let color = entry_color(&self.theme, entry);
        let dir_target = (entry.kind == EntryKind::Dir).then(|| entry.path.clone());

        let mut row = div()
            .px(px(4.0))
            .py(px(1.0))
            .text_color(color)
            .truncate()
            .child(SharedString::from(label));
        if highlighted {
            row = row.bg(rgb(SEL_BG));
        }
        row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                this.on_row_click(pane, idx, dir_target.clone(), ev.click_count, cx);
            }),
        )
    }
}

/// El primer carácter de `s` si `s` es exactamente uno.
fn single_char(s: Option<&str>) -> Option<char> {
    let s = s?;
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// ¿Sigue vigente el resultado de un `fs.list`? Solo si su generación coincide
/// con la vigente del pane: un cd más nuevo ya incrementó el contador, dejando
/// stale a cualquier list en vuelo anterior. Comparar la generación (y no el
/// `dir`) es robusto ante A→B→A — dos cds distintos al MISMO dir tienen
/// generaciones distintas, un dir-compare los confundiría.
#[must_use]
fn generation_is_current(current: u64, incoming: u64) -> bool {
    current == incoming
}

/// Componente vertical del delta de scroll (líneas o píxeles → f32).
fn scroll_y(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Pixels(p) => f32::from(p.y),
        ScrollDelta::Lines(p) => p.y,
    }
}

/// Etiqueta completa de una fila: badge hostil (si `display_name` marcó el
/// nombre) + nombre saneado + indicador de tipo. Extraída como función PURA
/// (sin GPUI) para poder testearla contra el corpus hostil de `norte-testkit`
/// sin levantar ventana/GPU (review encoding GAP).
#[must_use]
fn row_label(bytes: &[u8], kind: EntryKind) -> String {
    let (name, hostile) = norte_frontend::display_name(bytes);
    format!(
        "{}{}{}",
        if hostile {
            format!("{HOSTILE_BADGE} ")
        } else {
            String::new()
        },
        name,
        kind_indicator(kind),
    )
}

/// Indicador de tipo: «/» dir, «@» symlink, nada para archivo, «?» para lo demás.
fn kind_indicator(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File => "",
        EntryKind::Other => "?",
    }
}

/// Mapea el tipo de nodo del protocolo al tipo de archivo del tema. `File`/
/// `Other` → `Regular` (proto no trae `st_mode`; el theming por extensión sigue
/// aplicando encima).
fn file_kind_of(kind: EntryKind) -> FileKind {
    match kind {
        EntryKind::Dir => FileKind::Dir,
        EntryKind::Symlink => FileKind::Symlink,
        EntryKind::File | EntryKind::Other => FileKind::Regular,
    }
}

/// Color de texto de una entrada: extensión > kind > rol `regular`, con blanco
/// de fallback. `Theme::file_style` ya resuelve la prioridad.
fn entry_color(theme: &Theme, entry: &Entry) -> gpui::Rgba {
    let name = entry.path.file_name().map_or(&b""[..], Segment::as_bytes);
    let style = theme.file_style(name, file_kind_of(entry.kind));
    let fg = style.fg.or_else(|| theme.style(Role::Regular).fg);
    fg.map_or_else(|| rgb(0xffffff), theme_map::to_gpui_rgba)
}

impl Render for NorteGui {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(FG))
            .gap(px(2.0))
            .p(px(4.0))
            .child(self.render_pane(0, cx))
            .child(self.render_pane(1, cx))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| NorteGui::new(window, cx)),
        )
        .expect("no se pudo abrir la ventana GPUI");

        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::{generation_is_current, row_label};
    use norte_proto::EntryKind;

    /// El guard de generación: dos cds sobre el mismo pane (A luego B) →
    /// B incrementa la generación vigente; cuando A (viejo) llega tarde, su
    /// generación ya no coincide y su resultado se descarta, aunque A→B→A
    /// vuelva al mismo dir.
    #[test]
    fn resultado_stale_se_descarta_por_generacion() {
        // gen vigente del pane tras dos cds: 2. El outcome del primer cd (gen 1)
        // es stale.
        let vigente = 2u64;
        assert!(
            !generation_is_current(vigente, 1),
            "un resultado de gen 1 con el pane en gen 2 debe descartarse"
        );
        // El outcome del cd vigente (gen 2) sí se aplica.
        assert!(
            generation_is_current(vigente, 2),
            "el resultado de la generación vigente se aplica"
        );
        // A→B→A: el segundo A es gen 3, no la 1 del primero; el primer A (gen 1)
        // sigue stale aunque el dir coincida.
        let tras_a_b_a = 3u64;
        assert!(!generation_is_current(tras_a_b_a, 1));
        assert!(generation_is_current(tras_a_b_a, 3));
    }

    /// `row_label` sobre TODO el corpus hostil de `norte-testkit`: la etiqueta
    /// pintada jamás lleva un carácter de `is_terminal_hazard` crudo (bidi,
    /// control, invisible) — el saneado de `display_name` cubre la fila
    /// entera, no solo el nombre suelto (review encoding GAP, sin GPU).
    #[test]
    fn row_label_nunca_deja_hazards_crudos_del_corpus_hostil() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let label = row_label(&fixture.bytes, EntryKind::File);
            assert!(
                !label.chars().any(norte_encoding::is_terminal_hazard),
                "{}: row_label dejó un hazard crudo en {label:?}",
                fixture.id,
            );
        }
    }

    /// El badge hostil (`⚠`) aparece cuando `display_name` marca el nombre
    /// como alterado — no se pierde la señal de "esto no es exactamente el
    /// byte original" al extraer `row_label`.
    #[test]
    fn row_label_badge_cuando_display_name_es_hostil() {
        for fixture in norte_testkit::corpus::hostile_names() {
            let (_, hostile) = norte_frontend::display_name(&fixture.bytes);
            let label = row_label(&fixture.bytes, EntryKind::File);
            assert_eq!(
                label.starts_with(super::HOSTILE_BADGE),
                hostile,
                "{}: badge debe aparecer sii display_name es hostil (label={label:?})",
                fixture.id,
            );
        }
    }
}
