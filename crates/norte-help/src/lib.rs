//! norte's help corpus (ADR 0040): markdown-lite topics with TOML front
//! matter, embedded in the binary and localized.
//!
//! This crate draws NOTHING: it returns a typed model ([`Block`]/[`Span`])
//! that each frontend renders with its own technology (ratatui, GPUI, plain
//! text). The two live marks of the corpus — `{{cmd:id}}` and `[[topic]]` —
//! arrive UNRESOLVED: the chord is resolved at draw time against the user's
//! effective keymap, so the prose can never lie about keys.
//!
//! Example from the rustdoc (task 10 of this phase reactivates it, once
//! `corpus` exists and the doctest can compile):
//!
//! ```text
//! use norte_help::{Lang, topic};
//! let t = topic(Lang::En, "index").expect("the index topic exists");
//! assert_eq!(t.title, "Welcome to norte");
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

// Each module is uncommented by ITS OWN task of this phase (see the H3a plan):
// mod check; // task 8
mod corpus; // task 7
mod front_matter; // task 3
mod model; // task 2
mod parse; // tasks 4 and 5
// mod resolve; // task 9

// Public re-exports, in the same order; each one uncommented by its task:
// pub use check::{Issue, check_commands, check_contexts, check_corpus}; // task 8
pub use corpus::{topic, topic_ids, topics}; // task 7
pub use model::{Availability, Block, Callout, CommandRow, Origin, Reason, Span, Topic, TopicId}; // task 2
pub use norte_i18n::Lang;
pub use parse::{Limits, ParseError, Parsed, parse_trusted, parse_untrusted}; // tasks 4, 5 and 6
// pub use resolve::ChordResolver; // task 9
