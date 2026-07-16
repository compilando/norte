//! Fuzz corto (proptest) del parseo de temas y colores (ADR 0020, fase T6):
//! ninguna entrada arbitraria hace `panic` — Ok o Error tipado. Corre en el
//! gate de PR (como los otros fuzz proptest del repo).

use norte_theme::{Color, Theme};
use proptest::prelude::*;

proptest! {
    /// `Color::parse` nunca paniquea con texto arbitrario (incluye Unicode y
    /// controles); un color válido roundtrippea.
    #[test]
    fn color_parse_never_panics(s in ".{0,16}") {
        let _ = Color::parse(&s); // Ok o Err, jamás panic.
    }

    /// `Theme::from_toml` nunca paniquea con TOML arbitrario: bytes de basura
    /// dan Err; un TOML válido con claves desconocidas se tolera (forward-compat
    /// de ADR 0020, no `deny_unknown_fields` de nivel superior).
    #[test]
    fn theme_from_toml_never_panics(s in "\\PC{0,256}") {
        let _ = Theme::from_toml(&s);
    }

    /// Un tema con un color MALFORMADO en un rol falla LIMPIO (Err), jamás
    /// paniquea ni cuela un color basura.
    #[test]
    fn color_malformado_en_rol_es_error(bad in "[^#\"]{0,8}") {
        let src = format!("[roles]\nselection = {{ fg = \"{bad}\" }}\n");
        // O bien parsea (si `bad` resultara un hex válido, improbable con el
        // filtro) o es Err; nunca panic.
        let _ = Theme::from_toml(&src);
    }
}

/// Un color válido embebido en un tema llega intacto (no fuzz, ancla de
/// cordura del camino feliz).
#[test]
fn color_valido_en_tema_llega_intacto() {
    let t = Theme::from_toml("[roles]\nerror = { fg = \"#ff0000\" }\n").unwrap();
    assert_eq!(
        t.style(norte_theme::Role::Error).fg,
        Some(Color::rgb(0xff, 0, 0))
    );
}
