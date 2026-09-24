//! What WIT a guest was compiled against, read from the binary (ADR 0094).
//!
//! A WIT package's version travels INSIDE the name of every interface a
//! component imports or exports (`norte:host/host-log@0.1.0`,
//! `norte:plugin/previewer@0.8.0`), so a bump — any bump — makes an
//! already-compiled `.wasm` fail to instantiate: wasmtime fails naming the
//! missing interface, and nothing more. This module reads those names
//! without compiling anything, so the catalog can say "compiled against
//! `norte:plugin@0.7.0`, this norte serves `@0.8.0`" and list the plugin
//! as broken with that reason.
//!
//! Both imports AND exports are looked at: a previewer IMPORTS
//! `norte:host` and EXPORTS `norte:plugin`, and both versions have to
//! match.
//!
//! The host serves ONE version of each package ([`SERVED_WIT`]), with no
//! compatibility window: keeping one would mean leaving every old world
//! linked forever, and the first plugin that asks for a WIT slot is the
//! argument for not promising that yet.

use wasmparser::{Parser, Payload};

/// The single version of each `norte:*` package this host serves.
///
/// A structural test (`tests/wit_packages.rs`) compares it against the
/// `package …;` lines of the `.wit` files: bumping a package without
/// touching this would list freshly compiled guests as broken.
pub const SERVED_WIT: &[(&str, &str)] = &[
    ("norte:host", "0.1.0"),
    ("norte:plugin", "0.10.0"),
    ("norte:provider", "0.1.0"),
    ("norte:location", "0.2.0"),
    ("norte:renamer", "0.1.0"),
    ("norte:hook", "0.2.0"),
    ("norte:thumbnail", "0.1.0"),
    ("norte:panel", "0.1.0"),
];

/// A guest compiled against one version of a package that the host serves
/// as ANOTHER version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitMismatch {
    /// The package (`norte:plugin`).
    pub package: String,
    /// The version the guest references.
    pub built_against: String,
    /// The one this host serves.
    pub served: String,
}

/// The `(package, version)` pairs of the `norte:*` packages a component
/// imports or exports, sorted and deduplicated. A core module (not a
/// component), bytes that do not parse, or a component that names no
/// norte package: empty vector — never an error nor a panic, because the
/// catalog calls it on whatever is in `plugin.wasm`.
///
/// It also walks NESTED components: a guest embedding a component that
/// names `norte:plugin@0.7.0` is listed as mismatched even though
/// wasmtime only links the outer names. That is a possible false
/// positive, never a false pass, and no norte guest nests components
/// today. Whoever needs it can narrow this to the outer section.
///
/// The cost is linear in the file's size, which the catalog caps BEFORE
/// reading it ([`crate::MAX_ARTIFACT_BYTES`]); `wasmparser` neither
/// decompresses nor recurses.
///
/// ```
/// use norte_plugin_host::wit_packages;
/// assert!(wit_packages(b"garbage").is_empty());
/// assert!(wit_packages(b"\0asm\x01\0\0\0").is_empty());
/// ```
#[must_use]
pub fn wit_packages(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        let Ok(payload) = payload else {
            // Broken bytes from here on: what was collected so far still
            // counts.
            break;
        };
        match payload {
            Payload::ComponentImportSection(section) => {
                for import in section {
                    let Ok(import) = import else { break };
                    out.extend(parse_norte_name(import.name.name));
                }
            }
            Payload::ComponentExportSection(section) => {
                for export in section {
                    let Ok(export) = export else { break };
                    out.extend(parse_norte_name(export.name.name));
                }
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `norte:<pkg>/<iface>@<ver>` → `(norte:<pkg>, <ver>)`; any other shape,
/// `None`.
///
/// The version comes from the BINARY, and the binary is written by a
/// third party: only one shaped like a version is accepted
/// (`[A-Za-z0-9.+-]`, 64 bytes at most). Anything without that shape is
/// not a norte name and produces no mismatch — and the string that ends
/// up in the manager, in `plugin list` and in `norte doctor` cannot carry
/// a terminal escape nor a hundred kilobytes. `wasmparser` only
/// guarantees UTF-8.
fn parse_norte_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("norte:")?;
    let (pkg, tail) = rest.split_once('/')?;
    let (_iface, version) = tail.rsplit_once('@')?;
    let version_ok = !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b));
    let pkg_ok = !pkg.is_empty()
        && pkg.len() <= 64
        && pkg
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !version_ok || !pkg_ok {
        return None;
    }
    Some((format!("norte:{pkg}"), version.to_owned()))
}

/// The first package the host serves as ANOTHER version, or `None` if
/// everything matches (or the guest names nothing from norte).
///
/// ```
/// use norte_plugin_host::wit_mismatch;
/// let ok = vec![("norte:host".to_owned(), "0.1.0".to_owned())];
/// assert!(wit_mismatch(&ok).is_none());
/// let old = vec![("norte:plugin".to_owned(), "0.1.0".to_owned())];
/// let m = wit_mismatch(&old).unwrap();
/// assert_eq!((m.package.as_str(), m.built_against.as_str()), ("norte:plugin", "0.1.0"));
/// ```
#[must_use]
pub fn wit_mismatch(packages: &[(String, String)]) -> Option<WitMismatch> {
    packages.iter().find_map(|(package, version)| {
        let (_, served) = SERVED_WIT.iter().find(|(p, _)| *p == package)?;
        (served != version).then(|| WitMismatch {
            package: package.clone(),
            built_against: version.clone(),
            served: (*served).to_owned(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_name_of_a_norte_interface() {
        assert_eq!(
            parse_norte_name("norte:host/host-log@0.1.0"),
            Some(("norte:host".to_owned(), "0.1.0".to_owned()))
        );
        assert_eq!(parse_norte_name("wasi:io/streams@0.2.0"), None);
        assert_eq!(parse_norte_name("norte:host/host-log"), None);
        assert_eq!(parse_norte_name("norte:/x@1"), None);
    }

    /// The version is written by a third party's binary: a terminal
    /// escape or a hundred kilobytes after the `@` is not a version, and
    /// it never reaches any screen.
    #[test]
    fn a_version_not_shaped_like_a_version_is_not_a_norte_name() {
        assert_eq!(
            parse_norte_name("norte:plugin/previewer@\u{1b}]0;x\u{7}"),
            None
        );
        let long = format!("norte:plugin/previewer@{}", "9".repeat(100_000));
        assert_eq!(parse_norte_name(&long), None);
        assert_eq!(
            parse_norte_name("norte:plugin/previewer@0.9.0-rc.1+b"),
            Some(("norte:plugin".to_owned(), "0.9.0-rc.1+b".to_owned()))
        );
        assert_eq!(parse_norte_name("norte:Plu gin/x@1.0.0"), None);
    }
}
