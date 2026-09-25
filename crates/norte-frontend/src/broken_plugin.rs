//! A plugin that did not load, seen from a manager.
//!
//! The discoverer lists it in `errors`, not in the catalogue, so it has no
//! catalogue row to govern. The only thing a human can do with it from the
//! manager is uninstall it, and that needs an id. The rule for when there is
//! one lives here, ONCE, because both frontends apply it: a rule written
//! twice diverges silently.

use norte_proto::methods::{PluginInfo, PluginLoadError};

/// The id a plugin directory that did not load can be uninstalled with, or
/// `None` if there is none.
///
/// `plugin.uninstall` deletes `plugins/<id>/` and validates the id BEFORE
/// turning it into a path, so a directory not named like an id — `caf\xff`,
/// `a b` — has nothing to send it. The BYTES are checked when the peer sends
/// them (#265): the `dir` string comes from a `to_string_lossy`, and a
/// converted name is not the name that is on disk.
///
/// And there is also no id if a LOADED plugin from `loaded` uses it. The
/// discoverer does not require a directory to be named like the manifest's
/// `id`: `plugins/org.a/` can load as `org.b` next to a broken
/// `plugins/org.b/`. Uninstalling `org.b` would delete the broken one, but
/// would also withdraw the loaded one's approval, and the daemon would forget
/// it: a plugin the human did not choose.
///
/// ```
/// use norte_frontend::broken_plugin::uninstallable_id;
/// use norte_proto::methods::PluginLoadError;
///
/// let broken = PluginLoadError {
///     dir: "org.acme.broken".to_owned(),
///     reason: "el manifiesto no parsea".to_owned(),
///     dir_bytes: None,
/// };
/// assert_eq!(uninstallable_id(&broken, &[]).as_deref(), Some("org.acme.broken"));
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

    fn broken(dir: &str, dir_bytes: Option<&[u8]>) -> PluginLoadError {
        PluginLoadError {
            dir: dir.to_owned(),
            reason: "did not load".to_owned(),
            dir_bytes: dir_bytes.map(<[u8]>::to_vec),
        }
    }

    fn loaded(id: &str) -> PluginInfo {
        PluginInfo {
            id: id.to_owned(),
            name: "Other".to_owned(),
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
    fn a_directory_named_like_an_id_has_an_id() {
        assert_eq!(
            uninstallable_id(&broken("org.acme.roto", Some(b"org.acme.roto")), &[]).as_deref(),
            Some("org.acme.roto")
        );
    }

    #[test]
    fn a_name_that_is_not_an_id_has_no_id() {
        assert_eq!(uninstallable_id(&broken("a b", None), &[]), None);
        assert_eq!(uninstallable_id(&broken("..", None), &[]), None);
    }

    /// The bytes rule: the already-converted string could match an id that
    /// does not exist on disk.
    #[test]
    fn the_bytes_rule_not_the_converted_string() {
        assert_eq!(
            uninstallable_id(&broken("org.acme.roto", Some(b"org.acme.rot\xff")), &[]),
            None
        );
    }

    /// A broken one whose name is the id of a LOADED one is not offered:
    /// uninstalling that id would take the other one's approval with it.
    #[test]
    fn an_id_a_loaded_plugin_uses_is_not_offered() {
        assert_eq!(
            uninstallable_id(&broken("org.b", Some(b"org.b")), &[loaded("org.b")]),
            None
        );
        assert_eq!(
            uninstallable_id(&broken("org.b", Some(b"org.b")), &[loaded("org.a")]).as_deref(),
            Some("org.b")
        );
    }
}
