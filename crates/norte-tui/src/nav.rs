//! Navegación TC (spec 2026-07-18): el quick search PURO (`Mode`, `matches`,
//! `QuickSearch`) vive ahora en [`norte_frontend::nav`] — compartido con la
//! GUI — y se re-exporta aquí para no tocar los call-sites de la TUI. El
//! historial de directorios por pane ([`History`]) es específico de la TUI y
//! se queda.

pub use norte_frontend::nav::{
    History, Mode, QuickSearch, Trail, TrailStep, archive_root_for, fold, matches,
};

#[cfg(test)]
mod archive_nav_tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};

    fn entry(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("wire de test"),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    #[test]
    fn archive_root_for_decide_por_extension_y_kind() {
        let e = entry("file:///d/A.ZIP", EntryKind::File);
        assert_eq!(
            archive_root_for(&e).expect("mayúsculas entran").to_wire(),
            "zip+file:///d/A.ZIP/!"
        );
        assert!(archive_root_for(&entry("file:///d/a.tar", EntryKind::File)).is_some());
        assert!(archive_root_for(&entry("file:///d/a.txt", EntryKind::File)).is_none());
        // Un dir llamado x.zip NO es contenedor; un symlink tampoco (v1).
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Dir)).is_none());
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Symlink)).is_none());
        // #56 (antes v1 = no-op): Enter sobre un zip DENTRO de un tar
        // compone una capa más — anidamiento navegable.
        assert_eq!(
            archive_root_for(&entry("tar+file:///a.tar/!/i.zip", EntryKind::File))
                .expect("anidado navegable")
                .to_wire(),
            "zip+tar+file:///a.tar/!/i.zip/!"
        );
    }

    /// #55: `.tgz`/`.tar.gz` no coinciden con el token `tar+gz` vía el
    /// genérico `.{formato}` (el `+` no está en la extensión de archivo) —
    /// `EXT_ALIASES` los mapea explícitamente, case-insensitive, antes del
    /// genérico. `.tar`/`.zip` planos siguen funcionando sin pasar por el
    /// alias (`.tar.gz` NO debe casar `.tar`: termina en `.gz`).
    #[test]
    fn archive_root_for_extensiones_targz() {
        for wire in [
            "file:///d/a.tgz",
            "file:///d/a.tar.gz",
            "file:///d/A.TAR.GZ",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} debería ser navegable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        // Extensiones planas siguen funcionando (no capturadas por el alias).
        assert_eq!(
            archive_root_for(&entry("file:///d/a.tar", EntryKind::File))
                .expect("tar plano sigue")
                .scheme(),
            "tar+file"
        );
        assert_eq!(
            archive_root_for(&entry("file:///d/a.zip", EntryKind::File))
                .expect("zip plano sigue")
                .scheme(),
            "zip+file"
        );
    }

    /// Candado de encoding (#55, review): `ends_ci` es de BYTES y el compose
    /// no pasa por String — un nombre NO-UTF8 terminado en `.tgz` compone
    /// bien y sus bytes crudos sobreviven el wire (regla 1). Si alguien
    /// "simplifica" mañana con `to_str()`/lossy, esto se pone rojo.
    #[test]
    fn archive_root_for_targz_nombre_no_utf8() {
        for wire in [
            "file:///d/%FF%FE.tgz",
            "file:///d/a%F1o.TGZ",
            "file:///d/%FF.tar.gz",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} debería ser navegable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        assert_eq!(
            archive_root_for(&entry("file:///d/%FF%FE.tgz", EntryKind::File))
                .expect("no-UTF8 navegable")
                .to_wire(),
            "tar+gz+file:///d/%FF%FE.tgz/!",
            "los bytes crudos sobreviven el compose"
        );
    }
}
