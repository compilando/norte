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
        log_dir: _,
        log_retain: _,
        daemon_socket: _,

        // ─── El host las lee de `self.config`, en caliente respecto a su
        //     propio estado (un hueco nuevo las relee).
        ui_show_hidden: _,
        ui_parent_entry: _,
        ui_menu_bar: _,
        ui_panel_bar: _,
        ui_columns: _,
        hotlist: _,
        ui_diff: _,
        ui_diff_detached: _,

        // ─── El host las lee, pero SOLO EN PARTE, y eso es deuda con nombre.
        //
        //     `ui_theme`: solo como nombre de preset. Una RUTA a un `.toml`
        //     —que el terminal acepta desde la ADR 0020— se cae al tema por
        //     defecto y no se dice. Plan, fase 3.
        ui_theme: _,

        // ─── NO las lee la ventana, y no es una decisión: es la deuda que la
        //     auditoría de paridad puso nombre. Plan, fase 3.
        //
        //     `openers`/`ui_editor`: `pane.edit` es `pane.open`, o sea el
        //     manejador del escritorio, así que ni la tabla de openers ni el
        //     editor configurado se consultan.
        //     `quick_search`: el host arranca el buscador incremental en
        //     `Filter` a fuego.
        //     `ui_confirm_quit`: cerrar la ventana no pregunta nunca.
        ui_editor: _,
        ui_editor_detached: _,
        quick_search: _,
        ui_confirm_quit: _,

        // ─── De la TERMINAL, y con motivo.
        //
        //     `ui_mouse`: activar el ratón es una decisión de un emulador de
        //     terminal; una ventana lo tiene siempre.
        //     `daemon_mode`: la ventana SIEMPRE habla con un daemon
        //     (ADR 0066, D10), así que no hay modo que elegir.
        ui_mouse: _,
        daemon_mode: _,

        // ─── Del DAEMON: las aplica el proceso que sirve, no el que pinta.
        //     Llegan por el socket ya en efecto.
        archive_max_entries: _,
        archive_max_decompressed_bytes: _,
        archive_max_nesting: _,
        archive_rar_delegate: _,
        ai: _,

        // ─── MUERTAS en todo el workspace: no las lee nadie, ni aquí ni en
        //     el terminal. Están en el catálogo de ajustes, así que hoy la
        //     pantalla de Ajustes afirma algo falso. Plan, fase 3: se
        //     implementan o se quitan del catálogo.
        //
        //     Ojo con `ui_font*` y `ui_reduce_motion`: la lista de exclusión
        //     del cambio de perfil dice que «viajan en el catálogo del
        //     arranque y la hoja de estilos las lee una vez», y no es cierto
        //     —`style.css` fija la familia y el tamaño a mano—, así que el
        //     aviso da a entender que se aplicaron al arrancar.
        ui_font: _,
        ui_mono_font: _,
        ui_font_size: _,
        ui_reduce_motion: _,
        profile_start: _,

        // ─── Diagnóstico de la CARGA, no ajustes.
        //
        //     `project_warnings` sí se enseña (`aviso_de_arranque`).
        //     `profile_warnings` no lo enseña ninguno de los dos, y
        //     `norte-config` argumenta largo que callarlas es el fallo grave.
        //     Plan, fase 3.
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
