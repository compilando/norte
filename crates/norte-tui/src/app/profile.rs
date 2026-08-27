//! Los perfiles vistos desde `App` (ADR 0079): dónde escribe un ajuste con
//! un perfil puesto, y qué de un perfil no se puede aplicar en caliente en
//! ESTA terminal.
//!
//! Lo que NO está aquí es nada que los dos frontends compartan. Leer el
//! directorio es [`norte_frontend::config::read_profiles`] y girar por la
//! lista es [`norte_frontend::profile_picker::next_profile`]: las dos vivían
//! aquí, y la ventana las necesitaba igual — una segunda copia de «cuál es el
//! siguiente perfil» son dos órdenes distintas esperando a divergir
//! (ADR 0077).

impl crate::app::App {
    /// Dónde va un ajuste que el lector cambia desde la interfaz.
    ///
    /// El directorio del perfil ACTIVO si lo hay, y el del usuario si no.
    ///
    /// No es una preferencia de estilo: un perfil está POR ENCIMA de la capa
    /// del usuario (ADR 0079, D1), así que escribir ahí un ajuste que el
    /// perfil también fija lo deja tapado — el tema se guarda, la barra dice
    /// «config recargada», y la pantalla no cambia de color. Es exactamente la
    /// forma del bug que D10 arregló para los atajos, y que el resto de los
    /// ajustes no tenía arreglada.
    ///
    /// Cambiar un ajuste DENTRO de un espacio de trabajo significa cambiarlo
    /// en ese espacio, lo fije ya el perfil o no. Quien quiera tocar su capa
    /// de siempre sale del perfil primero.
    ///
    /// `None` cuando no hay dónde escribir, que es lo mismo que respondía
    /// `user_config_dir()` antes: el llamante ya sabe decirlo.
    #[must_use]
    pub fn config_write_dir(&self) -> Option<std::path::PathBuf> {
        match &self.active_profile {
            Some(name) => norte_config::profile_dir_from(&|k| std::env::var_os(k), name),
            None => norte_config::user_config_dir(),
        }
    }
}

/// Qué NO se puede aplicar sin reiniciar, de este perfil, en ESTA terminal.
///
/// Medido, no supuesto (D8). Lo que sí se aplica lo aplica
/// [`crate::config_reload::reload_config`], que es el paso 2 del cambio: tema
/// (ADR 0020), keymap entero, columnas con su re-orden, favoritos, openers, el
/// modo de quick search y la confirmación de salida; el ratón lo re-aplica el
/// bucle justo detrás, y la disposición y los ocultos llegan por los pasos 4
/// y 5. La lista de abajo es el resto.
///
/// **`[ui] lang` es lo único que queda**, y no por descuido: `norte_i18n::force`
/// corre UNA vez por proceso, y el hot-reload del watcher ya lleva esa misma
/// limitación escrita en su firma desde antes de que hubiera perfiles. Un
/// cambio que se callara esto sería un cambio que miente.
///
/// Las fuentes y `reduce_motion` no salen aquí porque en una terminal no
/// aplican en absoluto: son de la ventana, y decir «no se pudo aplicar» de algo
/// que este frontend nunca aplica sería ruido.
#[must_use]
pub fn no_aplicable_en_caliente(
    antes: &norte_config::CommonConfig,
    despues: &norte_config::CommonConfig,
) -> Vec<&'static str> {
    let mut fuera = Vec::new();
    if antes.ui_lang != despues.ui_lang {
        fuera.push("ui.lang");
    }
    fuera
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Un ajuste cambiado con un PERFIL activo se escribe EN EL PERFIL.
    ///
    /// Escribirlo en la capa del usuario lo deja tapado por el perfil, que
    /// está por encima (ADR 0079, D1): el tema se guarda, la barra dice
    /// «config recargada» y la pantalla no cambia de color. Es la misma forma
    /// que D10 arregló para los atajos, y que los ajustes no tenían.
    #[test]
    fn con_perfil_activo_los_ajustes_se_escriben_en_el_perfil() {
        let mut app = super::super::App::new(
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///x").expect("wire"),
                Vec::new(),
            ),
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///y").expect("wire"),
                Vec::new(),
            ),
        );
        app.active_profile = Some(OsString::from("work"));
        let dir = app.config_write_dir().expect("hay directorio");
        assert!(
            dir.ends_with("profiles/work"),
            "el ajuste va al perfil, no a la capa del usuario: {}",
            dir.display()
        );
    }

    /// Y sin perfil, donde siempre.
    #[test]
    fn sin_perfil_los_ajustes_van_a_la_capa_del_usuario() {
        let app = super::super::App::new(
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///x").expect("wire"),
                Vec::new(),
            ),
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///y").expect("wire"),
                Vec::new(),
            ),
        );
        assert_eq!(app.config_write_dir(), norte_config::user_config_dir());
    }

    /// Cambiar el idioma se ANUNCIA; cambiar el tema no, porque el tema sí se
    /// aplica en caliente.
    #[test]
    fn solo_el_idioma_se_anuncia() {
        let base = norte_config::load(&norte_config::Layers { dirs: Vec::new() }).expect("vacía");
        let mut otro = base.clone();
        otro.ui_theme = Some("nord".to_owned());
        assert!(
            no_aplicable_en_caliente(&base, &otro).is_empty(),
            "el tema es hot-reloadable (ADR 0020)"
        );

        let mut con_idioma = base.clone();
        con_idioma.ui_lang = Some("es".to_owned());
        assert_eq!(
            no_aplicable_en_caliente(&base, &con_idioma),
            vec!["ui.lang"]
        );
    }

    /// Cada campo de `CommonConfig` está CLASIFICADO: o se aplica en caliente,
    /// o se anuncia, o no es de este frontend.
    ///
    /// El destructuring va sin `..` a propósito. Un campo nuevo hace que este
    /// test no COMPILE, que es más fuerte que un assert que falle: obliga a
    /// decidir en qué grupo cae justo cuando alguien lo está añadiendo, y es
    /// la única manera de que la línea que el cambio de perfil le dice al
    /// lector siga siendo verdad dentro de un año.
    #[test]
    fn todo_campo_de_common_config_esta_clasificado() {
        let c = norte_config::load(&norte_config::Layers { dirs: Vec::new() }).expect("vacía");
        let norte_config::CommonConfig {
            // — Se aplican en caliente: `reload_config` (paso 2 del cambio).
            preset: _,
            ui_theme: _,
            quick_search: _,
            ui_confirm_quit: _,
            ui_columns: _,
            hotlist: _,
            // — El bucle de eventos lo re-aplica justo detrás de la recarga.
            ui_mouse: _,
            // — Se aplica en caliente: el reparto de cada frame lee la config
            //   vigente, así que la barra aparece o desaparece en el
            //   siguiente pintado sin nada más.
            ui_menu_bar: _,
            // — Llegan por los pasos 4 y 5 (disposición y siembra de huecos).
            ui_layout: _,
            ui_show_hidden: _,
            profile_start: _,
            // — SE ANUNCIA: `norte_i18n::force` corre una vez por proceso.
            ui_lang: _,
            // — De la VENTANA: una terminal no los aplica nunca, así que
            //   decir «no se pudo» sería ruido.
            ui_font: _,
            ui_mono_font: _,
            ui_font_size: _,
            ui_reduce_motion: _,
            // — Un perfil NO puede fijarlos (ADR 0079, D2), así que un cambio
            //   de perfil no los mueve por construcción.
            daemon_mode: _,
            daemon_socket: _,
            archive_max_entries: _,
            archive_max_decompressed_bytes: _,
            archive_max_nesting: _,
            archive_rar_delegate: _,
            log_dir: _,
            log_retain: _,
            ai: _,
            // — Diagnóstico de la carga, no ajustes.
            sources: _,
            project_warnings: _,
            profile_warnings: _,
            profile_title: _,
        } = c;
    }
}
