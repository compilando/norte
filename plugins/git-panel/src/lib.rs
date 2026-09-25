//! `org.norte.git-panel`: the official repository-status panel.
//!
//! The host opens the repository's root —the ancestor containing `.git`,
//! which is what the manifest declares as `location-root-marker`— and hands
//! this guest an opaque token. From there, all it does is READ two files:
//! `.git/HEAD` and `.git/logs/HEAD`.
//!
//! What it does NOT do: write, run `git`, or know where anything is. There
//! are no paths in this code; there is a token and relative paths.
//!
//! # Why the reflog and not the log
//!
//! A branch's log lives in the object database: loose objects are zlib
//! streams and packed ones need the pack index, i.e. an object reader
//! inside a `no_std` guest. The reflog (`.git/logs/HEAD`) is plain text, one
//! line per move, and answers the question that is genuinely hard to
//! remember: where did I come from. The cheap thing answers the useful one.
//!
//! # What this panel CANNOT say
//!
//! - **Whether there are unsaved changes.** `git-status` says that, since it
//!   compares the tree against the index and already exists as a column.
//!   Repeating it here would be the same count done twice and two answers
//!   that can disagree.
//! - **Nothing, outside a repository or over a location that is not
//!   `file://`.** The location capability mints no token for sftp, s3, mem
//!   or the inside of an archive: the panel SAYS so in a line, because an
//!   empty slot is not distinguishable from a broken one.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// The WASM layer only exists when compiled AS a component: the host's tests
// compile the same crate without it, which is what allows testing the
// decisions without a wasm runtime in between.
#[cfg(target_arch = "wasm32")]
mod guest;

/// The `kind` of the panel this plugin contributes; the same one from the
/// manifest.
pub const PANEL_KIND: &str = "status";

/// How many moves are shown if the configuration does not say otherwise.
pub const MOVES_DEFAULT: usize = 5;

/// What can be read from the repository without opening the object database.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The current branch, or `None` with a detached `HEAD`.
    pub branch: Option<String>,
    /// The commit `HEAD` points to, abbreviated to twelve characters.
    pub commit: Option<String>,
    /// The recent moves, from newest to oldest.
    pub moves: Vec<Move>,
}

/// A reflog move: where it went and why.
#[derive(Debug, PartialEq, Eq)]
pub struct Move {
    /// The destination commit, abbreviated.
    pub to: String,
    /// What git wrote as the reason (`checkout: moving from a to b`).
    pub reason: String,
}

/// The branch `.git/HEAD` names, if it points to one.
///
/// `ref: refs/heads/<branch>` is the normal case; a bare SHA is a detached
/// `HEAD` and there is no branch to name. Any `refs/…` is accepted and the
/// last segment is shown: a branch can be called `feature/x/y`, and keeping
/// everything after `refs/heads/` preserves the slashes the reader wrote.
///
/// ```
/// use git_panel::head_branch;
///
/// assert_eq!(head_branch(b"ref: refs/heads/main\n").as_deref(), Some("main"));
/// assert_eq!(head_branch(b"ref: refs/heads/feat/x\n").as_deref(), Some("feat/x"));
/// assert!(head_branch(b"9f1c2a0e\n").is_none());
/// ```
#[must_use]
pub fn head_branch(raw: &[u8]) -> Option<String> {
    let text = core::str::from_utf8(raw).ok()?;
    let line = text.lines().next()?.trim();
    let reference = line.strip_prefix("ref:")?.trim();
    let branch = reference.strip_prefix("refs/heads/")?;
    if branch.is_empty() {
        return None;
    }
    Some(branch.to_string())
}

/// A hash abbreviated to twelve characters, which is what git shows by
/// default in a large repository and what fits in a narrow panel.
fn abbreviate(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// The state `.git/logs/HEAD` describes: the current commit and the last
/// moves.
///
/// A line's format is `<before> <after> <author> <time> <zone>\t<reason>`.
/// It is read from the end backwards because the last one is the current
/// one, and at most `cap` moves are taken: a panel is not a history.
///
/// An empty reflog —a freshly created repository, with no commits— is not
/// an error: it returns a state with no commit, and the panel says so.
#[must_use]
pub fn del_reflog(raw: &[u8], cap: usize) -> (Option<String>, Vec<Move>) {
    let Ok(text) = core::str::from_utf8(raw) else {
        return (None, Vec::new());
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let commit = lines.last().and_then(|l| {
        let mut fields = l.split(' ');
        let _before = fields.next()?;
        let after = fields.next()?;
        Some(abbreviate(after))
    });
    let mut moves = Vec::new();
    for line in lines.iter().rev().take(cap) {
        let Some((head, reason)) = line.split_once('\t') else {
            continue;
        };
        let mut fields = head.split(' ');
        let (Some(_before), Some(after)) = (fields.next(), fields.next()) else {
            continue;
        };
        moves.push(Move {
            to: abbreviate(after),
            reason: reason.trim().to_string(),
        });
    }
    (commit, moves)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFLOG: &[u8] = b"0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 Oscar <o@x> 1700000000 +0200\tcommit (initial): first\n\
1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 Oscar <o@x> 1700000100 +0200\tcheckout: moving from main to feat/x\n";

    /// A detached `HEAD` does not invent a branch.
    #[test]
    fn detached_head_has_no_branch() {
        assert!(head_branch(b"9f1c2a0e9f1c2a0e\n").is_none());
        assert!(head_branch(b"").is_none());
        assert!(head_branch(b"ref: refs/tags/v1\n").is_none());
    }

    /// The commit is the DESTINATION of the last line, not the origin.
    ///
    /// It is this format's easy mistake: every line carries both, and
    /// keeping the first one shows the previous commit as if it were the
    /// current one.
    #[test]
    fn the_commit_is_the_destination_of_the_last_line() {
        let (commit, _) = del_reflog(REFLOG, 5);
        assert_eq!(commit.as_deref(), Some("222222222222"));
    }

    /// Moves go from NEWEST to oldest, and are capped.
    #[test]
    fn moves_go_from_newest_to_oldest() {
        let (_, moves) = del_reflog(REFLOG, 5);
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].reason, "checkout: moving from main to feat/x");
        assert_eq!(moves[1].reason, "commit (initial): first");

        let (_, one) = del_reflog(REFLOG, 1);
        assert_eq!(one.len(), 1, "the cap rules");
        assert_eq!(one[0].reason, "checkout: moving from main to feat/x");
    }

    /// An empty reflog is not an error: it is a repository with no commits.
    #[test]
    fn an_empty_reflog_is_not_an_error() {
        let (commit, moves) = del_reflog(b"", 5);
        assert!(commit.is_none());
        assert!(moves.is_empty());
    }

    /// A line without a tab is not a move, and does not bring down the rest.
    #[test]
    fn a_broken_line_is_skipped_without_bringing_down_the_rest() {
        let mut raw = Vec::from(&b"garbage without a tab\n"[..]);
        raw.extend_from_slice(REFLOG);
        let (commit, moves) = del_reflog(&raw, 5);
        assert!(commit.is_some());
        assert_eq!(moves.len(), 2);
    }
}
