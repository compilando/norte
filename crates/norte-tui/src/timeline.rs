//! The journal's timeline in the TUI (phase 7 of the WOW program): the slot,
//! what gets painted in it, and what happens when undoing to a point.
//!
//! The MODEL — what a row is, what a batch groups, what cut preserves the
//! flagged row, and how many entries it will carry — lives in
//! [`norte_frontend::timeline`], shared with the window. What is here is what
//! only this frontend knows: where the slot lands, how a point is painted,
//! and which key does what.

/// The slot's `kind`, exactly as the shared registry declares it.
pub const KIND: &str = "timeline";

/// How many rows are requested per page.
///
/// Well under the protocol's cap (200): this is a screen meant to be read,
/// and whatever does not fit is requested on reaching the bottom. Requesting
/// the maximum page on entry would fetch two hundred rows to show ten.
pub const POR_PAGINA: u32 = 50;
