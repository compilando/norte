//! Qué hace la VENTANA con cada clave de configuración (ADR 0097, decisión 2).
//!
//! El terminal ya tenía este guarda —`App::desde_config`, en
//! `norte-tui/src/app/profile.rs`, destructura `CommonConfig` sin `..`— y la
//! ventana no. Seis de los hallazgos de la auditoría de paridad de
//! 2026-09-05 son claves que habrían tenido que clasificarse aquí y que en su
//! lugar se quedaron sin leer sin que nadie lo notara, entre ellas
//! `openers.toml`, que es una feature documentada entera.
//!
//! **Sin `..` a propósito.** Un campo nuevo hace que esto no COMPILE, que es
//! más fuerte que un assert: obliga a decidir qué hace la ventana con la
//! clave justo cuando alguien la está añadiendo, en vez de dejar que «no la
//! lee nadie» y «la lee al arrancar» sean indistinguibles.
//!
//! Vive en `norte-ui-host` y no en `norte-gui-tauri` para que lo corra el
//! gate de siempre (`just t` / `just ci-fast`): una clave nueva se añade en
//! `norte-config`, que no toca la ventana, así que un guarda que solo corriera
//! en `gui-ci` se enteraría tarde. Algunas de las claves las lee la cáscara
//! (`norte-gui-tauri/src/startup.rs`) y no el host; se dice en cada grupo.
//!
//! **Esto NO afirma que lo clasificado esté bien.** Dice que está decidido.
//! Las que hoy están decididas como «no la lee» son deuda con nombre, y el
//! plan `docs/superpowers/plans/2026-09-05-paridad-tui-ventana.md` las lista.

/// Toda clave de `CommonConfig` está clasificada por lo que hace la ventana.
#[test]
fn toda_clave_de_config_esta_clasificada_para_la_ventana() {
    let c = norte_config::load(&norte_config::Layers { dirs: Vec::new() }).expect("config vacía");
    let norte_config::CommonConfig {
        // ─── La cáscara las lee al arrancar y viajan en `UiHostOptions`.
        //     `norte-gui-tauri/src/startup.rs`.
        preset: _,
        ui_lang: _,
        ui_layout: _,
        log:
            norte_config::LogSettings {
                dir: _,
                retain: _,
                format: _,
            },
        daemon:
            norte_config::DaemonSettings {
                socket: _,
                // De la TERMINAL: la ventana SIEMPRE habla con un daemon
                // (ADR 0066, D10), así que no hay modo que elegir.
                mode: _,
            },

        // ─── El host las lee de `self.config`, en caliente respecto a su
        //     propio estado (un hueco nuevo las relee).
        quick_search: _,
        ui_show_hidden: _,
        ui_parent_entry: _,
        ui_menu_bar: _,
        ui_panel_bar: _,
        ui_columns: _,
        hotlist: _,
        ui_diff: _,
        ui_diff_detached: _,
        // El cromo (spec 2026-09-10): la barra de teclas, el estilo de la
        // barra de paneles, el pie del panel, el formato de fecha, la
        // caducidad de los avisos y los botones de diálogo. Cada foto los lee.
        ui_chrome: _,

        // ─── La lee la cáscara al ARRANCAR con el resolutor compartido, así
        //     que acepta un preset o la ruta a un `.toml` (ADR 0020).
        //
        //     Sigue habiendo media deuda, y con nombre: al CAMBIAR DE PERFIL
        //     el host aplica solo presets (`aplicar_tema`), porque resolver
        //     una ruta pide leer un fichero y eso corre dentro del actor
        //     (regla 2). Plan, fase 3.
        ui_theme: _,
        //     Las variantes por esquema del escritorio (spec 2026-09-11, V6):
        //     la cáscara las resuelve al arrancar y viajan en el catálogo ya
        //     como variables; el renderer elige por `prefers-color-scheme`.
        ui_theme_light: _,
        ui_theme_dark: _,

        // ─── El host las lee para lanzar un programa: `openers.toml` manda en
        //     `pane.open` y `[ui] editor` en `pane.edit`, con el manejador del
        //     escritorio como último recurso en los dos.
        //
        //     Lo que sigue FUERA es `$EDITOR`, y es deliberado (#290): es un
        //     editor de terminal y esta ventana no tiene uno donde ponerlo.
        ui_editor: _,
        ui_editor_detached: _,

        // ─── El host la lee al pedir cerrar (`UiAction::RequestQuit`): la
        //     decisión de si preguntar es la compartida
        //     (`settings::quit_needs_confirm`), y «queda trabajo» aquí es que
        //     haya alguna task viva.
        ui_confirm_quit: _,

        // ─── De la TERMINAL, y con motivo.
        //
        //     `ui_mouse`: activar el ratón es una decisión de un emulador de
        //     terminal; una ventana lo tiene siempre. (`daemon.mode` también
        //     es de aquí; está arriba, con el resto de `daemon`.)
        ui_mouse: _,
        //     `ui_alt_menu`: el Alt solo lo tiene SIEMPRE una ventana; la
        //     clave existe porque en un terminal cuesta un protocolo de
        //     teclado que se come las tildes de tecla muerta.
        ui_alt_menu: _,

        // ─── Del DAEMON: las aplica el proceso que sirve, no el que pinta.
        //     Llegan por el socket ya en efecto.
        archive:
            norte_config::ArchiveSettings {
                max_entries: _,
                max_decompressed_bytes: _,
                max_nesting: _,
                rar_delegate: _,
            },
        ai: _,

        // ─── Del CATÁLOGO de arranque, no de la foto: las cuatro cruzan en
        //     `HostCatalog::appearance` y el renderer las enchufa como
        //     variables CSS. El tamaño mueve también la rejilla —esta ventana
        //     se reparte en celdas— y `reduce_motion` solo puede AÑADIR la
        //     petición del escritorio, nunca contradecirla (spec §17).
        //
        //     Una terminal no elige su fuente ni anima nada, así que en el
        //     terminal siguen sin aplicar y eso está dicho en su lista de
        //     exclusión.
        ui_font: _,
        ui_mono_font: _,
        ui_font_size: _,
        ui_reduce_motion: _,

        // ─── Se lee en el ARRANQUE y en el cambio de perfil, no en la foto:
        //     `Estado::siembra_de_perfil` coloca el hueco del que la sesión no
        //     sabe nada, una vez (ADR 0098).
        profile_start: _,

        // ─── Diagnóstico de la CARGA, no ajustes.
        //
        //     `project_warnings` y `profile_warnings` se enseñan los dos, y
        //     por el mismo camino: el conteo a la barra desde
        //     `aviso_de_arranque` y cada motivo al registro. El de perfil
        //     además se repite en cada cambio de perfil.
        //     `sources` es para el vigilante de config, que la ventana no
        //     tiene: su vista de «dónde vive esto» se construye de `capas`.
        //     `profile_title` lo relee el selector de perfiles del fichero,
        //     no de este campo.
        sources: _,
        project_warnings: _,
        profile_warnings: _,
        profile_title: _,
    } = c;
}

/// Y lo que NO es `CommonConfig` también.
///
/// El guarda de arriba destructuraba solo los escalares, y `openers.toml`
/// —una feature documentada entera— no es uno: vive en `FrontendConfig`. O
/// sea que el propio guarda tenía el hueco por el que se había colado la cosa
/// que vino a vigilar. Aquí se cierra.
#[test]
fn todo_campo_de_frontend_config_esta_clasificado_para_la_ventana() {
    let cfg = norte_ui_host::ajustes_por_defecto();
    let norte_frontend::config::FrontendConfig {
        // Los escalares, con su propio guarda arriba.
        common: _,

        // ─── Las lee la cáscara al arrancar (`startup.rs::keymaps`) y viajan
        //     fusionadas en `UiHostOptions`; el editor de atajos las vuelve a
        //     mirar para saber en qué capa escribe.
        keymap_layers: _,
        keymap_layer_kinds: _,
        keymap_layer_dirs: _,

        // ─── El host la lee: `pane.open` resuelve por mimetype antes de caer
        //     en el manejador del escritorio.
        openers: _,

        // ─── El host la lee: `pane.quick-search` arranca en el modo que
        //     diga la clave, como en el terminal.
        quick_search_mode: _,

        // ─── El host la lee: el selector de tema, el asistente y la pantalla
        //     de ajustes ofrecen los temas del usuario, igual que el terminal.
        user_themes: _,
    } = cfg;
}
