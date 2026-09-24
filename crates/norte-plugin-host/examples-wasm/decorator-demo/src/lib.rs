//! Example WASM guest (ADR 0037, G3 plan Task 2): a minimal decorator.
//!
//! Exports the `decorator` interface of the `norte-decorator` world:
//! `decorate` receives the BATCH of raw names/paths from the visible page
//! (rule 1: bytes, never assumed UTF-8; a defensive `from_utf8_lossy` is
//! done here only for classification, never for a span's `text` — this
//! guest never touches user text, it only checks whether it "contains
//! mod") and returns, FOR EACH entry in the SAME order (positional 1:1
//! contract, ADR 0037 decision table 1), an `"M"` badge if the name
//! contains the substring `"mod"`, or none (`badge: none`) otherwise.
//! Deterministic on purpose: the host's e2e verifies the exact positional
//! round trip with no dependency on any external state.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

wit_bindgen::generate!({
    world: "norte-decorator",
    path: "wit",
    // `host-log`/`host-config` live in ANOTHER package since the split
    // (ADR 0041 decision 4); wit-bindgen requires explicitly deciding what
    // to do with imports from outside the world's package.
    generate_all,
});

use exports::norte::plugin::decorator::{Decoration, Entry, Guest as DecoratorGuest};
use norte::host::host_log;

struct DecoratorDemo;

impl DecoratorGuest for DecoratorDemo {
    fn decorate(entries: Vec<Entry>) -> Vec<Decoration> {
        host_log::log(&format!("decorator-demo: {} entries", entries.len()));
        entries
            .iter()
            .map(|e| {
                let name = String::from_utf8_lossy(&e.name);
                if name.contains("mod") {
                    Decoration {
                        badge: Some("M".to_string()),
                        role: Some("warning".to_string()),
                    }
                } else {
                    Decoration {
                        badge: None,
                        role: None,
                    }
                }
            })
            .collect()
    }
}

export!(DecoratorDemo);
