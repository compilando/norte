//! The bridge reaches the daemon and nothing above it: user commands (ADR
//! 0177) live in `norte-frontend`, so an agent session cannot see them as
//! long as this crate never depends on it.

#[test]
fn mcp_does_not_depend_on_the_frontend() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("the crate's manifest reads");
    assert!(!manifest.contains("norte-frontend"), "{manifest}");
}
