//! The collector both help test binaries sweep with.
//!
//! It enumerates every string a [`Topic`] carries — id, title, tags,
//! `see_also`, commands, context, origin, headings, code fences, table cells
//! and every span of every block — labelled by where it lives. That set is
//! exactly what a renderer paints, which is what makes a sweep over it a
//! statement about what reaches a terminal rather than about the parser.
//!
//! It lives in a `tests/common` module and NOT behind a `pub` item of the
//! crate, because it is the shape of a TEST and not part of the help API:
//! `norte-help` would otherwise ship a walker over its own model that nothing
//! in production calls. Both `hostile.rs` (plugin topics, third-party bytes)
//! and `corpus.rs` (the shipped corpus, our own prose) reach it from here, so
//! the two families are swept by the SAME code and cannot drift into covering
//! different halves of the model.

use norte_help::{Block, Callout, Origin, Span, Topic};

/// The bytes a span carries: no syntax, just what a renderer would paint (or,
/// for the live marks, the id it would resolve).
pub fn payload(span: &Span) -> String {
    match span {
        Span::Text(t) | Span::Strong(t) | Span::Emph(t) | Span::Code(t) | Span::CommandRef(t) => {
            t.clone()
        }
        Span::TopicLink(id) => id.as_str().to_owned(),
    }
}

/// Every span a block carries, in order. Exhaustive on purpose: a new
/// [`Block`] variant carrying spans must be added here or the sweep would
/// quietly stop covering it.
pub fn collect_spans(block: &Block) -> Vec<Span> {
    match block {
        Block::Paragraph(spans) | Block::Callout { spans, .. } => spans.clone(),
        Block::Bullets(items) => items.concat(),
        Block::Heading { .. } | Block::Code { .. } | Block::Table { .. } => Vec::new(),
    }
}

/// Every string the topic carries, labelled by where it lives.
///
/// Exhaustive `match`es throughout: this is the sweep that makes the
/// "already masked" claim of `Origin::Plugin` checkable, and a variant that
/// escapes it escapes the claim.
///
/// [`Origin::BuiltIn`] contributes nothing (it carries no strings) rather
/// than panicking: one collector serves both corpora, and the caller that
/// cares which origin it is looking at asserts it where it knows the answer.
pub fn strings(topic: &Topic) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("id", topic.id.as_str().to_owned()),
        ("title", topic.title.clone()),
    ];
    out.extend(topic.tags.iter().map(|t| ("tag", t.clone())));
    out.extend(topic.see_also.iter().map(|t| ("see_also", t.to_string())));
    // A command id is a DISPATCH KEY, never masked — masking a key destroys
    // the identity it exists to carry. It is swept all the same, because the
    // parser REFUSES a key it could not paint safely instead of rewriting
    // it, which is what keeps this assertion true.
    out.extend(topic.commands.iter().map(|c| ("command", c.clone())));
    out.extend(topic.context.iter().map(|c| ("context", c.clone())));
    match &topic.origin {
        Origin::BuiltIn => {}
        Origin::Plugin { id, publisher, .. } => {
            out.push(("origin.id", id.clone()));
            out.extend(publisher.iter().map(|p| ("origin.publisher", p.clone())));
        }
    }
    for block in &topic.blocks {
        match block {
            Block::Heading { level, text } => {
                assert!((1..=3).contains(level), "level {level} is outside 1..=3");
                out.push(("heading", text.clone()));
            }
            Block::Code { lang, text } => {
                out.extend(lang.iter().map(|l| ("code.lang", l.clone())));
                // The `\n` between lines is the PARSER's structure, not the
                // author's bytes: masking it would collapse the block into
                // one unreadable line. Every other byte is swept — split on
                // `\n` and NOT `lines()`, which also strips a trailing `\r`
                // and would let a raw CR walk straight through the sweep.
                out.extend(text.split('\n').map(|l| ("code.text", l.to_owned())));
            }
            Block::Table { header, rows } => {
                assert!(
                    rows.iter().all(|r| r.len() == header.len()),
                    "a ragged row would panic a renderer indexing by column"
                );
                out.extend(header.iter().map(|c| ("table.header", c.clone())));
                out.extend(rows.iter().flatten().map(|c| ("table.cell", c.clone())));
            }
            Block::Callout { kind, .. } => {
                assert!(matches!(kind, Callout::Note | Callout::Warn | Callout::Tip));
            }
            Block::Paragraph(_) | Block::Bullets(_) => {}
        }
        out.extend(collect_spans(block).iter().map(|s| ("span", payload(s))));
    }
    out
}
