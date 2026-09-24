//! The WASM layer: the `norte-columns` world's bindings and nothing more.
//!
//! Everything that decides anything lives in the other modules and is
//! tested on the host. Here only translation happens: the token and prefix
//! the host gives into [`crate::status::Location`] calls, and the verdict
//! into cells.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::status::{Location, Meta};

wit_bindgen::generate!({
    world: "norte-columns",
    path: "wit",
    generate_all,
});

use exports::norte::plugin::columns::{Guest as ColumnsGuest, LocationRef};
use norte::host::{host_config, host_log};
use norte::location::location;

/// The host's location, seen as [`Location`].
struct HostLocation {
    token: String,
}

impl Location for HostLocation {
    fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String> {
        location::read(&self.token, rel)
    }

    fn stat(&self, rel: &[u8]) -> Result<Meta, String> {
        let meta = location::stat(&self.token, rel)?;
        Ok(Meta {
            is_dir: matches!(meta.kind, location::EntryKind::Dir),
            size: meta.size,
            mtime_sec: meta.mtime_sec,
            mtime_nsec: meta.mtime_nsec,
        })
    }
}

struct GitStatus;

impl ColumnsGuest for GitStatus {
    fn column_values(
        id: String,
        location: Option<LocationRef>,
        entries: Vec<Vec<u8>>,
    ) -> Vec<Option<String>> {
        if id != crate::COLUMN_ID {
            return entries.iter().map(|_| None).collect();
        }
        // Without an approved location there is nothing to say, and saying
        // so with empty cells is the correct response: the panel keeps
        // painting.
        let Some(loc) = location else {
            return entries.iter().map(|_| None).collect();
        };
        let host = HostLocation {
            token: loc.token.clone(),
        };
        let Ok(raw) = host.read(b".git/index") else {
            // No index means no repository (or the host never got to open
            // it): empty cells, never a made-up mark.
            return entries.iter().map(|_| None).collect();
        };
        let index = match crate::index::GitIndex::parse(&raw) {
            Ok(index) => index,
            Err(why) => {
                host_log::log(&alloc::format!("git-status: unreadable index: {why:?}"));
                return entries.iter().map(|_| None).collect();
            }
        };
        // The index's OWN mtime is what decides whether an entry is "racy":
        // without it, a change made within the same second passes as clean.
        let index_mtime = host.stat(b".git/index").map_or(0, |m| m.mtime_sec);
        let ignores = crate::load_ignores(&host, &loc.prefix);
        // The configured style: the host resolves the manifest's defaults
        // before calling, so a `None` is a manifest changed underfoot and
        // the usual letters are the least strange answer.
        let style = crate::status::Style::parse(
            host_config::get("glyphs").as_deref(),
            host_config::get("ignored").as_deref(),
        );
        crate::status::status_for_with(
            &index,
            &ignores,
            &host,
            &loc.prefix,
            &entries,
            index_mtime,
            style,
        )
    }
}

export!(GitStatus);
