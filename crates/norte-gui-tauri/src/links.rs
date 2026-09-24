//! What external link can be opened, and who decides that it can.
//!
//! The window does NOT open any yet: there is no surface that produces them.
//! What exists is the gate, closed and with its test, because the question —
//! "does it reject a scheme that is not on the list?" — gets answered once
//! and holds for whenever there is one, and because the default answer of a
//! renderer without this check is "I open whatever I'm given", which with a
//! `file://` means reading disk and with a system scheme means running
//! something.

/// The ONLY schemes a UI link is allowed to carry.
///
/// Not `file:`, not `data:`, not `javascript:`, nothing of the system: a link
/// that appears in a help topic, in a plugin's output or in a file name is
/// DATA, and data does not choose what program gets launched.
pub const ESQUEMAS: &[&str] = &["https", "http", "mailto"];

/// Why it is not opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The scheme is not on the list.
    #[error("scheme not allowed")]
    Scheme,
    /// It does not even have the shape of an absolute URL.
    #[error("not an absolute URL")]
    Shape,
    /// It carries control characters, spaces or directional marks: a link
    /// that is painted one way and points somewhere else.
    #[error("the URL carries control characters")]
    Control,
}

/// Validates a link before anyone thinks about opening it.
///
/// # Errors
/// [`LinkError`] when the scheme is not allowed, the shape is not that of an
/// absolute URL, or the text carries control characters.
///
/// ```
/// use norte_gui_tauri::links::{validar, LinkError};
///
/// assert!(validar("https://norte.example/docs").is_ok());
/// assert_eq!(validar("file:///etc/passwd"), Err(LinkError::Scheme));
/// assert_eq!(validar("javascript:alert(1)"), Err(LinkError::Scheme));
/// ```
pub fn validar(url: &str) -> Result<(), LinkError> {
    // The SAME predicate that masks names (`norte_encoding::is_terminal_hazard`),
    // and not a fourth hand-written list: the one that used to be here left
    // out `U+061C` — a directional mark — and every zero-width invisible. A
    // link that does not read as what it opens does not get opened, and that
    // rule already exists once.
    if url
        .chars()
        .any(|c| norte_encoding::is_terminal_hazard(c) || c.is_whitespace())
    {
        return Err(LinkError::Control);
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err(LinkError::Shape);
    };
    if scheme.is_empty() || rest.is_empty() {
        return Err(LinkError::Shape);
    }
    // ASCII-lowercase comparison: `JavaScript:` is the same scheme as
    // `javascript:` to the browser, and a list that only looks at lowercase
    // is a list you can skip past by writing uppercase.
    let scheme = scheme.to_ascii_lowercase();
    if !ESQUEMAS.contains(&scheme.as_str()) {
        return Err(LinkError::Scheme);
    }
    if scheme == "mailto" && rest.contains('?') {
        return Err(LinkError::Shape);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_allowed_passes() {
        assert!(validar("https://ejemplo.test/a").is_ok());
        assert!(validar("mailto:alguien@ejemplo.test").is_ok());
    }

    #[test]
    fn everything_else_does_not() {
        for u in [
            "file:///etc/passwd",
            "JavaScript:alert(1)",
            "data:text/html,<script>",
            "ssh://host",
            "vscode://file/etc/passwd",
        ] {
            assert_eq!(validar(u), Err(LinkError::Scheme), "{u} should not pass");
        }
    }

    #[test]
    fn a_broken_url_does_not_pass() {
        assert_eq!(validar("sin-esquema"), Err(LinkError::Shape));
        assert_eq!(validar("https:"), Err(LinkError::Shape));
    }

    /// A link with a control character inside is painted one way and points
    /// somewhere else.
    #[test]
    fn control_characters_do_not_pass() {
        assert_eq!(validar("https://a.test/\u{202e}x"), Err(LinkError::Control));
        assert_eq!(validar("https://a.test/ x"), Err(LinkError::Control));
        // The ones the hand-written list used to leave out.
        assert_eq!(validar("https://a.test/\u{061c}x"), Err(LinkError::Control));
        assert_eq!(validar("https://a.test/\u{200b}x"), Err(LinkError::Control));
        assert_eq!(validar("https://a.test/\u{feff}x"), Err(LinkError::Control));
    }

    /// A `mailto:` with a query does not pass: it has historically been used
    /// to attach files from disk through a link.
    #[test]
    fn a_mailto_with_a_query_does_not_pass() {
        assert_eq!(
            validar("mailto:a@b.test?attach=/etc/shadow"),
            Err(LinkError::Shape)
        );
        assert!(validar("mailto:a@b.test").is_ok());
    }
}
