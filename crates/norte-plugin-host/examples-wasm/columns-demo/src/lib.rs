//! Example WASM guest (ADR 0037, G3 plan Task 4/G3b): a minimal columns.
//!
//! Exports the `columns` interface of the `norte-columns` world:
//! `column-values` receives the BATCH of raw names/paths from the visible
//! page (rule 1: bytes, never assumed UTF-8 — the length is measured in
//! BYTES, not chars, so this guest needs no decoding) and returns, FOR
//! EACH entry in the SAME order (positional 1:1 contract, ADR 0037
//! decision table 1), the name's length as decimal text for the
//! `"name-len"` column; any other column `id` (that this guest does not
//! declare in its manifest) answers `none` for the whole page —
//! deterministic and defensive, without guessing what an id that does not
//! belong to it would mean. The host's e2e verifies the exact positional
//! round trip with no dependency on any external state.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

wit_bindgen::generate!({
    world: "norte-columns",
    path: "wit",
    // `host-log`/`host-config` live in ANOTHER package since the split
    // (ADR 0041 decision 4); wit-bindgen requires explicitly deciding what
    // to do with imports from outside the world's package.
    generate_all,
});

use exports::norte::plugin::columns::{Guest as ColumnsGuest, LocationRef};
use norte::host::host_log;
use norte::location::location;

struct ColumnsDemo;

/// The only column id this guest declares and knows how to value (the
/// same id as its test manifest in `plugins_column_values_e2e.rs`).
const NAME_LEN_COLUMN: &str = "name-len";

/// Test column for the `location` capability (ADR 0057): for each entry
/// returns the size `stat` reports under the token, or `none` if the host
/// gives no location (no approved capability, or no token). A guest with
/// no location still has to keep answering, not fail.
const STAT_SIZE_COLUMN: &str = "stat-size";

impl ColumnsGuest for ColumnsDemo {
    fn column_values(
        id: String,
        location: Option<LocationRef>,
        entries: Vec<Vec<u8>>,
    ) -> Vec<Option<String>> {
        host_log::log(&format!(
            "columns-demo: id={id} {} entries, location={}",
            entries.len(),
            if location.is_some() { "yes" } else { "no" }
        ));
        if id != NAME_LEN_COLUMN && id != STAT_SIZE_COLUMN {
            // An id this guest does not supply: `none` for the WHOLE page,
            // never guessed nor omitted from the positional vector.
            return entries.iter().map(|_| None).collect();
        }
        if id == STAT_SIZE_COLUMN {
            let Some(loc) = location else {
                return entries.iter().map(|_| None).collect();
            };
            return entries
                .iter()
                .map(|raw| {
                    // The visible entry hangs off the PREFIX, not the root:
                    // the root can be an ancestor (project marker).
                    let mut rel = loc.prefix.clone();
                    if !rel.is_empty() {
                        rel.push(b'/');
                    }
                    rel.extend_from_slice(raw);
                    rel
                })
                .map(|rel| match location::stat(&loc.token, &rel) {
                    Ok(meta) => Some(meta.size.to_string()),
                    // The host says no (no capability, unknown token): an
                    // empty cell, never a jam.
                    Err(why) => {
                        host_log::log(&format!("columns-demo: stat denied: {why}"));
                        None
                    }
                })
                .collect();
        }
        entries
            .iter()
            .map(|raw| Some(raw.len().to_string()))
            .collect()
    }
}

export!(ColumnsDemo);
