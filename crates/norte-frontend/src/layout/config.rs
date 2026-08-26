//! Cargar una disposición de paneles de un fichero.
//!
//! El nombre de una disposición **es un nombre de fichero**, y por eso viaja
//! como [`OsStr`] y no como `String` (#246): `--layout` y el picker acaban los
//! dos en `<dir>/layouts/<nombre>.toml`, así que pasar el valor por
//! `to_string_lossy` cambiaba el fichero que se abre — `$'\xff'` y `$'\xfe'`
//! aterrizaban los dos en `layouts/\xEF\xBF\xBD.toml`, en silencio.
//!
//! Y el fichero se resuelve **byte a byte contra el directorio** (#245): en
//! APFS o NTFS, `load(dir, "orthodox")` con un `Orthodox.toml` guardado abría
//! el fichero del usuario mientras la fila decía «de fábrica» y la vista
//! previa enseñaba el preset. Aquí se listan las entradas y se exige el nombre
//! EXACTO, así que el resultado es el mismo en los tres sistemas.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use super::{LayoutError, Node};

/// El subdirectorio del directorio de configuración donde viven.
pub const LAYOUTS_DIR: &str = "layouts";

/// La extensión, sin punto.
const EXT: &str = "toml";

/// Nombres de dispositivo de Win32, que se resuelven ANTES de mirar el disco.
///
/// Se rechazan en TODOS los sistemas, no solo en Windows: un `layouts/CON.toml`
/// creado en Linux y sincronizado a un Windows abriría la consola desde una
/// TUI que tiene el terminal en modo raw, y `NUL` daría una lectura vacía. Un
/// nombre reservado no vale más de un lado que del otro, así que se rechaza
/// donde se escribe y donde se lee.
/// ¿Este nombre puede ser un fichero de `layouts/` y nada más?
///
/// `Path::components().count() == 1` NO basta y esa era la comprobación
/// anterior (#246): en Windows `Path::new("C:")` es exactamente un componente
/// —un `Prefix`— y `Path::join` con un prefijo SUSTITUYE la base entera, así
/// que el `format!` acababa leyendo `C:.toml` relativo al directorio actual de
/// la unidad C. Aquí se mira el nombre, no su forma de ruta.
/// Delega en [`norte_config::valid_profile_name`], que es la MISMA pregunta
/// —«¿puede esto ser una entrada suelta de un directorio nuestro?»— y estaba
/// contestada dos veces. La canónica vive en `norte-config` porque está
/// debajo: los perfiles la necesitan para no dejar que un nombre apunte la
/// capa de configuración a cualquier sitio del disco, y dos copias de una
/// regla de seguridad divergen.
fn nombre_usable(name: &OsStr) -> bool {
    norte_config::valid_profile_name(name)
}

/// `<name>.toml`, sin pasar por `String`.
fn con_extension(name: &OsStr) -> OsString {
    let mut f = name.to_os_string();
    f.push(".");
    f.push(EXT);
    f
}

/// Lee `<dir>/layouts/<name>.toml` y valida lo que trae.
///
/// El formato es el MISMO que serializa [`to_toml`] y el mismo que lleva el
/// blob de sesión de L2: uno solo para el fichero, la sesión y lo que escupa
/// un futuro editor de layouts (ADR 0058).
///
/// El fichero se busca en el LISTADO del directorio y se exige que su nombre
/// coincida byte a byte con el pedido: en un sistema que no distingue
/// mayúsculas, dejar resolver al SO abría el fichero del usuario cuando se
/// pedía el de fábrica (#245).
///
/// # Errors
///
/// [`LayoutError::BadName`] si el nombre no puede ser un fichero de
/// `layouts/`, [`LayoutError::NotFound`] si no hay ninguno con ese nombre
/// exacto, [`LayoutError::Parse`] si no es TOML válido o no describe un árbol,
/// y lo que devuelva [`super::validate`] si el árbol es incoherente.
pub fn load(dir: &Path, name: &OsStr) -> Result<Node, LayoutError> {
    if !nombre_usable(name) {
        return Err(LayoutError::BadName(name.to_string_lossy().into_owned()));
    }
    let carpeta = dir.join(LAYOUTS_DIR);
    let buscado = con_extension(name);
    let existe = std::fs::read_dir(&carpeta)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name() == buscado);
    let ruta = carpeta.join(&buscado);
    if !existe {
        return Err(LayoutError::NotFound(ruta.display().to_string()));
    }
    let texto = std::fs::read_to_string(&ruta)
        .map_err(|_| LayoutError::NotFound(ruta.display().to_string()))?;
    let arbol: Node = toml::from_str(&texto).map_err(|e| LayoutError::Parse(e.to_string()))?;
    super::validate(&arbol)?;
    Ok(arbol)
}

/// Los nombres de los layouts que el usuario tiene en `<dir>/layouts/`,
/// ordenados.
///
/// No valida ni parsea: el selector los ENSEÑA, y quien elija uno roto se
/// entera al elegirlo con el error del cargador.
///
/// Un directorio que no existe no es un error: es un usuario que no ha
/// guardado ninguno.
///
/// Devuelve [`OsString`] y no `String`: un nombre que no es UTF-8 es un
/// fichero como cualquier otro y antes desaparecía del selector sin decir
/// nada (#246 m2). La extensión se compara sin distinguir mayúsculas, porque
/// `MIO.TOML` es el mismo fichero para el SO que lo guardó así.
#[must_use]
pub fn list(dir: &Path) -> Vec<OsString> {
    let Ok(entradas) = std::fs::read_dir(dir.join(LAYOUTS_DIR)) else {
        return Vec::new();
    };
    let mut nombres: Vec<OsString> = entradas
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(EXT))
        })
        .filter_map(|e| e.path().file_stem().map(OsStr::to_os_string))
        // Un nombre que `load` no aceptaría no se ofrece: la fila estaría ahí
        // para fallar al pulsarla.
        .filter(|n| nombre_usable(n))
        .collect();
    nombres.sort();
    nombres
}

/// El árbol como TOML, para escribirlo.
///
/// # Errors
///
/// [`LayoutError::Parse`] si el árbol no se puede serializar.
pub fn to_toml(tree: &Node) -> Result<String, LayoutError> {
    toml::to_string_pretty(tree).map_err(|e| LayoutError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dir, KindId, SlotId};

    fn arbol() -> Node {
        Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        )
    }

    fn escribe(dir: &Path, fichero: &OsStr, texto: &str) {
        let layouts = dir.join(LAYOUTS_DIR);
        std::fs::create_dir_all(&layouts).expect("mkdir");
        std::fs::write(layouts.join(fichero), texto).expect("write");
    }

    #[test]
    fn un_layout_escrito_se_vuelve_a_leer() {
        let dir = tempfile::tempdir().expect("tmp");
        escribe(
            dir.path(),
            OsStr::new("mio.toml"),
            &to_toml(&arbol()).expect("toml"),
        );
        assert_eq!(load(dir.path(), OsStr::new("mio")).expect("carga"), arbol());
    }

    #[test]
    fn listar_devuelve_los_toml_ordenados_y_sin_extension() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(list(dir.path()).is_empty(), "sin directorio, sin nombres");
        for n in ["zeta.toml", "alfa.toml", "notas.txt"] {
            escribe(dir.path(), OsStr::new(n), "");
        }
        assert_eq!(
            list(dir.path()),
            vec![OsString::from("alfa"), OsString::from("zeta")]
        );
    }

    /// `MIO.TOML` es el mismo fichero para el sistema que lo guardó así, y
    /// `--layout MIO` lo cargaba: no aparecer en el selector era una fila que
    /// faltaba, no una protección (#246 m2).
    #[test]
    fn la_extension_no_distingue_mayusculas() {
        let dir = tempfile::tempdir().expect("tmp");
        escribe(dir.path(), OsStr::new("MIO.TOML"), "");
        assert_eq!(list(dir.path()), vec![OsString::from("MIO")]);
    }

    /// Un nombre con separadores NO construye una ruta: viene de la config
    /// del usuario —de CUALQUIER capa, la del proyecto incluida— y `../algo`
    /// saldría del directorio de layouts.
    #[test]
    fn un_nombre_con_ruta_dentro_se_rechaza() {
        let dir = tempfile::tempdir().expect("tmp");
        for malo in ["../secreto", "", "sub/mio", "sub\\mio", ".", ".."] {
            assert!(
                matches!(
                    load(dir.path(), OsStr::new(malo)),
                    Err(LayoutError::BadName(_))
                ),
                "{malo:?} debería rechazarse"
            );
        }
    }

    /// `Path::new("C:")` es UN componente en Windows —un `Prefix`— y `join`
    /// con él sustituye la base entera: la comprobación de «un solo
    /// componente» lo admitía y la lectura se iba al directorio actual de la
    /// unidad C (#246 M2).
    #[test]
    fn un_prefijo_de_unidad_no_es_un_nombre() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(matches!(
            load(dir.path(), OsStr::new("C:")),
            Err(LayoutError::BadName(_))
        ));
        assert!(
            matches!(
                load(dir.path(), OsStr::new("notas:secreto")),
                Err(LayoutError::BadName(_))
            ),
            "un flujo alternativo de NTFS tampoco"
        );
    }

    /// `--layout CON` leía `layouts\\CON.toml`, que Win32 resuelve a la
    /// CONSOLA, desde una TUI con el terminal en modo raw (#246 M2). Se
    /// rechaza en todos los sistemas: el fichero se sincroniza, el nombre
    /// reservado viaja con él.
    #[test]
    fn los_nombres_de_dispositivo_de_windows_se_rechazan_en_todas_partes() {
        let dir = tempfile::tempdir().expect("tmp");
        for malo in ["CON", "con", "NUL", "com1", "LPT9", "CON.toml"] {
            assert!(
                matches!(
                    load(dir.path(), OsStr::new(malo)),
                    Err(LayoutError::BadName(_))
                ),
                "{malo} debería rechazarse"
            );
        }
        // Y no se ofrecen en el selector, que es de donde salen sin teclear.
        escribe(dir.path(), OsStr::new("CON.toml"), "");
        assert!(list(dir.path()).is_empty());
    }

    /// Windows se come el punto y el espacio finales: el fichero que se abre
    /// no sería el que se nombró.
    #[test]
    fn un_punto_o_un_espacio_al_final_no_es_un_nombre() {
        let dir = tempfile::tempdir().expect("tmp");
        for malo in ["mio.", "mio "] {
            assert!(
                matches!(
                    load(dir.path(), OsStr::new(malo)),
                    Err(LayoutError::BadName(_))
                ),
                "{malo:?} debería rechazarse"
            );
        }
    }

    /// El nombre se resuelve contra el LISTADO, byte a byte. En APFS o NTFS
    /// `load(dir, "orthodox")` con un `Orthodox.toml` guardado abría el
    /// fichero del usuario mientras la fila decía «de fábrica» (#245); aquí
    /// no hay fichero con ese nombre, y punto — la misma respuesta en los
    /// tres sistemas.
    #[test]
    fn un_nombre_que_solo_difiere_en_mayusculas_no_es_el_mismo_fichero() {
        let dir = tempfile::tempdir().expect("tmp");
        escribe(
            dir.path(),
            OsStr::new("Orthodox.toml"),
            &to_toml(&arbol()).expect("toml"),
        );
        assert!(matches!(
            load(dir.path(), OsStr::new("orthodox")),
            Err(LayoutError::NotFound(_))
        ));
        assert_eq!(
            load(dir.path(), OsStr::new("Orthodox")).expect("el suyo sí"),
            arbol()
        );
    }

    #[test]
    fn un_layout_que_no_esta_lo_dice_por_su_nombre() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(matches!(
            load(dir.path(), OsStr::new("nada")),
            Err(LayoutError::NotFound(_))
        ));
    }

    /// Un árbol INCOHERENTE se rechaza al cargar, no al pintar: el sitio
    /// donde un usuario puede hacer algo al respecto es el arranque.
    #[test]
    fn un_layout_incoherente_no_llega_a_pintarse() {
        let dir = tempfile::tempdir().expect("tmp");
        let repetido = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        );
        escribe(
            dir.path(),
            OsStr::new("roto.toml"),
            &to_toml(&repetido).expect("toml"),
        );
        assert!(matches!(
            load(dir.path(), OsStr::new("roto")),
            Err(LayoutError::DuplicateSlotId(_))
        ));
    }

    /// Sin ningún listado no hay disposición: se rechaza al cargar, que es
    /// donde todavía queda la pantalla anterior (#242).
    #[test]
    fn un_layout_sin_listado_no_llega_a_aplicarse() {
        let dir = tempfile::tempdir().expect("tmp");
        let sin = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        );
        escribe(
            dir.path(),
            OsStr::new("sin.toml"),
            &to_toml(&sin).expect("toml"),
        );
        assert!(matches!(
            load(dir.path(), OsStr::new("sin")),
            Err(LayoutError::NoBrowser)
        ));
    }

    /// TODO el corpus hostil pasa por el cargador y NINGÚN nombre saca la
    /// lectura de `<dir>/layouts/`: ni los separadores, ni `C:`, ni un
    /// reservado de Win32, ni un nombre que no es texto. La comprobación
    /// anterior era `components().count() == 1`, que admite `C:` (#246 M2).
    ///
    /// Se comprueba sobre el ERROR y sobre el efecto: lo que no se rechaza
    /// tiene que dar `NotFound` de un fichero DENTRO del directorio —el
    /// directorio no existe, así que ninguno se abre— y nunca `BadName` de
    /// algo que sí valía.
    #[cfg(unix)]
    #[test]
    fn ningun_nombre_del_corpus_sale_del_directorio_de_layouts() {
        use std::os::unix::ffi::OsStrExt as _;

        let dir = tempfile::tempdir().expect("tmp");
        let dentro = dir.path().join(LAYOUTS_DIR);
        for n in norte_testkit::corpus::hostile_names() {
            let name = OsStr::from_bytes(&n.bytes);
            match load(dir.path(), name) {
                Err(LayoutError::BadName(_)) => {}
                Err(LayoutError::NotFound(ruta)) => assert!(
                    ruta.starts_with(&dentro.display().to_string()),
                    "{} resolvió fuera: {ruta}",
                    n.id
                ),
                otro => panic!("{}: {otro:?}", n.id),
            }
        }
    }

    /// Un nombre que no es UTF-8 es un fichero como cualquier otro: sale en
    /// el listado y se carga por sus bytes. Antes desaparecía del selector, y
    /// por `--layout` se convertía en `\u{FFFD}` —o sea, en OTRO fichero, o
    /// en el mismo para dos bytes distintos (#246 M1).
    #[cfg(unix)]
    #[test]
    fn un_nombre_que_no_es_utf8_ni_se_pierde_ni_se_confunde() {
        use std::os::unix::ffi::OsStrExt as _;

        let dir = tempfile::tempdir().expect("tmp");
        let crudo = OsStr::from_bytes(b"\xff");
        escribe(
            dir.path(),
            OsStr::from_bytes(b"\xff.toml"),
            &to_toml(&arbol()).expect("toml"),
        );
        assert_eq!(list(dir.path()), vec![crudo.to_os_string()]);
        assert_eq!(load(dir.path(), crudo).expect("carga"), arbol());
        // El reemplazo del lossy es OTRO nombre, y si existiera se abriría en
        // lugar del pedido.
        assert!(matches!(
            load(dir.path(), OsStr::from_bytes("\u{FFFD}".as_bytes())),
            Err(LayoutError::NotFound(_))
        ));
    }
}
