//! Cargar una disposición de paneles de un fichero.

use std::path::Path;

use super::{LayoutError, Node};

/// El subdirectorio del directorio de configuración donde viven.
pub const LAYOUTS_DIR: &str = "layouts";

/// Lee `<dir>/layouts/<name>.toml` y valida lo que trae.
///
/// El formato es el MISMO que serializa [`to_toml`] y el mismo que llevará el
/// blob de sesión de L2: uno solo para el fichero, la sesión y lo que escupa
/// un futuro editor de layouts (ADR 0058).
///
/// # Errors
///
/// [`LayoutError::NotFound`] si no hay fichero, [`LayoutError::Parse`] si no
/// es TOML válido o no describe un árbol, y lo que devuelva
/// [`super::validate`] si el árbol es incoherente.
pub fn load(dir: &Path, name: &str) -> Result<Node, LayoutError> {
    // El nombre viene de la config del usuario y se usa para construir una
    // ruta: se acota a un componente simple para que `../..` no salga del
    // directorio de layouts.
    if name.is_empty() || Path::new(name).components().count() != 1 {
        return Err(LayoutError::BadName(name.to_owned()));
    }
    let ruta = dir.join(LAYOUTS_DIR).join(format!("{name}.toml"));
    let texto = std::fs::read_to_string(&ruta)
        .map_err(|_| LayoutError::NotFound(ruta.display().to_string()))?;
    let arbol: Node = toml::from_str(&texto).map_err(|e| LayoutError::Parse(e.to_string()))?;
    super::validate(&arbol)?;
    Ok(arbol)
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

    #[test]
    fn un_layout_escrito_se_vuelve_a_leer() {
        let dir = tempfile::tempdir().expect("tmp");
        let layouts = dir.path().join(LAYOUTS_DIR);
        std::fs::create_dir_all(&layouts).expect("mkdir");
        std::fs::write(layouts.join("mio.toml"), to_toml(&arbol()).expect("toml")).expect("write");
        assert_eq!(load(dir.path(), "mio").expect("carga"), arbol());
    }

    /// Un nombre con separadores NO construye una ruta: viene de la config
    /// del usuario, y `../../algo` saldría del directorio de layouts.
    #[test]
    fn un_nombre_con_ruta_dentro_se_rechaza() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(matches!(
            load(dir.path(), "../secreto"),
            Err(LayoutError::BadName(_))
        ));
        assert!(matches!(load(dir.path(), ""), Err(LayoutError::BadName(_))));
    }

    #[test]
    fn un_layout_que_no_esta_lo_dice_por_su_nombre() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(matches!(
            load(dir.path(), "nada"),
            Err(LayoutError::NotFound(_))
        ));
    }

    /// Un árbol INCOHERENTE se rechaza al cargar, no al pintar: el sitio
    /// donde un usuario puede hacer algo al respecto es el arranque.
    #[test]
    fn un_layout_incoherente_no_llega_a_pintarse() {
        let dir = tempfile::tempdir().expect("tmp");
        let layouts = dir.path().join(LAYOUTS_DIR);
        std::fs::create_dir_all(&layouts).expect("mkdir");
        let repetido = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        );
        std::fs::write(layouts.join("roto.toml"), to_toml(&repetido).expect("toml"))
            .expect("write");
        assert!(matches!(
            load(dir.path(), "roto"),
            Err(LayoutError::DuplicateSlotId(_))
        ));
    }
}
