//! The AGENT sessions this window has seen, and undoing one whole session
//! (#276).
//!
//! **Where the list comes from, and why that matters.** There is no method in
//! the protocol that enumerates live agent sessions: the only thing that
//! names them is the approval request an agent fires
//! (`policy.approval_required`, whose `session` is optional). So this list is
//! exactly "the ones THIS window has seen ask for permission", and the screen
//! itself says so — a list that presents itself as the system's census of
//! agents and is not would be worse than not having one.
//!
//! What it buys over typing the id by hand, which is what task 5.3 rejected:
//! the operand is CHOSEN. A session id typed by a human on a governance
//! surface is an id that can be mistyped, and undoing the wrong session is
//! undoing someone else's work.
//!
//! A session's id is an OPAQUE key from the daemon: it is painted masked —it
//! can carry any byte— and travels RAW, because it is the key the core
//! resolves it with.

use crate::bridge::clamp_display;
use crate::dto::{AgentRowView, AgentsView};

/// Cap on remembered sessions.
///
/// A daemon that keeps announcing requests cannot be allowed to grow this
/// without bound. The OLDEST by last-seen is forgotten first, since it is the
/// one least likely to still be doing anything.
const MAX_SESIONES: usize = 128;

/// What is known about an agent session.
#[derive(Debug, Clone)]
struct Session {
    /// The RAW id, exactly as it arrived: it is what goes back to the daemon.
    id: String,
    /// How many of its requests this window has seen.
    seen: u32,
    /// How many were approved FROM HERE.
    approved: u32,
    /// The last op-kind it asked for, already masked and with its flag.
    last: (String, bool),
    /// The order in which it was last seen: the highest wins.
    stamp: u64,
}

/// The sessions seen, and the panel open over them.
#[derive(Debug, Default)]
pub(crate) struct Agentes {
    /// What was seen, by id.
    sessions: std::collections::HashMap<String, Session>,
    /// The logical "last seen" clock.
    clock: u64,
    /// Which one is chosen, BY ID and not by position.
    ///
    /// The list reorders itself —a new request bumps its session to first
    /// place— and a selection by index means a different row the moment that
    /// happens. It is the same rule 6.2 wrote down for compared rows: they
    /// are named by id, never by position.
    selected: Option<String>,
    /// How many times the list has CHANGED.
    ///
    /// It travels with the view and comes back with the click: a click is
    /// resolved against the list the reader was looking at, not against the
    /// current one. Without this, a request arriving between the click and
    /// its delivery turns "this row" into another one — and here "this row"
    /// is whose work gets undone.
    generation: u64,
    /// How many sessions have been forgotten because of the cap.
    forgotten: u64,
    /// The sessions with an undo in progress.
    undoing: std::collections::HashSet<String>,
}

impl Agentes {
    /// Notes that this session asked for permission for `op`.
    pub(crate) fn vista(&mut self, id: &str, op: &str) {
        self.clock += 1;
        self.generation += 1;
        let stamp = self.clock;
        let (paintable, hostile) = norte_frontend::display_name(op.as_bytes());
        let entry = self
            .sessions
            .entry(id.to_owned())
            .or_insert_with(|| Session {
                id: id.to_owned(),
                seen: 0,
                approved: 0,
                last: (String::new(), false),
                stamp,
            });
        entry.seen = entry.seen.saturating_add(1);
        entry.last = (clamp_display(paintable), hostile);
        entry.stamp = stamp;
        self.prune();
    }

    /// Notes that this session had an op approved from here.
    pub(crate) fn aprobada(&mut self, id: &str) {
        if let Some(s) = self.sessions.get_mut(id) {
            s.approved = s.approved.saturating_add(1);
            self.generation += 1;
        }
    }

    /// Notes that an undo was launched for this session.
    pub(crate) fn deshaciendo(&mut self, id: &str) {
        self.undoing.insert(id.to_owned());
        self.generation += 1;
    }

    /// This session's undo finished, one way or another.
    pub(crate) fn deshecha(&mut self, id: &str) {
        if self.undoing.remove(id) {
            self.generation += 1;
        }
    }

    /// `true` if this session already has a live undo in progress.
    pub(crate) fn tiene_undo_vivo(&self, id: &str) -> bool {
        self.undoing.contains(id)
    }

    /// Forgets some when there are too many, and notes it.
    ///
    /// NOT plainly the oldest: the session id is chosen by the AGENT, and
    /// nothing stops it from reconnecting a hundred and twenty-eight times
    /// with new ids, each asking for a permission, to push out of the list
    /// exactly the session whose work someone might want to undo. What is
    /// forgotten first is whatever nobody has touched —a single request and
    /// no approval from here—, never one with an undo in progress, and the
    /// forgotten count TRAVELS: a trimmed list that presents itself as
    /// complete is what turns the attack into "that session doesn't exist".
    fn prune(&mut self) {
        while self.sessions.len() > MAX_SESIONES {
            let Some(old) = self
                .sessions
                .values()
                .filter(|s| !self.undoing.contains(&s.id))
                .min_by_key(|s| (s.approved > 0 || s.seen > 1, s.stamp))
                .map(|s| s.id.clone())
            else {
                return;
            };
            self.sessions.remove(&old);
            if self.selected.as_deref() == Some(old.as_str()) {
                self.selected = None;
            }
            self.forgotten = self.forgotten.saturating_add(1);
        }
    }

    /// The sessions in the order in which they are painted: most recent
    /// first.
    fn sorted(&self) -> Vec<&Session> {
        let mut v: Vec<&Session> = self.sessions.values().collect();
        // By descending stamp, with id as the tiebreak: two sessions cannot
        // share a stamp, but an order that depends on a `HashMap`'s
        // iteration makes the list dance between repaints.
        v.sort_by(|a, b| b.stamp.cmp(&a.stamp).then_with(|| a.id.cmp(&b.id)));
        v
    }

    /// The RAW id of the chosen session.
    pub(crate) fn elegida(&self) -> Option<String> {
        match &self.selected {
            // By ID: if the row moved —or disappeared—, the selection
            // follows it, and it does not keep pointing at whoever took its
            // spot.
            Some(id) if self.sessions.contains_key(id) => Some(id.clone()),
            _ => self.sorted().first().map(|s| s.id.clone()),
        }
    }

    /// Where the selection falls within the painted list.
    fn index(&self) -> usize {
        let order = self.sorted();
        self.selected
            .as_ref()
            .and_then(|id| order.iter().position(|s| &s.id == id))
            .unwrap_or(0)
    }

    /// Moves the selection within the list.
    pub(crate) fn mover(&mut self, delta: i64) {
        let order = self.sorted();
        if order.is_empty() {
            return;
        }
        let target = i64::try_from(self.index())
            .unwrap_or(0)
            .saturating_add(delta);
        let i = usize::try_from(target.max(0))
            .unwrap_or(0)
            .min(order.len() - 1);
        self.selected = Some(order[i].id.clone());
    }

    /// Puts the selection on a specific row (a click), if the click speaks
    /// of the list that was being painted.
    ///
    /// Out of generation it is NOT clamped nor ignored: it is refused.
    /// Clamping against a list that moved is choosing for the reader, and
    /// here what is being chosen is whose work gets undone.
    pub(crate) fn senalar(&mut self, row: usize, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        let order = self.sorted();
        let Some(s) = order.get(row) else {
            return false;
        };
        self.selected = Some(s.id.clone());
        true
    }

    /// Starts with no selection: the panel opens and closes, and the
    /// previous one described a list that may have changed entirely.
    pub(crate) fn al_abrir(&mut self) {
        self.selected = None;
    }

    /// The panel's projection.
    ///
    /// `listening` is whether this window is subscribed to the approvals
    /// channel: one that is not —mounted without effects— has an empty list
    /// FOR THAT REASON, and saying there "no agent has asked for permission"
    /// would claim something it cannot know.
    pub(crate) fn vista_de(&self, lang: norte_i18n::Lang, listening: bool) -> AgentsView {
        AgentsView {
            rows: self
                .sorted()
                .into_iter()
                .map(|s| {
                    let (id, hostile) = norte_frontend::display_name(s.id.as_bytes());
                    AgentRowView {
                        session: clamp_display(id),
                        session_hostile: hostile,
                        undoing: self.undoing.contains(&s.id),
                        counts: clamp_display(norte_i18n::ta_in(
                            lang,
                            "agents-counts",
                            &[
                                ("seen", &s.seen.to_string()),
                                ("approved", &s.approved.to_string()),
                            ],
                        )),
                        last_op: s.last.0.clone(),
                        last_op_hostile: s.last.1,
                    }
                })
                .collect(),
            cursor: self.index() as u64,
            generation: self.generation,
            forgotten: self.forgotten,
            // What this list IS, right there on the screen: the ones THIS
            // window has seen ask for permission, which is not the system's
            // census of agents. Without saying so, an empty list reads as
            // "no agent has touched anything", a claim this window cannot
            // make.
            note: clamp_display(norte_i18n::t_in(lang, "agents-note")),
            empty: clamp_display(norte_i18n::t_in(
                lang,
                if listening {
                    "agents-empty"
                } else {
                    "agents-not-listening"
                },
            )),
        }
    }
}
