//! The protocol catalogue does NOT fall behind (ADR 0089).
//!
//! A new method is touched in many places. None of them is superfluous, and
//! the daemon's flat dispatch is deliberate. What was missing was that
//! **forgetting one would be noticed**.
//!
//! This file closes the first door: a method constant that is not in the
//! catalogue turns the gate red. The other surfaces — the daemon's dispatch,
//! the remote client — are checked by `norte-core/tests/catalogo_rpc.rs`,
//! which is where they can be read.
//!
//! The CODE is read, not a hand-written list, for the same reason the host
//! key sweep does it: a list drifts from the code at the first new surface.

use norte_proto::catalog::{CATALOG, Kind};

/// Constants that are NOT wire methods, with the reason they are skipped.
const NOT_METHODS: &[&str] = &[
    // The protocol version, which is a number and not a call.
    "PROTOCOL_VERSION",
];

/// The method constants declared in `methods.rs`: `NAME` → `wire`.
fn declared_constants() -> Vec<(String, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/methods.rs");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        let Some(rest) = l.strip_prefix("pub const ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(": &str = ") else {
            continue;
        };
        let value = value.trim().trim_end_matches(';').trim_matches('"');
        out.push((name.to_owned(), value.to_owned()));
    }
    assert!(
        out.len() > 60,
        "the sweep has to see the whole protocol, and it sees {}",
        out.len()
    );
    out
}

/// **Every method constant is in the catalogue.**
///
/// This is the one that turns the catalogue into something that cannot be
/// forgotten: adding `pub const FS_WHATEVER` and not registering it leaves
/// this test red, with the name in front.
#[test]
fn no_constant_is_left_out_of_the_catalogue() {
    let catalogued: std::collections::BTreeSet<&str> = CATALOG.iter().map(|m| m.name).collect();
    let mut missing = Vec::new();
    for (name, wire) in declared_constants() {
        if NOT_METHODS.contains(&name.as_str()) {
            continue;
        }
        if !catalogued.contains(wire.as_str()) {
            missing.push(format!("{name} (\"{wire}\")"));
        }
    }
    assert!(
        missing.is_empty(),
        "methods not registered in `catalog.rs`: {}.\n\
         A method that is not in the catalogue is checked by nobody: not that \
         the daemon dispatches it, not that the client knows how to request it, \
         not that its type is in the schema.",
        missing.join(", ")
    );
}

/// And the other way around: the catalogue does not name methods that do not
/// exist.
///
/// Without this, deleting a constant would leave a ghost entry that the other
/// tests would accept — and the gate would stay green over a method that is
/// no longer there. (The compiler catches the CONSTANT's name; this catches
/// the wire one.)
#[test]
fn the_catalogue_does_not_name_what_does_not_exist() {
    let declared: std::collections::BTreeSet<String> = declared_constants()
        .into_iter()
        .map(|(_, wire)| wire)
        .collect();
    for m in CATALOG {
        assert!(
            declared.contains(m.name),
            "the catalogue names `{}`, which is no longer declared in methods.rs",
            m.name
        );
    }
}

/// **The catalogue has a GOLDEN, so deleting or renaming a method shows up.**
///
/// Until now nothing protected wire names. `docs/schema/proto.schema.json`
/// publishes TYPES, not methods, and the only golden with method names
/// (`tests/golden/types/envelope.json`) contains four. Deleting
/// `fs.rename_batch` — a textbook wire break — turned nothing red beyond the
/// compilation of its callers.
///
/// The catalogue is now the one complete list, and without a snapshot it
/// would not protect either: the test above only checks that it does not name
/// what no longer exists, i.e. it FOLLOWS the deletion instead of resisting
/// it. This one resists it: any addition, removal or shape change comes out
/// as a diff the reviewer sees.
///
/// Regenerated with `NORTE_UPDATE_GOLDEN=1`, like the others.
#[test]
fn the_catalogue_has_a_golden() {
    let mut lines: Vec<String> = CATALOG
        .iter()
        .map(|m| {
            format!(
                "{}\t{:?}\t{:?}\t{}\t{}\t{}",
                m.name,
                m.kind,
                m.shape,
                m.params_ty,
                m.result_ty,
                // Where a `Stream` delivers through: it is part of what the
                // catalogue asserts, so changing it has to show up in the
                // diff just like changing the shape.
                m.stream_notifs.join(",")
            )
        })
        .collect();
    lines.sort();
    let actual = format!("{}\n", lines.join("\n"));

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/catalogo.tsv");
    if std::env::var_os("NORTE_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("dir is created");
        std::fs::write(&path, &actual).expect("golden is written");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "golden {} is missing ({e}). Generate it with NORTE_UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "the protocol catalogue changed. If it is on purpose, regenerate with \
         NORTE_UPDATE_GOLDEN=1 and get the diff REVIEWED: a method that \
         disappears or changes shape is a wire break."
    );
}

/// The catalogue covers the whole protocol, not a sample.
#[test]
fn the_catalogue_is_not_half_done() {
    let requests = CATALOG.iter().filter(|m| m.kind == Kind::Request).count();
    let notifications = CATALOG
        .iter()
        .filter(|m| m.kind == Kind::Notification)
        .count();
    assert!(requests > 55, "catalogued requests: {requests}");
    assert!(
        notifications >= 8,
        "catalogued notifications: {notifications}"
    );
}
