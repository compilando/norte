//! norte's help corpus (ADR 0040): markdown-lite topics with TOML front
//! matter, embedded in the binary and localized.
//!
//! This crate draws NOTHING: it returns a typed model ([`Block`]/[`Span`])
//! that each frontend renders with its own technology (ratatui, GPUI, plain
//! text). The two live marks of the corpus — `{{cmd:id}}` and `[[topic]]` —
//! arrive UNRESOLVED: the chord is resolved at draw time against the user's
//! effective keymap, so the prose can never lie about keys.
//!
//! The shape of that promise, end to end — a topic out of the corpus, a mark
//! still unresolved inside it, and a frontend turning it into a key:
//!
//! ```
//! use norte_help::{Availability, Block, ChordResolver, Lang, Span, render_span, topic};
//!
//! // What only a FRONTEND knows: the user's effective keymap, how to name a
//! // command, and whether it can run in the pane being drawn (rule 7 — no UI
//! // and no core state in this crate, so all three arrive through the trait).
//! struct Keymap;
//!
//! impl ChordResolver for Keymap {
//!     fn chord(&self, command: &str) -> Option<String> {
//!         // This user moved copy off F5.
//!         (command == "pane.copy").then(|| "Ctrl+C".to_owned())
//!     }
//!     fn label(&self, command: &str) -> String {
//!         command.to_owned()
//!     }
//!     fn availability(&self, _command: &str) -> Availability {
//!         Availability::Available
//!     }
//! }
//!
//! let index = topic(Lang::En, "index").expect("the index topic exists");
//! assert_eq!(index.title, "Welcome to norte");
//!
//! // The corpus stores the command ID and never a key, so the mark is still
//! // a `CommandRef` after parsing…
//! let copying = topic(Lang::En, "copying").expect("the copying topic exists");
//! let mark = Span::CommandRef("pane.copy".to_owned());
//! assert!(
//!     copying.blocks.iter().any(|b| matches!(b, Block::Paragraph(s) if s.contains(&mark))),
//!     "the prose refers to the command, not to a chord"
//! );
//!
//! // …and it becomes the key this reader actually has, at draw time.
//! assert_eq!(render_span(&mark, &Keymap), "Ctrl+C");
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod check; // integrity checks over the corpus, returned as data
mod corpus; // the embedded pages and their lookups
mod front_matter; // the TOML header between `+++` fences
mod model; // topics and blocks, free of any UI type
mod parse; // markdown-lite parser, trusted and untrusted
mod resolve; // live command marks, resolved at draw time

pub use check::{
    Inert, Issue, Mark, Stale, check_commands, check_commands_in, check_contexts,
    check_contexts_in, check_corpus, check_locales,
};
pub use corpus::{topic, topic_for_command, topic_for_context, topic_ids, topics};
pub use model::{Availability, Block, Callout, CommandRow, Origin, Reason, Span, Topic, TopicId};
pub use norte_i18n::Lang;
pub use parse::{
    CutAndDecoded, Limits, ParseError, Parsed, cut_and_decode_untrusted, foreign_commands,
    has_broken_front_matter, is_blank_id, parse_trusted, parse_untrusted,
};
pub use resolve::{ChordResolver, CommandText, ResolvedRow, render_command, render_span, rows_of};
