//! norte-gui — SPIKE M5 (hito 1), Task 2: ventana GPUI mínima.
//!
//! Objetivo: abrir UNA ventana nativa con el texto estático «norte-gui». Sin
//! daemon, sin theming, sin listado — eso llega en T3/T4/T5. Aquí solo se valida
//! que el render de GPUI arranca en este entorno Linux (o se registra por qué no,
//! como dato del criterio 4 del spike).
//!
//! API descubierta contra los ejemplos del rev pineado de GPUI
//! (`crates/gpui/examples/hello_world.rs` en zed-industries/zed
//! @ f14fea9bf3c93797d5161f7440ed418655bc6c57):
//!
//! - **`App`** (en los ejemplos `cx: &mut App`): el contexto global de la
//!   aplicación. Se obtiene dentro del callback de `run`. Desde él se abren
//!   ventanas (`open_window`) y se activa la app (`activate`).
//! - **`Window`**: cada ventana nativa. `open_window` recibe unas `WindowOptions`
//!   (posición/tamaño) y una closure constructora del *root view*.
//! - **root view / `Render`**: el contenido de la ventana es una *entity* (aquí
//!   `NorteGui`) que implementa el trait `Render`; su método `render` devuelve un
//!   árbol de elementos (`div()...child(...)`) — el equivalente GPUI a un árbol
//!   DOM con layout flex.
//!
//! `application()` vive en el crate hermano `gpui_platform` (ver Cargo.toml): es
//! quien elige el backend nativo real (`gpui_linux` con wayland/x11 en Linux) y
//! devuelve un `gpui::Application` listo para `.run(...)`.

use gpui::{
    App, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};
use gpui_platform::application;

/// El *root view* de la ventana: una entity con estado mínimo (el texto a pintar)
/// que GPUI vuelve a renderizar cuando cambia. Para el spike el estado es
/// constante — un `SharedString` (string barato de clonar, el tipo de texto de
/// GPUI) con «norte-gui».
struct NorteGui {
    texto: SharedString,
}

impl Render for NorteGui {
    /// Construye el árbol de elementos de la ventana. `div()` es el elemento
    /// contenedor; las llamadas encadenadas son estilo (flex, colores, tamaño) al
    /// modo utility-CSS. `.child(...)` cuelga el texto estático dentro.
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .size_full()
            // Fondo gris oscuro y texto claro: sin theming aún (llega en T5); solo
            // que el texto se lea sobre el fondo.
            .bg(rgb(0x1e1e1e))
            .text_xl()
            .text_color(rgb(0xffffff))
            .child(self.texto.clone())
    }
}

fn main() {
    // `application()` construye el `gpui::Application` con el backend nativo de
    // este OS. `.run(closure)` arranca el run-loop de la plataforma (bloquea la
    // vida de la app) y nos entrega el `App` global una vez inicializado.
    application().run(|cx: &mut App| {
        // Ventana centrada de 500x300 px en la pantalla primaria (`None`).
        let bounds = Bounds::centered(None, size(px(500.0), px(300.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            // Constructor del root view: `cx.new(...)` crea la entity `NorteGui`
            // que GPUI gestionará y renderizará.
            |_window, cx| {
                cx.new(|_cx| NorteGui {
                    texto: "norte-gui".into(),
                })
            },
        )
        .expect("no se pudo abrir la ventana GPUI");

        // Trae la app al frente / la marca como activa (como en los ejemplos).
        cx.activate(true);
    });
}
