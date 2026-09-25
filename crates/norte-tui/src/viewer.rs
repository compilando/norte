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
    let mut out = if !v.describes_bytes() {
        // A plugin's preview: the header's «via …» says whose it is.
        String::new()
    } else if v.encoding_name().is_empty() {
        t("viewer-binary")
    } else {
        v.encoding_name().to_owned()
    };
    if v.is_forced() {
        out.push(' ');
        out.push_str(&t("viewer-forced"));
    }
    if !v.hex && v.describes_bytes() {
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
    out.trim_start().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin's preview says nothing about the file's bytes (#380). It
    /// used to fall through to «binary · no EOL» for a README that is UTF-8
    /// with LF: the viewer holds the plugin's spans, not the file.
    #[test]
    fn a_plugin_preview_does_not_call_the_file_binary() {
        let path = norte_vfs::VPath::parse("file:///x/README.md").expect("test wire");
        let v = Viewer::with_plugin_preview_styled(path, "Markdown".to_owned(), &[], false);
        let s = status(&v);
        assert!(!s.contains(&t("viewer-binary")), "{s}");
        assert!(!s.contains(&t("eol-none")), "{s}");
    }
}
