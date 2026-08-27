//! Los perfiles vistos desde `App` (ADR 0079): leerlos del disco, girar por
//! la lista, y —a partir de la tarea 4— cambiar de uno a otro en caliente.
//!
//! Lo que NO está aquí es la decisión de qué se enseña de cada fila: eso es
//! [`norte_frontend::profile_picker`], que es puro y lo comparten los dos
//! frontends (regla 7).

use std::ffi::{OsStr, OsString};
use std::path::Path;

use norte_frontend::profile_picker::UserProfile;

/// Lee todos los perfiles de `<dir>/profiles/`, con su título y su motivo si
/// no cargan.
///
/// **Bloquea**: lista un directorio y abre un fichero por perfil. Quien la
/// llame desde el bucle de eventos pasa por `spawn_blocking` (regla 2). Es lo
/// que hace el brazo de `profile.pick` en `dispatch`, y #244 es por qué.
///
/// Un perfil que no parsea NO desaparece: vuelve con su `problem` puesto, para
/// que el selector lo enseñe roto en vez de esconder un directorio que el
/// lector creó.
#[must_use]
pub fn lee_todos(dir: &Path) -> Vec<UserProfile> {
    let raiz = dir.join("profiles");
    norte_config::list_profiles(&raiz)
        .unwrap_or_default()
        .into_iter()
        .map(|name| {
            let toml = raiz.join(&name).join("norte.toml");
            let (title, problem) = match std::fs::read_to_string(&toml) {
                // Un perfil sin `norte.toml` es legítimo: puede traer solo su
                // `layouts/` o su `keymap.toml`.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
                Err(e) => (None, Some(e.kind().to_string())),
                Ok(raw) => match toml::from_str::<norte_config::NorteToml>(&raw) {
                    Ok(p) => (p.profile.title, None),
                    // El diagnóstico NO cita el contenido del fichero: la
                    // barra de mensajes tiene un tope y una config puede
                    // llevar rutas (#73).
                    Err(e) => (None, Some(e.message().to_owned())),
                },
            };
            UserProfile {
                name,
                title,
                problem,
            }
        })
        .collect()
}

/// El perfil que sigue (o precede) al activo, girando por el final.
///
/// `None` cuando no hay a dónde ir: ni perfiles, o solo el que ya está activo
/// — girar sobre uno solo es un cambio que no cambia nada, y hacerlo pasar por
/// la secuencia entera tiraría y recargaría la pantalla para dejarla igual.
///
/// Sin perfil activo, `next` es el primero y `prev` el último: entrar por
/// cualquiera de los dos extremos es lo que espera quien todavía no ha elegido
/// ninguno.
#[must_use]
pub fn siguiente(
    perfiles: &[UserProfile],
    activo: Option<&OsStr>,
    hacia_delante: bool,
) -> Option<OsString> {
    if perfiles.is_empty() {
        return None;
    }
    let Some(activo) = activo else {
        let i = if hacia_delante { 0 } else { perfiles.len() - 1 };
        return Some(perfiles[i].name.clone());
    };
    let actual = perfiles.iter().position(|p| p.name == activo)?;
    if perfiles.len() == 1 {
        return None;
    }
    let n = perfiles.len();
    let i = if hacia_delante {
        (actual + 1) % n
    } else {
        (actual + n - 1) % n
    };
    Some(perfiles[i].name.clone())
}

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

    fn perfiles(nombres: &[&str]) -> Vec<UserProfile> {
        nombres
            .iter()
            .map(|n| UserProfile {
                name: OsString::from(*n),
                title: None,
                problem: None,
            })
            .collect()
    }

    #[test]
    fn gira_por_el_final_en_los_dos_sentidos() {
        let p = perfiles(&["a", "b", "c"]);
        assert_eq!(
            siguiente(&p, Some(OsStr::new("c")), true).as_deref(),
            Some(OsStr::new("a")),
            "del último al primero"
        );
        assert_eq!(
            siguiente(&p, Some(OsStr::new("a")), false).as_deref(),
            Some(OsStr::new("c")),
            "y del primero al último"
        );
    }

    /// Girar sobre UN solo perfil no es un cambio: hacerlo pasar por la
    /// secuencia entera tiraría y recargaría la pantalla para dejarla igual.
    #[test]
    fn con_un_solo_perfil_activo_no_hay_a_donde_ir() {
        let p = perfiles(&["a"]);
        assert_eq!(siguiente(&p, Some(OsStr::new("a")), true), None);
        assert_eq!(siguiente(&p, Some(OsStr::new("a")), false), None);
    }

    /// Sin perfil activo se entra por el extremo que corresponda al sentido.
    #[test]
    fn sin_activo_se_entra_por_un_extremo() {
        let p = perfiles(&["a", "b", "c"]);
        assert_eq!(siguiente(&p, None, true).as_deref(), Some(OsStr::new("a")));
        assert_eq!(siguiente(&p, None, false).as_deref(), Some(OsStr::new("c")));
    }

    /// Un activo que ya no está en la lista —lo borraron con el programa
    /// abierto— no elige ninguno a ciegas: girar desde un sitio que no existe
    /// no tiene respuesta buena, y saltar al primero movería al lector a un
    /// perfil que no pidió.
    #[test]
    fn un_activo_que_ya_no_existe_no_elige_a_ciegas() {
        let p = perfiles(&["a", "b"]);
        assert_eq!(siguiente(&p, Some(OsStr::new("fantasma")), true), None);
    }

    #[test]
    fn sin_perfiles_no_hay_nada() {
        assert_eq!(siguiente(&[], None, true), None);
    }

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
