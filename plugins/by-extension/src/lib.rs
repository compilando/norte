//! `org.norte.by-extension`: an organizer that files each name into a folder
//! named after its extension (demo of the `organizer` category, phase 8).
//!
//! It needs no capabilities at all: the answer is in the name. That is on
//! purpose — it makes this plugin the smallest possible end-to-end exercise
//! of the `norte:organizer` ABI, so a failure here is the ABI and not the
//! plugin's own cleverness.
//!
//! The decisions live in pure functions with their own tests; the WIT glue
//! only exists when compiled as a component.

/// The folder a name belongs in: its extension, lowercased, or `sin-extension`
/// when it has none.
///
/// A leading dot is NOT an extension (`.bashrc` has none), which is the rule
/// every file manager uses and the one a human expects to see.
#[must_use]
pub fn carpeta_de(name: &str) -> String {
    match name.rsplit_once('.') {
        // `rsplit_once` on `.bashrc` gives `("", "bashrc")`: an empty stem
        // means the dot was leading, so there is no extension.
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => "sin-extension".to_owned(),
    }
}

/// Where `name` should go, or `None` when there is nothing to propose.
///
/// `None` for a name that has no folder to go to and for one that would move
/// onto itself. Returning a no-op would make the plan look like it does
/// something it does not.
#[must_use]
pub fn destino(name: &str) -> Option<String> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{}/{name}", carpeta_de(name)))
}

#[cfg(target_family = "wasm")]
mod guest {
    wit_bindgen::generate!({
        world: "norte:organizer/norte-organizer",
        path: "wit",
        generate_all,
    });

    use exports::norte::organizer::organizer::{Guest, LocationRef, Proposal};

    struct ByExtension;

    impl Guest for ByExtension {
        fn plan(
            id: String,
            _location: Option<LocationRef>,
            names: Vec<String>,
        ) -> Result<Vec<Proposal>, String> {
            if id != "by-extension" {
                return Err(format!("unknown organizer `{id}`"));
            }
            Ok(names
                .into_iter()
                .filter_map(|n| {
                    super::destino(&n).map(|proposed_rel| Proposal {
                        current: n,
                        proposed_rel,
                    })
                })
                .collect())
        }
    }

    export!(ByExtension);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_carpeta_es_la_extension_en_minusculas() {
        assert_eq!(carpeta_de("foto.JPG"), "jpg");
        assert_eq!(carpeta_de("a.tar.gz"), "gz");
    }

    /// Un punto inicial NO es una extensión: `.bashrc` no va a una carpeta
    /// llamada `bashrc`.
    #[test]
    fn un_punto_inicial_no_es_extension() {
        assert_eq!(carpeta_de(".bashrc"), "sin-extension");
        assert_eq!(carpeta_de("LEEME"), "sin-extension");
        assert_eq!(carpeta_de("acaba.en.punto."), "sin-extension");
    }

    #[test]
    fn el_destino_cuelga_de_la_carpeta() {
        assert_eq!(destino("factura.pdf").as_deref(), Some("pdf/factura.pdf"));
        assert_eq!(destino("LEEME").as_deref(), Some("sin-extension/LEEME"));
    }

    /// Un nombre con `/` no es un nombre, y no se propone nada para él: el
    /// host lo rechazaría, y proponerlo sería ensuciar el plan con algo que
    /// el lector va a ver desaparecer.
    #[test]
    fn un_nombre_con_barra_no_propone_nada() {
        assert!(destino("a/b").is_none());
        assert!(destino("").is_none());
    }
}
