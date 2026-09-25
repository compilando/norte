//! The TUI's viewer: re-exports the core Viewer from norte-frontend + composes
//! the localized status text (Fluent) over its getters.

pub use norte_frontend::viewer::{PAGE, PluginPreviewView, Viewer};

use norte_encoding::Eol;
use norte_i18n::t;

/// The viewer's localized status line (what used to be `Viewer::status`):
/// encoding/binary, forced, EOL, lossy, truncated — the user ALWAYS knows
/// what they see (spec §6). Lives in the TUI (i18n) over the core Viewer's
/// getters.
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
    // The COLUMN, and only when it is not the first one. It is the only thing
    // that says "you are scrolled right" in the docked viewer, whose bottom
    // border carries this very line and so cannot also carry a horizontal
    // scrollbar: the scroll keys DO work there, and doing so with no
    // indicator at all is half the breakage this work fixes.
    if v.hscroll() > 0 {
        let _ = write!(out, "  {}/{}", v.hscroll() + 1, v.max_cols().max(1));
    }
    out
}
