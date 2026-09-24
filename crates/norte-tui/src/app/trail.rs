//! The navigation trail, which no longer lives here.
//!
//! [`Trail`] and [`TrailStep`] moved to `norte-frontend`: the question they
//! answer — where did I come from, where do I go back to — is not a terminal
//! question, and the graphical host answers it the same way. Re-exported here
//! so the call sites don't need to change.

pub use norte_frontend::nav::{Trail, TrailStep};
