//! Viewer de la TUI: re-exporta el Viewer core de norte-frontend + compone el
//! texto de estado localizado (Fluent) sobre sus getters.

pub use norte_frontend::viewer::{PAGE, PluginPreviewView, Viewer};

use norte_encoding::Eol;
use norte_i18n::t;

/// Línea de estado localizada del viewer (lo que antes era `Viewer::status`):
/// encoding/binario, forzado, EOL, pérdidas, truncado — el usuario SIEMPRE sabe
/// qué ve (spec §6). Vive en la TUI (i18n) sobre los getters del Viewer core.
#[must_use]
pub fn status(v: &Viewer) -> String {
    use std::fmt::Write;
    let mut out = if v.encoding_name().is_empty() {
        t("viewer-binary")
    } else {
        v.encoding_name().to_owned()
    };
    if v.is_forced() {
        out.push(' ');
        out.push_str(&t("viewer-forced"));
    }
    if !v.hex {
        let eol = match v.eol() {
            Eol::Lf => "LF".to_owned(),
            Eol::CrLf => "CRLF".to_owned(),
            Eol::Cr => "CR".to_owned(),
            Eol::Mixed => t("eol-mixed"),
            Eol::None => t("eol-none"),
        };
        let _ = write!(out, "  {eol}");
    }
    if v.had_errors() {
        let _ = write!(out, "  {}", t("viewer-lossy"));
    }
    if v.truncated {
        let _ = write!(out, "  {}", t("viewer-truncated"));
    }
    // La COLUMNA, y solo cuando no es la primera. Es lo único que dice «estás
    // desplazado a la derecha» en el visor acoplado, cuyo borde de abajo lleva
    // esta misma línea y por eso no puede llevar una barra horizontal: las
    // teclas de desplazar SÍ funcionan ahí, y hacerlo sin ningún indicador es
    // la mitad de la avería que este trabajo arregla.
    if v.hscroll() > 0 {
        let _ = write!(out, "  {}/{}", v.hscroll() + 1, v.max_cols().max(1));
    }
    out
}
