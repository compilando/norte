//! List a directory to PUT INTO a pane.
//!
//! It used to live in the `ntc` binary's root, which is a crate DISTINCT
//! from this lib, and that made it unreachable for everything else —
//! including [`crate::session_push`], which restores the session by
//! listing the slots the session placed.
//!
//! The error is the `Backend`'s and not an `anyhow`: rule 6 reserves
//! `anyhow` for binaries, and here the `anyhow` only wrapped a
//! [`norte_proto::Error`] in its own `Display` — the type was lost for no
//! gain. The binary's callers still use `?` inside an `anyhow` function,
//! which is exactly what `From` already knows how to do.

use norte_core::backend::Backend;
use norte_proto::{Error, VPath};

use crate::app::Pane;

/// Startup's initial pane: a COMPLETE listing of `start` requesting the
/// configured attrs (#117) — without them the attr cells would be born
/// blank until the first cd/refresh. Rule 7: everything through the
/// `Backend`.
///
/// # Errors
///
/// Whatever the `Backend` returns when listing `start`: not translated nor
/// wrapped, because whoever paints it needs the type (a startup
/// `PermissionDenied` is not said the same way as a `NotFound`).
pub async fn initial_pane(
    backend: &Backend,
    start: &VPath,
    attrs: &[String],
) -> Result<Pane, Error> {
    let (entries, skipped) = backend.list_with_skipped_attrs(start, attrs).await?;
    // A pane's startup is a screen: the anchor is retained (#301).
    backend.remember_listing_anchor(start).await;
    let mut pane = Pane::new(start.clone(), entries);
    // #93: the container's skipped ones also on STARTUP — the badge should
    // not be born empty when the data is free (review #117 task 2).
    pane.set_skipped(skipped);
    Ok(pane)
}
