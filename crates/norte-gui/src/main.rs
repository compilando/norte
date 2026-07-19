//! norte-gui — SPIKE M5 (hito 1). Binario GPUI que pinta un dir REAL del daemon.
//!
//! - T2: ventana GPUI mínima (validó render en Linux).
//! - T3: `theme_map::to_gpui_rgba` (mapeo norte-theme → color GPUI, con test).
//! - T4: conectar al daemon + pintar el listado (criterio 1 del ADR).
//! - **T5 (este): color por tipo de archivo con `norte-theme`** (criterio 3:
//!   el theming compartido cruza a la GPU sin retrabajo — `Theme::file_style`
//!   y `theme_map::to_gpui_rgba` son los MISMOS que usaría cualquier otro
//!   frontend; solo cambia el tipo de color de salida).
//!
//! API descubierta contra los ejemplos del rev pineado de GPUI
//! (`crates/gpui/examples/{hello_world,testing}.rs` en zed-industries/zed
//! @ f14fea9bf3c93797d5161f7440ed418655bc6c57):
//!
//! - **`App`**: contexto global; abre ventanas (`open_window`), activa la app.
//! - **`Window`** + root view (`Render`): el contenido es una *entity* (`NorteGui`)
//!   cuyo `render` devuelve un árbol de elementos (`div()…child(…)`).
//! - **async → UI**: `cx.spawn(async move |this, cx| { … this.update(cx, |view, cx|
//!   { …; cx.notify(); }) })` (patrón de `examples/testing.rs`). El `RemoteBackend`
//!   (tokio) corre en un hilo aparte con su runtime; el resultado llega por un
//!   `oneshot` que se `.await`-ea dentro del `cx.spawn`. Así el render nunca se
//!   bloquea esperando al daemon (ver `backend_task.rs`).

use gpui::{
    App, Bounds, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

use norte_proto::{Entry, EntryKind};
use norte_theme::{FileKind, Role, Theme};

mod backend_task;
mod theme_map;

/// Estado de la carga del listado. La ventana re-renderiza cuando cambia
/// (`cx.notify()` tras el `update`).
enum LoadState {
    /// Aún conectando/listando contra el daemon.
    Connecting,
    /// Listado recibido del daemon.
    Loaded(Vec<Entry>),
    /// Falló (daemon caído, `NotFound`, config inválida…). NUNCA panic:
    /// el mensaje se pinta en la ventana.
    Failed(String),
}

/// El *root view* de la ventana: mantiene el estado de la carga y lo pinta.
struct NorteGui {
    state: LoadState,
    /// Tema cacheado UNA vez por ventana (T5, criterio 3): `Theme::preset_default()`
    /// parsea TOML — no es gratis, así que se calcula aquí, no por-frame
    /// por-entrada en `render`. El tema es estático durante la vida del spike
    /// (no hay reload en caliente; eso es fuera de alcance de M5 hito 1).
    theme: Theme,
}

impl NorteGui {
    /// Construye el view en estado «conectando» y arranca la carga en segundo
    /// plano. La config (socket/dir) sale de entorno; si ni eso resuelve, se
    /// nace directamente en `Failed` (sin lanzar el hilo).
    fn new(cx: &mut Context<Self>) -> Self {
        let theme = Theme::preset_default();
        match backend_task::LoadConfig::from_env() {
            Ok(cfg) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                backend_task::spawn_load(cfg, tx);
                // GPUI async → UI: espera el resultado del hilo tokio y re-renderiza.
                cx.spawn(async move |this, cx| {
                    let outcome = rx.await;
                    this.update(cx, |view, cx| {
                        view.state = match outcome {
                            Ok(Ok(entries)) => LoadState::Loaded(entries),
                            Ok(Err(msg)) => LoadState::Failed(msg),
                            // El emisor se soltó sin enviar (hilo abortado): raro.
                            Err(_) => LoadState::Failed("carga interrumpida".into()),
                        };
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
                Self {
                    state: LoadState::Connecting,
                    theme,
                }
            }
            Err(e) => Self {
                state: LoadState::Failed(format!("config inválida: {e}")),
                theme,
            },
        }
    }
}

/// Nombre de una entrada para display: bytes del último segmento → UTF-8 lossy →
/// saneado por la fuente ÚNICA (`mask_terminal_hazards`). Regla 1: los bytes
/// no-UTF8 no rompen (van a `U+FFFD`), nunca se asume UTF-8 ni se hace `unwrap`.
fn display_name(entry: &Entry) -> String {
    let bytes = entry
        .path
        .file_name()
        .map_or(&b""[..], norte_proto::Segment::as_bytes);
    let lossy = String::from_utf8_lossy(bytes);
    norte_encoding::mask_terminal_hazards(&lossy)
}

/// Indicador de tipo minimalista (el color por tipo llega en T5): «/» dir,
/// «@» symlink, nada para archivo, «?» para lo demás.
fn kind_indicator(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Dir => "/",
        EntryKind::Symlink => "@",
        EntryKind::File => "",
        EntryKind::Other => "?",
    }
}

/// Mapea el tipo de nodo del protocolo (`norte_proto::EntryKind`) al tipo de
/// archivo del tema (`norte_theme::FileKind`). NO son 1:1 (T5, criterio 3):
///
/// - `Dir` → `FileKind::Dir` y `Symlink` → `FileKind::Symlink`: mapean 1:1.
/// - `File` y `Other` → `FileKind::Regular`: proto (`Entry`) no trae permisos
///   ni `st_mode`, así que no hay forma de distinguir `Executable`/`Fifo`/
///   `Socket`/`BlockDevice`/`CharDevice` desde un `fs.list` — esos `FileKind`
///   del tema son inalcanzables desde este mapeo (quedarían para un M5 futuro
///   si `Entry` gana esos metadatos). `Other` (device/socket/fifo real, o un
///   `EntryKind` de protocolo futuro que este cliente no conoce) tampoco
///   distingue, y cae al mismo `Regular` que un archivo normal — el theming
///   por EXTENSIÓN (`[files.ext]`, prioridad más alta en `Theme::file_style`)
///   sigue aplicando igual, así que un `.rs` colorea aunque el kind sea
///   `Regular`.
fn file_kind_of(kind: EntryKind) -> FileKind {
    match kind {
        EntryKind::Dir => FileKind::Dir,
        EntryKind::Symlink => FileKind::Symlink,
        EntryKind::File | EntryKind::Other => FileKind::Regular,
    }
}

/// Color de texto para una entrada (T5, criterio 3: el theming cruza a la GPU).
///
/// `Theme::file_style` ya resuelve la prioridad extensión > kind > rol
/// `regular` con fallback interno (`files.rs`/`theme.rs`), así que aquí solo
/// queda: (1) mapear `entry.kind` a `FileKind`, (2) pedir el `Style`, (3) si
/// trae `fg`, convertirlo a `gpui::Rgba` con `theme_map::to_gpui_rgba`. Si NO
/// trae `fg` (el preset `default` no colorea el rol `regular` — un archivo
/// sin extensión conocida cae ahí, ver `presets/default.toml`), se prueba el
/// color de rol base (`Role::Regular`) y, si tampoco hay, el blanco por
/// defecto que ya pintaba el contenedor antes de T5.
fn entry_color(theme: &Theme, entry: &Entry) -> gpui::Rgba {
    let name = entry
        .path
        .file_name()
        .map_or(&b""[..], norte_proto::Segment::as_bytes);
    let style = theme.file_style(name, file_kind_of(entry.kind));
    let fg = style.fg.or_else(|| theme.style(Role::Regular).fg);

    // Diagnóstico opcional (T5 step 3): confirma sin verificación visual que
    // `style_for`/`file_style` devuelve colores distintos por tipo/extensión.
    if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
        eprintln!(
            "[norte-gui] '{}' kind={:?} filekind={:?} fg={:?}",
            String::from_utf8_lossy(name),
            entry.kind,
            file_kind_of(entry.kind),
            fg
        );
    }

    fg.map_or_else(|| rgb(0xffffff), theme_map::to_gpui_rgba)
}

/// Una fila de la lista: nombre saneado + indicador de tipo, coloreada por
/// tipo de archivo (T5).
fn entry_row(theme: &Theme, entry: &Entry) -> impl IntoElement {
    let texto: SharedString =
        format!("{}{}", display_name(entry), kind_indicator(entry.kind)).into();
    let color = entry_color(theme, entry);
    div().py(px(2.0)).text_color(color).child(texto)
}

impl Render for NorteGui {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Contenedor: columna vertical con fondo oscuro; el texto BASE es
        // blanco (mismo valor que `entry_color` usa de fallback), pero cada
        // fila de entrada se sobreescribe con su color por tipo (T5).
        let base = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xffffff))
            .p(px(12.0))
            .gap(px(2.0));

        match &self.state {
            LoadState::Connecting => base.child(SharedString::from("conectando…")),
            LoadState::Failed(msg) => base.child(SharedString::from(format!("error: {msg}"))),
            LoadState::Loaded(entries) if entries.is_empty() => {
                base.child(SharedString::from("(directorio vacío)"))
            }
            LoadState::Loaded(entries) => {
                let theme = &self.theme;
                base.children(entries.iter().map(|entry| entry_row(theme, entry)))
            }
        }
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(500.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(NorteGui::new),
        )
        .expect("no se pudo abrir la ventana GPUI");

        cx.activate(true);
    });
}
