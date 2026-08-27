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
}
