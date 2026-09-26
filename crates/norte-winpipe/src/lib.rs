//! The Windows named pipe's security boundary for the norte daemon (ADR
//! 0159).
//!
//! On unix the daemon's socket lives in a 0700 directory and both ends
//! check the peer's uid (`SO_PEERCRED`). A named pipe has neither for free:
//! its default DACL lets Everyone READ, and tokio exposes no peer identity.
//! This crate supplies both, and is the only place the pipe needs
//! `unsafe`:
//!
//! - `create_server`: a pipe instance whose protected DACL grants access
//!   to the current user's SID and nobody else, rejecting remote clients;
//! - `client_user`: the user SID of the connecting process, the daemon's
//!   `SO_PEERCRED`;
//! - `server_is_ours`: the client's check, read from the pipe object (owner
//!   and integrity label), not from a PID.
//!
//! Everything is empty off Windows.
#![deny(unsafe_code)]

#[cfg(windows)]
mod imp;
#[cfg(windows)]
pub use imp::{UserSid, client_user, create_server, current_user, server_is_ours};
