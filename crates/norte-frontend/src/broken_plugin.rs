//! Una extensión que no cargó, vista desde un gestor.
//!
//! El descubridor la lista en `errors` y no en el catálogo, así que no tiene
//! fila de catálogo que gobernar. Lo único que un humano puede hacer con ella
//! desde el gestor es desinstalarla, y para eso hace falta un id. La regla de
//! cuándo lo hay vive aquí, UNA vez, porque la aplican los dos frontends: una
//! regla escrita dos veces diverge en silencio.

use norte_proto::methods::{PluginInfo, PluginLoadError};

/// El id con el que se puede desinstalar un directorio de extensión que no
/// cargó, o `None` si no lo hay.
///
/// `plugin.uninstall` borra `plugins/<id>/` y valida el id ANTES de
/// convertirlo en ruta, así que un directorio que no se llama como un id
/// —`caf\xff`, `a b`— no tiene nada que mandarle. Se miran los BYTES si el
/// peer los manda (#265): la cadena `dir` sale de un `to_string_lossy`, y un
/// nombre convertido no es el nombre que hay en disco.
///
/// Y tampoco hay id si una extensión CARGADA de `loaded` lo usa. El
/// descubridor no exige que el directorio se llame como el `id` del
/// manifiesto: `plugins/org.a/` puede cargar como `org.b` al lado de un
/// `plugins/org.b/` roto. Desinstalar `org.b` borraría el roto, pero
/// retiraría también la aprobación de la cargada y el daemon la olvidaría:
/// una extensión que el humano no eligió.
///
/// ```
/// use norte_frontend::broken_plugin::uninstallable_id;
/// use norte_proto::methods::PluginLoadError;
///
/// let roto = PluginLoadError {
///     dir: "org.acme.roto".to_owned(),
///     reason: "el manifiesto no parsea".to_owned(),
///     dir_bytes: None,
/// };
/// assert_eq!(uninstallable_id(&roto, &[]).as_deref(), Some("org.acme.roto"));
/// ```
#[must_use]
pub fn uninstallable_id(e: &PluginLoadError, loaded: &[PluginInfo]) -> Option<String> {
    let id = match &e.dir_bytes {
        Some(bytes) => std::str::from_utf8(bytes).ok()?,
        None => e.dir.as_str(),
    };
    if !norte_proto::methods::is_valid_plugin_id(id) || loaded.iter().any(|p| p.id == id) {
        return None;
    }
    Some(id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::uninstallable_id;
    use norte_proto::methods::{PluginInfo, PluginLoadError};

    fn roto(dir: &str, dir_bytes: Option<&[u8]>) -> PluginLoadError {
        PluginLoadError {
            dir: dir.to_owned(),
            reason: "no cargó".to_owned(),
            dir_bytes: dir_bytes.map(<[u8]>::to_vec),
        }
    }

    fn cargada(id: &str) -> PluginInfo {
        PluginInfo {
            id: id.to_owned(),
            name: "Otra".to_owned(),
            publisher: String::new(),
            version: "1.0.0".to_owned(),
            category: "command".to_owned(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    #[test]
    fn un_directorio_que_se_llama_como_un_id_tiene_id() {
        assert_eq!(
            uninstallable_id(&roto("org.acme.roto", Some(b"org.acme.roto")), &[]).as_deref(),
            Some("org.acme.roto")
        );
    }

    #[test]
    fn un_nombre_que_no_es_un_id_no_tiene_id() {
        assert_eq!(uninstallable_id(&roto("a b", None), &[]), None);
        assert_eq!(uninstallable_id(&roto("..", None), &[]), None);
    }

    /// Mandan los bytes: la cadena ya convertida podría casar con un id que
    /// en disco no existe.
    #[test]
    fn mandan_los_bytes_y_no_la_cadena_convertida() {
        assert_eq!(
            uninstallable_id(&roto("org.acme.roto", Some(b"org.acme.rot\xff")), &[]),
            None
        );
    }

    /// Un roto cuyo nombre es el id de una CARGADA no se ofrece: desinstalar
    /// ese id se llevaría la aprobación de la otra.
    #[test]
    fn un_id_que_usa_una_cargada_no_se_ofrece() {
        assert_eq!(
            uninstallable_id(&roto("org.b", Some(b"org.b")), &[cargada("org.b")]),
            None
        );
        assert_eq!(
            uninstallable_id(&roto("org.b", Some(b"org.b")), &[cargada("org.a")]).as_deref(),
            Some("org.b")
        );
    }
}
