//! Corpus de ayuda de norte (ADR 0040): temas en markdown-lite con front
//! matter TOML, embebidos en el binario y localizados.
//!
//! Este crate NO pinta nada: devuelve un modelo tipado (`Block`/`Span`)
//! que cada frontend renderiza con su propia tecnología (ratatui, GPUI,
//! texto plano). Las dos marcas vivas del corpus —`{{cmd:id}}` y
//! `[[tema]]`— llegan SIN resolver: el chord se resuelve al pintar contra
//! el keymap efectivo del usuario, así la prosa jamás miente sobre teclas.
//!
//! Ejemplo del rustdoc (lo reactiva la tarea 10 de esta fase, cuando
//! `corpus` ya existe y el doctest puede compilar):
//!
//! ```text
//! use norte_help::{Lang, topic};
//! let t = topic(Lang::En, "index").expect("el índice existe");
//! assert_eq!(t.title, "Welcome to norte");
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

// Cada módulo lo descomenta SU tarea de esta fase (ver el plan H3a):
// mod check; // tarea 8
// mod corpus; // tarea 7
// mod front_matter; // tarea 3
// mod model; // tarea 2
// mod parse; // tareas 4 y 5
// mod resolve; // tarea 9

// Reexports públicos, en el mismo orden; cada uno lo descomenta su tarea:
// pub use check::{Issue, check_commands, check_contexts, check_corpus}; // tarea 8
// pub use corpus::{topic, topic_ids, topics}; // tarea 7
// pub use model::{
//     Availability, Block, Callout, CommandRow, Origin, Reason, Span, Topic, TopicId,
// }; // tarea 2
pub use norte_i18n::Lang;
// pub use parse::{Limits, ParseError, Parsed, parse_trusted, parse_untrusted}; // tareas 4 y 5
// pub use resolve::ChordResolver; // tarea 9
