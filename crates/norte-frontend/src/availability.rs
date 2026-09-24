//! Whether a command can run RIGHT NOW, and why not.
//!
//! One table, two frontends. The GUI dims a context-menu entry and the TUI
//! dims a help row for the same reasons, and they must agree: a menu that
//! greys out "copy" while the help page says it is available is worse than
//! either alone. The vocabulary is [`norte_help::Reason`], so the third
//! consumer — the help corpus' own rendering — reads the same answers.
//!
//! The table is a function of FACTS the caller gathers, never of state it
//! reaches for: a verdict computed while a menu is open must not change under
//! the reader's cursor, and the frontends disagree about how to compute some
//! of the facts (a `.zip` is enterable in the TUI and not in the GUI).
//!
//! What the table deliberately does NOT decide:
//!
//! - **Labels and painting.** [`verdict`] is keyed by command id and returns a
//!   verdict; each frontend keeps its own wording and its own dim style.
//! - **Which commands exist.** An id the table has no arm for is available
//!   (see [`verdict`]): the table is a list of known IMPEDIMENTS, not a
//!   registry of commands.
//! - **Impediments of STATE.** This is the boundary of what dimming MEANS
//!   here, and it is a decision rather than a gap. The table models
//!   impediments of BACKEND (the location refuses mutation) and of TARGET (the
//!   entry the command would act on is the wrong kind, or there are too many
//!   of them). It never models "there is nothing to do right now": `task.cancel`
//!   with no task running, `nav.parent` at the root, `nav.back` and
//!   `nav.forward` on an empty trail are all knowable no-ops in common states,
//!   and all four stay lit. Two reasons. A trail that is empty AT THIS INSTANT
//!   is not the same kind of fact as a backend that cannot write — the first
//!   changes with the next keystroke and the second needs the reader to go
//!   somewhere else — and a reader who learns "dimmed means it does not apply
//!   here" from one row must not meet a row where it meant "not yet". The
//!   frontends have all of these facts in hand and could fill them cheaply;
//!   the reason they do not is this paragraph, not the cost. Pinned by
//!   `the_table_does_not_model_state_impediments`.
//!
//! One veto is knowable, in scope, and DEFERRED: `pane.open` (the TUI's F4)
//! refuses anything the openers cannot resolve to a native path, and
//! [`Reason::Unsupported`] already has its Fluent strings. It needs a seventh
//! field in [`Facts`] — "the focused entry has a native path" — which no
//! caller computes today, so it arrives with the change that adds it rather
//! than being quietly missing.

use norte_help::{Availability, Reason};

/// What the frontend knows about the current context and the table needs in
/// order to decide what can run.
///
/// Every field is a boolean the CALLER computes, never raw state the table
/// interprets. That is not indirection for its own sake: the frontends answer
/// some of these questions differently and both answers are right. In the TUI
/// a `.zip` file is enterable (`nav.enter` composes an archive scheme onto
/// it); in the GUI it is not, because the GUI has no archive composition on
/// Enter. A `kind: EntryKind` in here would force the table to pick one of the
/// two and be wrong in the other frontend. [`Facts::rename_single`] is the
/// second instance of exactly that, so the pattern is the rule here and not an
/// exception made once.
///
/// (`clippy::struct_excessive_bools`: allowed on purpose. These are seven
/// INDEPENDENT observations about one moment, not the states of a machine —
/// any combination of them is a real context, so there is no enum to collapse
/// them into. Wrapping each in a two-variant enum would make every call site
/// read `Enterable::No, Viewable::Yes, RenameSingle::Yes` for no gain: the
/// field names already say which question each answer belongs to.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "field names already say which question each answer belongs to"
)]
pub struct Facts {
    /// The focused entry can be entered (a directory, or an archive the
    /// frontend knows how to compose a scheme for).
    pub enterable: bool,
    /// The focused entry has something to show in a viewer.
    pub viewable: bool,
    /// Exactly one entry will be renamed.
    ///
    /// Not "exactly one entry is selected", because the two frontends TARGET
    /// differently: the GUI's rename refuses a multiple selection, while the
    /// TUI's renames the entry under the CURSOR and ignores the marks
    /// entirely. Same shape as [`Facts::enterable`] — the table cannot know, so
    /// the caller says. The field is deliberately about the rename and not
    /// about the selection: a generic `single` read by this one arm invited the
    /// TUI to fill it from its marked set, which dimmed `pane.rename` for a
    /// batch the TUI renames one entry of quite happily.
    pub rename_single: bool,
    /// The pane the command reads FROM refuses mutation.
    pub source_read_only: bool,
    /// The pane the command writes TO refuses mutation.
    pub dest_read_only: bool,
    /// The connection behind the acting pane is degraded.
    ///
    /// Gathered but NOT a veto today, and that is a decision rather than an
    /// omission: `connection.degraded` on the wire means the session is
    /// UNENCRYPTED (`tls-auth-rejected`, `ftp-plaintext` — see
    /// `norte_proto::methods::ConnectionDegraded`), not that it cannot act. A
    /// plaintext FTP session copies, moves and deletes perfectly well, so
    /// dimming those rows would tell every FTP user that the app refuses what
    /// it is about to do — over-dimming, which misleads exactly as much as
    /// not dimming at all. It stays in [`Facts`] because the caller already
    /// has it and because a future wire reason that DOES prevent acting (an
    /// unreachable or reconnecting session) belongs in this slot without
    /// changing the shape of the struct.
    pub degraded: bool,
    /// Mutations through this backend are recorded in a journal, so they can
    /// be undone.
    ///
    /// `false` for the TUI's in-process engine — the one `norte-tui` builds
    /// without `--daemon`. Since #167 that engine DOES carry the state
    /// directory's journal, but it still installs no sync spool, and `sync.plan`
    /// refuses without one; this field gates synchronising, which needs both, so
    /// it stays `false` and the name undersells what it answers. It is an
    /// impediment of BACKEND and not of state, which is why it belongs here
    /// and not in the paragraph this module's docs write against: it does not
    /// change with the next keystroke, the reader has to start norte
    /// differently.
    ///
    /// Only `pane.sync-dirs` reads it today. Copying, moving and deleting
    /// stay lit without a journal because they have always worked that way
    /// and dimming them would be a new claim about a pre-existing gap;
    /// synchronising is the first mutation the core itself refuses without one
    /// (`sync.apply` needs the journal to open a batch), so this is the field
    /// that says so before the reader presses the key.
    pub journalled: bool,
    /// Whether this process talks to the DAEMON, and not to its embedded
    /// core (phase 9).
    ///
    /// Read by `app.handoff`, and it is a different question from
    /// `journalled`: that one says whether mutations get recorded, this one
    /// whether there is a daemon to share the session with. A handoff with
    /// no daemon does not just look bad: there is nobody to hand the screen
    /// to, so the other frontend would open blank — and that has to be SAID
    /// before the key, not after.
    pub daemon: bool,
    /// Whether there is a desktop to ask for a window.
    ///
    /// `false` over SSH, which is the case that matters: there the handoff
    /// has nowhere to go, and offering it would be offering a window nobody
    /// is going to see. Decided by the caller because the answer belongs to
    /// the process's environment (`DISPLAY`, `WAYLAND_DISPLAY`) and not to
    /// anything this table can look at.
    pub windowed: bool,
}

/// Fluent id for a reason, WITHOUT a frontend prefix: both frontends read
/// these keys now, so a `gui-` one would have the TUI shipping GUI strings.
///
/// The `match` has a wildcard ON PURPOSE: [`Reason`] is `#[non_exhaustive]`,
/// so a variant added later must not break the build of every frontend. It
/// falls back to `reason-unavailable` ("unavailable right now"), which is
/// safe because it is true of every reason by construction — a row is only
/// asked for its key when it is already unavailable, so the fallback loses
/// detail and never states anything false.
#[must_use]
pub fn reason_key(reason: Reason) -> &'static str {
    match reason {
        Reason::ReadOnlyBackend => "reason-read-only",
        Reason::Unsupported => "reason-unsupported",
        Reason::PluginInactive => "reason-plugin-inactive",
        Reason::PolicyDenied => "reason-policy-denied",
        Reason::ConnectionDegraded => "reason-connection-degraded",
        Reason::WrongTarget => "reason-wrong-target",
        Reason::NeedsDaemon => "reason-needs-daemon",
        Reason::NeedsDesktop => "reason-needs-desktop",
        Reason::AnsweredByTheOverlay => "reason-answered-by-overlay",
        _ => "reason-unavailable",
    }
}

/// Whether a location on this scheme refuses mutation, decided SYNTACTICALLY.
///
/// Today that means "inside an archive" (`zip+file`, `tar+gz+file`…, ADR
/// 0018/0028): the archive provider announces `READ_ONLY` and no mutation ever
/// leaves it, so the scheme alone is enough to know.
///
/// It exists for the moment BEFORE the capability flags have arrived. A
/// frontend that caches `Capabilities` per connection should prefer the flags
/// and fall back here (see `norte_tui::app::App::pane_read_only`); one that
/// caches nothing has only this.
///
/// It dims only what is read-only BY CONSTRUCTION, and everything else it
/// reports writable: a read-only SFTP export and an S3 bucket the credentials
/// cannot write to both come back `false` here. That is the safe direction and
/// it is the reason the fallback is allowed to be this crude — every wrong
/// answer offers an operation that then refuses with a real error the reader
/// sees, whereas the opposite mistake, dimming a location that would have
/// accepted the write, teaches the reader that the app cannot do something it
/// can and is not corrected by anything.
///
/// ```
/// use norte_frontend::availability::scheme_is_read_only;
///
/// assert!(scheme_is_read_only("zip+file"));
/// assert!(scheme_is_read_only("tar+gz+file"));
/// assert!(!scheme_is_read_only("file"));
/// assert!(!scheme_is_read_only("s3"));
/// ```
#[must_use]
pub fn scheme_is_read_only(scheme: &str) -> bool {
    norte_proto::scheme_archive_format(scheme).is_some()
}

/// Whether a location refuses mutation: the flags if they have arrived, the
/// scheme if they have not.
///
/// The two-step answer both frontends need, in ONE place. Each had written it
/// out — `norte_tui::app::App::pane_read_only` and the GUI host's
/// `solo_read` — which is the shape ADR 0077 exists to stop: two spellings
/// of one decision, drifting quietly. `enter_target` moved here for the same
/// reason and in the same change.
///
/// The ORDER is the decision. The provider's own answer wins, because it knows
/// about a read-only export or an `ro` mount that no scheme can express; the
/// scheme answers only while nothing has come back, and it errs towards
/// writable (see [`scheme_is_read_only`] for why that direction is the safe
/// one).
///
/// ```
/// use norte_frontend::availability::read_only;
/// use norte_proto::{Capabilities, CapabilityFlags};
///
/// let ro = Capabilities { flags: CapabilityFlags::READ_ONLY, max_path: None };
/// let rw = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
///
/// // The flag governs, in both directions.
/// assert!(read_only(Some(ro), "sftp"));
/// assert!(!read_only(Some(rw), "sftp"));
/// // No answer yet, the scheme answers.
/// assert!(read_only(None, "zip+file"));
/// assert!(!read_only(None, "sftp"));
/// ```
#[must_use]
pub fn read_only(caps: Option<norte_proto::Capabilities>, scheme: &str) -> bool {
    caps.map_or_else(
        || scheme_is_read_only(scheme),
        |c| c.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
    )
}

/// Disabled for `reason`.
fn no(reason: Reason) -> Availability {
    Availability::Unavailable { reason }
}

/// `Available` if `ok`, otherwise disabled for `reason`.
fn gated(ok: bool, reason: Reason) -> Availability {
    if ok {
        Availability::Available
    } else {
        no(reason)
    }
}

/// The FIRST reason whose condition holds, or `Available` if none does. The
/// list's order is the order of importance: with two impediments at once,
/// the one the user would have to resolve first is explained (dropping
/// marks does not make a zip writable).
fn first_failure(checks: &[(bool, Reason)]) -> Availability {
    match checks.iter().find(|(hit, _)| *hit) {
        Some((_, reason)) => no(*reason),
        None => Availability::Available,
    }
}

/// Whether `command` can run under `facts`, and why not.
///
/// An id with no arm here is [`Availability::Available`]. That fail-OPEN
/// default is the opposite of the usual one and it is deliberate: the table
/// cannot know every command in the vocabulary (plugins contribute their own,
/// and the help corpus names commands this crate never sees), so dimming by
/// default would turn every new command into one the help declares broken.
/// Offering it and letting it fail honestly beats denying it out of ignorance.
///
/// ```
/// use norte_frontend::availability::{Facts, verdict};
/// use norte_help::Reason;
///
/// let in_a_zip = Facts {
///     enterable: false,
///     viewable: true,
///     rename_single: true,
///     source_read_only: true,
///     dest_read_only: false,
///     degraded: false,
///     journalled: true,
///     daemon: true,
///     windowed: true,
/// };
/// // Reading FROM the zip: copying is fine.
/// assert!(verdict("pane.copy", &in_a_zip).is_available());
/// // Writing INSIDE the zip: deleting is not.
/// assert_eq!(
///     verdict("pane.delete", &in_a_zip).reason(),
///     Some(Reason::ReadOnlyBackend)
/// );
/// // A command the table does not know is offered.
/// assert!(verdict("app.quit", &in_a_zip).is_available());
/// ```
#[must_use]
pub fn verdict(command: &str, facts: &Facts) -> Availability {
    match command {
        // Entering: the caller says what is enterable, and both frontends
        // ask the same place ([`crate::nav::enter_target`]) — a directory,
        // a symlink, and a container, which is what the scheme composes
        // onto. The table does not look at the count again: the frontend
        // that requires a SINGLE entry already folded that into the fact.
        "nav.enter" => gated(facts.enterable, Reason::WrongTarget),
        "pane.view" => gated(facts.viewable, Reason::WrongTarget),
        // Copying READS from the source (a zip is fine) and WRITES to the
        // destination.
        "pane.copy" => gated(!facts.dest_read_only, Reason::ReadOnlyBackend),
        // Moving writes to BOTH: it deletes at the source.
        "pane.move" => gated(
            !facts.dest_read_only && !facts.source_read_only,
            Reason::ReadOnlyBackend,
        ),
        // The real rename (`pane.rename`, shift+F6): ONE entry. WHICH one,
        // and whether several marks count as one, is said by the caller
        // (`rename_single`), because the two frontends target differently
        // — the GUI refuses on a multiple selection, the TUI renames the
        // one under the cursor and ignores the marks.
        //
        // The ORDER of these two checks is the decision, not a detail:
        // inside a zip with three marks, "it is read-only" is what the user
        // would have to resolve first (dropping the marks does not make a
        // zip writable), so the backend wins.
        "pane.rename" => first_failure(&[
            (facts.source_read_only, Reason::ReadOnlyBackend),
            (!facts.rename_single, Reason::WrongTarget),
        ]),
        // Both write to the SOURCE and only the source vetoes them. AI
        // rename additionally acts on the WHOLE folder, not on the marked
        // target, so unlike `pane.rename` the count does not affect it
        // (which is why it does not share an arm with it).
        //
        // `pane.delete-permanent` (shift+F8) belongs to the TUI's
        // vocabulary and not to the GUI's menu, but it is vetoed by the
        // SAME criterion: deleting past the trash is still writing to the
        // source. Without this arm the copy page dimmed F8 and left
        // shift+F8 lit inside a zip — two adjacent rows saying the opposite
        // of each other.
        //
        // `pane.mkdir` (F7) creates INSIDE the focused pane, which is the
        // source: same veto and for the same reason. Without an arm it fell
        // into the fail-OPEN and the reader got as far as typing the name
        // into the modal before the dispatch failed.
        // Organize (phase 8) CREATES folders and moves files inside the
        // focused pane: same veto as rename, and for the same reason.
        "pane.ai-rename"
        | "pane.organize"
        | "pane.delete"
        | "pane.delete-permanent"
        | "pane.mkdir" => gated(!facts.source_read_only, Reason::ReadOnlyBackend),
        // Synchronizing (spec 2, item 1): writes to the DESTINATION —like
        // copying— and also deletes and overwrites there, so the core
        // requires a journal (`sync.apply` opens an undoable batch; hard
        // rule 4) and refuses without one. The order is the decision, same
        // as in `pane.rename`: with no daemon there is nothing the reader
        // can fix by staying where they are, and "this destination does not
        // write" is advice for a session that could actually synchronize.
        // Without this arm it fell into the fail-OPEN and the reference
        // sheet offered the key the embedded engine rejects.
        "pane.sync-dirs" => first_failure(&[
            (!facts.journalled, Reason::NeedsDaemon),
            (facts.dest_read_only, Reason::ReadOnlyBackend),
        ]),
        // The HANDOFF between frontends (phase 9). Two different
        // impediments, and the order says which one is shown: with no
        // daemon there is nobody to hand the screen to —the other frontend
        // would open blank—, and with no desktop there is nowhere to put
        // it. The two are fixed in different ways (start against the
        // daemon; sit at the machine), so they are told apart instead of
        // saying "unavailable".
        //
        // Without this arm it fell into the fail-OPEN, and the palette
        // offered over SSH a command that drops the session and launches a
        // window nobody sees —the worse of the two failures, because it
        // leaves the screen ownerless.
        "app.handoff" => first_failure(&[
            (!facts.daemon, Reason::NeedsDaemon),
            (!facts.windowed, Reason::NeedsDesktop),
        ]),
        // Two different things land here, and they should not be confused
        // when reading: commands that have NO possible impediment
        // (`pane.copy-path` never touches the backend — it is fine even
        // inside a zip; neither does `app.quit`) and ones this table does
        // not know, which are offered by the fail-OPEN documented above.
        // They carry no arm of their own because the verdict would be
        // identical and clippy does not allow the redundant arm; the test
        // `a_degraded_connection_does_not_veto_on_its_own` pins the one for
        // `pane.copy-path`.
        _ => Availability::Available,
    }
}

/// The plugin id inside a palette dispatch key, `plugin:{id}:{command}`.
///
/// `None` for anything that is not one of those keys. A key with the prefix
/// but no `id:command` after it returns `None` too, and the caller treats that
/// as inactive: a malformed key names no plugin, so there is nothing that
/// could run it.
///
/// # Where the boundary is
///
/// The FIRST `:` after the prefix, and that is not a coin toss. The two halves
/// are validated differently and the split has to follow the asymmetry: the
/// core restricts `plugin_id` to reverse-DNS (`[A-Za-z0-9-]` segments, so
/// never a `:`), while `command_id` comes out of the manifest with NO charset
/// validation and may carry any byte, colons included — see
/// [`crate::palette::Row`], which builds these keys. Splitting on the LAST
/// colon, or splitting more than once, would attribute
/// `plugin:acme.ftp:do:it` to a plugin that does not exist.
///
/// The one input where this disagrees with `norte_help`'s `is_own_command` is
/// a plugin id that itself contains a `:`. That crate refuses such an id
/// outright (it makes the split ambiguous, so it fails closed and the plugin
/// gets no command rows at all); this function cannot see the ambiguity from
/// the key alone and reports the segment before the first colon. The
/// divergence is documented rather than papered over because it is
/// unreachable from a validated id and because the two failures point the
/// same way in practice: a `help.md` whose plugin id carries a colon yields
/// zero command rows, so there is no verdict left for this function to get
/// wrong on that path.
///
/// ```
/// use norte_frontend::availability::plugin_of_command;
///
/// assert_eq!(plugin_of_command("plugin:acme.ftp:sync"), Some("acme.ftp"));
/// // The command id may carry colons; the plugin id may not.
/// assert_eq!(plugin_of_command("plugin:acme.ftp:do:it"), Some("acme.ftp"));
/// // Not a plugin key, or not a whole one.
/// assert_eq!(plugin_of_command("pane.copy"), None);
/// assert_eq!(plugin_of_command("plugin:acme.ftp"), None);
/// assert_eq!(plugin_of_command("plugin:"), None);
/// ```
#[must_use]
pub fn plugin_of_command(command: &str) -> Option<&str> {
    let rest = command.strip_prefix("plugin:")?;
    let (id, cmd) = rest.split_once(':')?;
    (!id.is_empty() && !cmd.is_empty()).then_some(id)
}

/// [`verdict`], plus the arm for plugin-contributed commands (H3e).
///
/// `active` is the set of plugin ids that are approved AND enabled — the
/// frontend's SNAPSHOT, taken when the help opened. A `plugin:` key whose
/// plugin is not in it is [`Reason::PluginInactive`]: the row stays visible,
/// because the reader is looking at that plugin's own page and "it is here but
/// switched off" is the answer they came for, and it dims because
/// `plugin.run_command` would refuse it.
///
/// The palette does NOT get this row — it filters an inactive plugin's
/// commands out entirely (see [`crate::palette::plugin_rows`]), and that
/// decision stands. A list of everything you can run has no business showing
/// what you cannot; a page ABOUT one plugin has every business saying that
/// this is the plugin's own command and it is switched off.
///
/// Malformed `plugin:` keys are inactive rather than available: the fail-OPEN
/// default of [`verdict`] exists for commands this table does not KNOW, and a
/// key that names no plugin is not unknown, it is broken.
///
/// ```
/// use norte_frontend::availability::{Facts, verdict_with_plugins};
/// use norte_help::Reason;
/// use std::collections::BTreeSet;
///
/// let facts = Facts {
///     enterable: false,
///     viewable: true,
///     rename_single: true,
///     source_read_only: false,
///     dest_read_only: false,
///     degraded: false,
///     journalled: true,
///     daemon: true,
///     windowed: true,
/// };
/// let active: BTreeSet<String> = ["acme.ftp".to_owned()].into_iter().collect();
///
/// assert!(verdict_with_plugins("plugin:acme.ftp:sync", &facts, &active).is_available());
/// assert_eq!(
///     verdict_with_plugins("plugin:other:sync", &facts, &active).reason(),
///     Some(Reason::PluginInactive)
/// );
/// // A built-in command never looks at the set.
/// assert!(verdict_with_plugins("pane.copy", &facts, &BTreeSet::new()).is_available());
/// ```
#[must_use]
pub fn verdict_with_plugins(
    command: &str,
    facts: &Facts,
    active: &std::collections::BTreeSet<String>,
) -> Availability {
    if command.starts_with("plugin:") {
        let ok = plugin_of_command(command).is_some_and(|id| active.contains(id));
        return gated(ok, Reason::PluginInactive);
    }
    verdict(command, facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::Reason;

    fn one_file() -> Facts {
        Facts {
            enterable: false,
            viewable: true,
            rename_single: true,
            source_read_only: false,
            dest_read_only: false,
            degraded: false,
            journalled: true,
            daemon: true,
            windowed: true,
        }
    }

    #[test]
    fn copying_to_a_read_only_destination_is_vetoed() {
        let f = Facts {
            dest_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.copy", &f).reason(),
            Some(Reason::ReadOnlyBackend)
        );
        // And the other way around: reading FROM a read-only source is
        // exactly the case the function exists to allow — copying from a
        // .zip to a bucket.
        let from_zip = Facts {
            source_read_only: true,
            ..one_file()
        };
        assert!(verdict("pane.copy", &from_zip).is_available());
    }

    #[test]
    fn rename_reports_the_first_failure_not_the_last() {
        // The order is the decision: with a batch of 3 files inside a zip,
        // "it is read-only" explains more than "there is more than one".
        let f = Facts {
            rename_single: false,
            source_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.rename", &f).reason(),
            Some(Reason::ReadOnlyBackend)
        );
    }

    /// The SAME divergence as `enterable`, over rename: the GUI refuses on a
    /// multiple selection, the TUI renames the one under the cursor and
    /// ignores the marks. With a generic `single` deciding this arm, the
    /// TUI's help dimmed shift+F6 as soon as there were two marks — a dimmed
    /// row for something the app does without blinking, which is exactly
    /// the mistake H3d exists to not make.
    #[test]
    fn rename_is_decided_by_the_caller_not_the_count() {
        let tui_with_marks = Facts {
            rename_single: true,
            ..one_file()
        };
        assert!(
            verdict("pane.rename", &tui_with_marks).is_available(),
            "the TUI renames the one under the cursor: it is not dimmed"
        );
        let gui_with_marks = Facts {
            rename_single: false,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.rename", &gui_with_marks).reason(),
            Some(Reason::WrongTarget),
            "the GUI refuses on a multiple selection"
        );
    }

    #[test]
    fn entering_is_decided_by_the_caller_not_the_entry_kind() {
        // The REAL divergence between frontends: in the TUI a .zip is
        // ENTERED (the scheme is composed), in the GUI it is not. That is
        // why the fact is "enterable", not "is a directory".
        let zip = Facts {
            enterable: true,
            ..one_file()
        };
        assert!(verdict("nav.enter", &zip).is_available());
        assert_eq!(
            verdict("nav.enter", &one_file()).reason(),
            Some(Reason::WrongTarget)
        );
    }

    #[test]
    fn a_command_the_table_does_not_know_is_available() {
        // Fail-OPEN on purpose, and it is the opposite of what is usually
        // asked for: the table cannot know every command in the vocabulary,
        // and dimming by default would turn every new command into one the
        // help declares broken. Offering it and having it fail honestly is
        // better than denying it out of ignorance.
        assert!(verdict("app.quit", &one_file()).is_available());
        assert!(verdict("no.such.command", &one_file()).is_available());
    }

    /// The HANDOFF (phase 9) says WHICH of the two things is missing, and in
    /// that order.
    ///
    /// Without this arm it fell into the fail-OPEN, and the palette offered
    /// over SSH a command that drops the screen and launches a window
    /// nobody sees. The two reasons are told apart because they are fixed
    /// in different ways: start against the daemon, or sit at the machine.
    #[test]
    fn the_handoff_says_whether_the_daemon_or_the_desktop_is_missing() {
        let complete = Facts {
            daemon: true,
            windowed: true,
            ..one_file()
        };
        assert!(verdict("app.handoff", &complete).is_available());

        let no_daemon = Facts {
            daemon: false,
            ..complete
        };
        assert_eq!(
            verdict("app.handoff", &no_daemon),
            Availability::Unavailable {
                reason: Reason::NeedsDaemon
            }
        );

        let no_desktop = Facts {
            windowed: false,
            ..complete
        };
        assert_eq!(
            verdict("app.handoff", &no_desktop),
            Availability::Unavailable {
                reason: Reason::NeedsDesktop
            }
        );

        // With both missing, the daemon governs: it is the first thing to
        // fix, and telling someone who also has no daemon "you need a
        // desktop" makes them fix the wrong thing.
        let neither = Facts {
            daemon: false,
            windowed: false,
            ..complete
        };
        assert_eq!(
            verdict("app.handoff", &neither),
            Availability::Unavailable {
                reason: Reason::NeedsDaemon
            }
        );
    }

    #[test]
    fn every_reason_has_a_fluent_key_and_none_overlap() {
        use std::collections::BTreeSet;
        let mut seen = BTreeSet::new();
        for r in [
            Reason::ReadOnlyBackend,
            Reason::NeedsDesktop,
            Reason::Unsupported,
            Reason::PluginInactive,
            Reason::PolicyDenied,
            Reason::ConnectionDegraded,
            Reason::WrongTarget,
            Reason::NeedsDaemon,
        ] {
            let k = reason_key(r);
            assert!(!k.is_empty(), "{r:?} has no key");
            assert!(seen.insert(k), "repeated key: {k}");
        }
    }

    /// Every key [`reason_key`] names exists in BOTH locales — `norte-i18n`'s
    /// parity test covers the whole catalogue, this covers that these keys
    /// are REAL keys and not a typo that would reach the UI as its own id.
    #[test]
    fn reason_keys_exist_in_both_locales() {
        for key in [
            "reason-read-only",
            "reason-unsupported",
            "reason-plugin-inactive",
            "reason-policy-denied",
            "reason-connection-degraded",
            "reason-wrong-target",
            "reason-needs-daemon",
            "reason-unavailable",
        ] {
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                assert_ne!(
                    norte_i18n::t_in(lang, key),
                    key,
                    "missing {key} in {lang:?}"
                );
            }
        }
    }

    /// Deleting without the trash is vetoed like deleting: inside a zip
    /// BOTH rows of the copy page (F8 and shift+F8) have to say the same
    /// thing — whichever stayed lit would promise the more destructive one.
    #[test]
    fn permanent_delete_is_vetoed_like_delete() {
        let inside_a_zip = Facts {
            source_read_only: true,
            ..one_file()
        };
        for cmd in ["pane.delete", "pane.delete-permanent"] {
            assert_eq!(
                verdict(cmd, &inside_a_zip).reason(),
                Some(Reason::ReadOnlyBackend),
                "{cmd} offered inside a read-only backend"
            );
        }
    }

    /// MAJOR-3(a): creating a directory WRITES to the focused pane, so it
    /// is vetoed by the same criterion as deleting. Without an arm it fell
    /// into the fail-OPEN and inside a zip the help offered F7: the reader
    /// types a name into the modal, confirms it and the dispatch fails. It
    /// is the SAME argument the phase itself used to add
    /// `pane.delete-permanent` — two adjacent rows saying the opposite of
    /// each other.
    #[test]
    fn creating_a_directory_is_vetoed_like_writing() {
        let inside_a_zip = Facts {
            source_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.mkdir", &inside_a_zip).reason(),
            Some(Reason::ReadOnlyBackend),
        );
        assert!(
            verdict("pane.mkdir", &one_file()).is_available(),
            "outside a read-only backend it is offered"
        );
    }

    /// MAJOR-3(b): the table models BACKEND and TARGET impediments, and
    /// never STATE ones. These four are KNOWN no-ops in common states
    /// —nothing running, at the root, no trail— and are still offered: a
    /// trail that is empty RIGHT NOW is not the same kind of fact as a
    /// backend that cannot write, and the reader who sees a dimmed row
    /// learns "this cannot be done here", not "this has nothing to do yet".
    /// The decision is written in the module's rustdoc; this test is where
    /// it gets changed if it is ever decided otherwise.
    #[test]
    fn the_table_does_not_model_state_impediments() {
        for cmd in ["task.cancel", "nav.parent", "nav.back", "nav.forward"] {
            assert!(
                verdict(cmd, &one_file()).is_available(),
                "{cmd} dimmed by a STATE impediment"
            );
        }
    }

    /// Syncing with no journal is TURNED OFF, with a reason that can be
    /// acted on: start norte against the daemon. It is not a state
    /// impediment —it does not change with the next key— but a backend one,
    /// which is the class this table does model. The TUI's embedded engine
    /// has no journal or spool and `sync.apply` refuses closed (hard rule
    /// 4), so without this arm the reference sheet offered a dead key —
    /// which is exactly what #159 just cost once.
    #[test]
    fn syncing_with_no_journal_sends_to_the_daemon() {
        let embedded = Facts {
            journalled: false,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.sync-dirs", &embedded).reason(),
            Some(Reason::NeedsDaemon)
        );
        assert!(
            verdict("pane.sync-dirs", &one_file()).is_available(),
            "with a journal it is offered"
        );
        // And comparing does NOT turn off for the same reason: reading the
        // two trees mutates nothing, so it needs no journal. Two
        // neighboring commands that say different things because they are
        // different things.
        assert!(verdict("pane.compare-dirs", &embedded).is_available());
    }

    /// With a daemon but against a destination that does not write, the
    /// reason is the destination's. The ORDER matters: with no daemon there
    /// is nothing the reader can fix by staying where they are, so that one
    /// wins even when both hold.
    #[test]
    fn syncing_reports_the_first_failure_not_the_last() {
        let towards_a_zip = Facts {
            dest_read_only: true,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.sync-dirs", &towards_a_zip).reason(),
            Some(Reason::ReadOnlyBackend)
        );
        let neither_one = Facts {
            dest_read_only: true,
            journalled: false,
            ..one_file()
        };
        assert_eq!(
            verdict("pane.sync-dirs", &neither_one).reason(),
            Some(Reason::NeedsDaemon)
        );
    }

    fn active_set(ids: &[&str]) -> std::collections::BTreeSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_disabled_plugins_command_is_dimmed() {
        let v = verdict_with_plugins(
            "plugin:acme.ftp:sync",
            &one_file(),
            &active_set(&["otro.plugin"]),
        );
        assert_eq!(v.reason(), Some(Reason::PluginInactive));
    }

    #[test]
    fn an_active_plugins_command_is_offered() {
        let v = verdict_with_plugins(
            "plugin:acme.ftp:sync",
            &one_file(),
            &active_set(&["acme.ftp"]),
        );
        assert!(v.is_available());
    }

    #[test]
    fn a_built_in_commands_does_not_look_at_plugins() {
        let v = verdict_with_plugins("pane.copy", &one_file(), &active_set(&[]));
        assert!(v.is_available(), "the `plugin:` prefix is what decides");
    }

    #[test]
    fn a_malformed_plugin_key_is_not_offered() {
        // `plugin:` with neither id nor command identifies nothing:
        // fail-closed, because the only possible dispatch would be against
        // a plugin that does not exist.
        let v = verdict_with_plugins("plugin:", &one_file(), &active_set(&["acme.ftp"]));
        assert_eq!(v.reason(), Some(Reason::PluginInactive));
    }

    /// The boundary is marked by the FIRST `:` after `plugin:`, and that is
    /// not a detail: `plugin_id` is reverse-DNS validated by the core
    /// (never carries a `:`), while `command_id` comes out of the manifest
    /// with NO charset validation and can carry as many as it likes.
    /// Splitting on the last one, or splitting more than once, would
    /// attribute `plugin:acme.ftp:do:it` to a plugin that does not exist
    /// and would dim a row that can actually run.
    #[test]
    fn the_commands_id_can_carry_colons() {
        assert_eq!(
            plugin_of_command("plugin:acme.ftp:do:it"),
            Some("acme.ftp"),
            "the boundary is the first `:`, not the last"
        );
        assert!(
            verdict_with_plugins(
                "plugin:acme.ftp:do:it",
                &one_file(),
                &active_set(&["acme.ftp"])
            )
            .is_available()
        );
    }

    /// Every form that does not name a plugin AND a command is `None`, and
    /// the verdict for all of them is the same: off. The list is the
    /// contract —what the table considers "broken" versus "unknown"— and
    /// that is why it is enumerated here and not deduced from the
    /// implementation.
    #[test]
    fn keys_that_name_no_plugin_and_command_are_none() {
        for key in [
            "plugin:",
            "plugin::",
            "plugin:acme.ftp",
            "plugin:acme.ftp:",
            "plugin::sync",
        ] {
            assert_eq!(plugin_of_command(key), None, "{key} identified something");
            assert_eq!(
                verdict_with_plugins(key, &one_file(), &active_set(&["acme.ftp", ""])).reason(),
                Some(Reason::PluginInactive),
                "{key} offered"
            );
        }
        // And what does not carry the prefix is none of its business.
        assert_eq!(plugin_of_command("pane.copy"), None);
        assert_eq!(plugin_of_command("plugins:acme.ftp:sync"), None);
    }

    /// The SYNTACTIC criterion: an archive-composed scheme is read-only by
    /// construction; a provider one is not.
    #[test]
    fn the_archive_scheme_is_read_only() {
        assert!(scheme_is_read_only("zip+file"));
        assert!(scheme_is_read_only("tar+file"));
        assert!(scheme_is_read_only("tar+gz+file"));
        assert!(!scheme_is_read_only("file"));
        assert!(!scheme_is_read_only("sftp"));
        assert!(!scheme_is_read_only("s3"));
        assert!(!scheme_is_read_only("mem"));
    }

    /// A degraded connection does NOT veto anything, and that is pinned on
    /// purpose: `connection.degraded` means "unencrypted session", not
    /// "unusable session". Dimming copy/move/delete for it would tell every
    /// FTP user that the app refuses to do what it is about to do. If the
    /// wire reason ever comes to mean "cannot act", this test is where the
    /// decision gets changed, in plain sight.
    #[test]
    fn a_degraded_connection_does_not_veto_on_its_own() {
        let f = Facts {
            degraded: true,
            ..one_file()
        };
        for cmd in [
            "pane.copy",
            "pane.move",
            "pane.delete",
            "pane.rename",
            "pane.copy-path",
        ] {
            assert!(
                verdict(cmd, &f).is_available(),
                "{cmd} dimmed by a plaintext session"
            );
        }
    }
}
