//! Pointer semantics shared by both frontends: ctrl/shift marking, marking
//! by sweeping, and dragging entries from one pane to the other.
//!
//! The rules live HERE, once (hard rule 7), so that the TUI (crossterm
//! `MouseEvent`) and the GUI (GPUI `MouseDownEvent`) cannot drift into two
//! different file managers. This module knows nothing about terminals,
//! pixels, rows, rects or GPUs: its whole world is a pane number, an index
//! into that pane's [`crate::PaneState::entries`], and two modifiers. Each
//! frontend does its own hit test, feeds the machine, and applies the
//! [`Effect`]s it gets back.
//!
//! The gesture is decided at PRESS, in this order — modifiers first,
//! because a modifier is an explicit statement of intent and the row's mark
//! state is only an inference:
//!
//! 1. ctrl+press toggles that one entry ([`Effect::SetMark`]) and arms
//!    nothing: it is a discrete gesture, and it wins over everything else so
//!    that an already-marked row can always be unmarked.
//! 2. shift+press marks the range from the pane's cursor
//!    ([`Effect::MarkRange`], additive) and arms a sweep anchored at that
//!    cursor, so shift+drag keeps extending the same range. It wins over the
//!    mark state: extending a range onto a row that is ALREADY marked is a
//!    routine gesture (ctrl+click a few files, then shift+click past them),
//!    and reading it as a transfer would both drop the extension and, with
//!    shift still held at release over the other pane, turn it into a MOVE
//!    of the whole selection.
//! 3. press on a MARKED row arms a [`DragKind::Transfer`]: the pane's marks
//!    travel to the other pane. That fork — the row's mark state, not a
//!    modifier — is what lets ONE gesture do both jobs.
//! 4. press on an UNMARKED row arms a [`DragKind::MarkSweep`]: what the
//!    pointer sweeps gets marked.
//!
//! # A sweep that crosses panes is PROMOTED to a transfer
//!
//! Case 4 alone would make the commonest drag in any file manager — grab
//! one unmarked file, pull it into the other pane — transfer NOTHING: it
//! would sweep one row and mark it. So the moment the pointer of a case-4
//! sweep leaves its own pane, the gesture is promoted: a release over the
//! other pane transfers the row the button went down on
//! ([`Effect::Transfer::promoted`]), and anything the sweep marked on the
//! way out is GIVEN BACK ([`Effect::RevertSweep`]).
//!
//! Two consequences are deliberate. Promotion changes what the gesture
//! DOES, not what is selected: it never marks the pressed row, and a
//! promoted drag that is cancelled (released on chrome, or dropped by
//! [`Drag::cancel`]) leaves the marks exactly as they were before the
//! gesture. And the gesture means two things depending on where it ends, so
//! it must SAY which before the button comes up — that is what
//! [`Drag::pending`] is for, and a frontend that drops without rendering it
//! is not finished.
//!
//! A sweep armed by a shift+press is NOT promotable (case 2): shift means
//! "extend the range", the range routinely ends on the other side of the
//! pane boundary while the modifier is still down, and reading that as a
//! drop would turn a marking gesture into a MOVE of the whole selection.
//!
//! Two rules exist to keep the pointer from acting on the user's behalf:
//! a release that lands outside every row CANCELS instead of guessing a
//! destination, and a drag that never leaves the row it started on is just
//! a click. And the copy/move decision of a transfer is read from the
//! modifiers held at RELEASE, not at press, so that a user who starts
//! dragging and changes their mind does not move files they meant to copy.
//!
//! # Delivery must not change the outcome
//!
//! crossterm reports the pointer per CELL and GPUI per PIXEL, and both
//! coalesce motion under load. The same physical gesture therefore arrives
//! as a different number of events in each frontend, so every sweep effect
//! re-states the whole range instead of describing a step, the release
//! re-states it once more from the last row seen inside the anchor's pane
//! (rather than assuming any motion was delivered at all), and a motion
//! that does not change the row returns nothing. Coalescing can drop the
//! rows the pointer passed over — nothing can invent those — but it can no
//! longer change which rows the gesture ends up marking.

/// Live modifiers at the instant of an event. Only the two that mean
/// something to marking: everything else a frontend can distinguish
/// (buttons, alt, super) is either its own business or not part of these
/// gestures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    /// Control (or its platform equivalent): "toggle just this one".
    pub ctrl: bool,
    /// Shift: "extend the range" at press, "move instead of copy" at the
    /// release of a transfer.
    pub shift: bool,
}

impl Mods {
    /// No modifier held.
    pub const NONE: Self = Self {
        ctrl: false,
        shift: false,
    };
    /// Control alone.
    pub const CTRL: Self = Self {
        ctrl: true,
        shift: false,
    };
    /// Shift alone.
    pub const SHIFT: Self = Self {
        ctrl: false,
        shift: true,
    };

    /// Builds a modifier pair.
    #[must_use]
    pub const fn new(ctrl: bool, shift: bool) -> Self {
        Self { ctrl, shift }
    }
}

/// One row of one pane: everything this machine knows about the world.
///
/// `index` is the ABSOLUTE index into [`crate::PaneState::entries`], never a
/// painted row number and never a position inside a quick-search filter —
/// the frontend's hit test resolves the row it painted back to the entry it
/// came from (both frontends already render from absolute indices) before
/// calling in. Marking then re-applies the filter itself
/// ([`crate::PaneState::mark_range`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    /// Which pane, in the frontend's own numbering (norte has two).
    pub pane: usize,
    /// Index into that pane's `entries`.
    pub index: usize,
}

impl Spot {
    /// Builds a spot.
    #[must_use]
    pub const fn new(pane: usize, index: usize) -> Self {
        Self { pane, index }
    }
}

/// What the frontend knows when the button goes DOWN. A struct rather than
/// four positional arguments: two of the fields are a `usize` and the third
/// a `bool`, and a call site that swapped them would compile and quietly
/// mark the wrong rows.
#[derive(Debug, Clone, Copy)]
pub struct Press {
    /// The row under the pointer.
    pub at: Spot,
    /// Was that row ALREADY marked? The sweep/transfer fork.
    pub marked: bool,
    /// The pane's cursor at that instant: the anchor a shift+click marks
    /// from. It is deliberately the CURSOR and not hidden state — the
    /// anchor of a range is a thing the user can see on screen.
    pub cursor: usize,
    /// Modifiers held when the button went down.
    pub mods: Mods,
}

/// What an armed drag is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    /// Marking whatever the pointer sweeps over, within one pane.
    MarkSweep,
    /// Carrying the source pane's marks towards the other pane.
    ///
    /// Only a press on a MARKED row arms this. A press on an UNMARKED row
    /// arms a [`DragKind::MarkSweep`] that becomes a transfer of that ONE
    /// row if the pointer crosses into the other pane (see the module
    /// docs): the kind stays `MarkSweep` because the gesture can still come
    /// home and go back to marking — what a release would do right now is
    /// [`Drag::pending`], not this.
    Transfer,
}

/// What the frontend must do. The machine never touches a `PaneState` and
/// never submits a task: it only says what should happen, and each effect
/// maps onto exactly one existing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Focus `pane` and put its cursor on `index`
    /// ([`crate::PaneState::set_cursor`]). An effect whose `index` is where
    /// the cursor already sits still means "focus this pane".
    MoveCursor {
        /// Pane to focus.
        pane: usize,
        /// Row the cursor lands on.
        index: usize,
    },
    /// Mark (`marked = true`) or unmark one entry
    /// ([`crate::PaneState::set_mark`]). Only ctrl produces this; it is the
    /// ONLY effect that can unmark, which is why ranges never carry a flag.
    SetMark {
        /// Pane that owns the entry.
        pane: usize,
        /// Row to set.
        index: usize,
        /// New state of the mark.
        marked: bool,
    },
    /// Mark every visible entry between two rows, inclusive, in either
    /// order, ADDITIVELY ([`crate::PaneState::mark_range`]) — a shift+click
    /// extends a selection and must never take anything back.
    MarkRange {
        /// Pane that owns the entries.
        pane: usize,
        /// One end of the range (the anchor).
        from: usize,
        /// The other end (the pointer).
        to: usize,
    },
    /// Arm a sweep ([`crate::PaneState::begin_sweep`]): the pane snapshots
    /// its marks so the following [`Effect::SweepRange`]s can rubber-band
    /// against them.
    BeginSweep {
        /// Pane being swept.
        pane: usize,
    },
    /// Set the sweep's current extent ([`crate::PaneState::apply_sweep`]):
    /// the pane restores the baseline armed by [`Effect::BeginSweep`] and
    /// marks the range, so a drag that RETREATS gives back the rows it
    /// pulled off. Unlike [`Effect::MarkRange`] this is not additive, and
    /// that is the whole point: an add-only sweep leaves everything the
    /// pointer ever touched marked, and an overshoot happens at the
    /// viewport edge under autoscroll, where the surplus rows are the ones
    /// that just scrolled out of sight.
    SweepRange {
        /// Pane that owns the entries.
        pane: usize,
        /// The anchor the sweep grows from.
        from: usize,
        /// The row under the pointer.
        to: usize,
    },
    /// Give back everything the sweep in progress marked
    /// ([`crate::PaneState::revert_sweep`]), leaving the selection as it was
    /// before the gesture and the sweep still armed.
    ///
    /// Emitted when a mark sweep is PROMOTED to a transfer by crossing into
    /// the other pane (see the module docs): the rows it swept on the way
    /// out were never the point of the gesture, and leaving them marked
    /// would make a promotion change the selection behind the user's back.
    /// If the pointer comes home, the next [`Effect::SweepRange`] re-states
    /// the range against the same baseline and nothing is lost.
    RevertSweep {
        /// Pane being swept.
        pane: usize,
    },
    /// Send entries of `from_pane` to `to_pane`. The frontend routes this
    /// through the SAME task submission as the keyboard copy/move — same
    /// confirmation, same policy gate, same journal entry, same undo. A
    /// drop is a mutation, not a quieter second path.
    Transfer {
        /// Pane the entries come from.
        from_pane: usize,
        /// Pane they land in.
        to_pane: usize,
        /// `true` = move, `false` = copy. Read from the modifiers held at
        /// RELEASE.
        move_files: bool,
        /// `None` = the source pane's MARKS travel (the ordinary drag of a
        /// selection). `Some(index)` = the gesture was PROMOTED from a mark
        /// sweep and carries that ONE row instead, whatever the pane's
        /// marks are — the frontend must not read the marks in that case,
        /// or a drag of one file out of a marked selection would copy the
        /// whole selection.
        promoted: Option<usize>,
    },
}

/// What a release RIGHT NOW would do, for the feedback a frontend owes the
/// user before the button comes up: which rows, where to, and copy or move.
///
/// It is derived from the same state and the same rules as
/// [`Drag::release`], so the label cannot promise one thing and the drop do
/// another. `mods` is passed in live rather than remembered, so the answer
/// changes the instant shift goes down or up mid-drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// Marking rows in `pane`: a release changes no files.
    Marking {
        /// Pane being marked.
        pane: usize,
    },
    /// Carrying rows out of `from_pane`, with the pointer still over that
    /// same pane: a release right now does NOTHING (dropping at home is a
    /// no-op). The frontend may still highlight the source.
    Carrying {
        /// Pane the gesture started in.
        from_pane: usize,
    },
    /// A release right now DROPS: it would emit the matching
    /// [`Effect::Transfer`].
    Drop {
        /// Pane the entries come from.
        from_pane: usize,
        /// Pane they would land in.
        to_pane: usize,
        /// `true` = move, `false` = copy, from the modifiers held NOW.
        move_files: bool,
        /// Same meaning as [`Effect::Transfer::promoted`]: `Some(index)` =
        /// that one row, `None` = the source pane's marks.
        promoted: Option<usize>,
    },
}

/// An armed gesture.
#[derive(Debug, Clone, Copy)]
struct Active {
    kind: DragKind,
    /// Where the range grows FROM. Equal to `origin` except for a
    /// shift+press, which anchors at the pane's cursor.
    anchor: Spot,
    /// Where the button went down.
    origin: Spot,
    /// Last row seen INSIDE the anchor's pane. Seeded at `origin` and
    /// updated by every in-pane motion, so a release that lands outside
    /// that pane can still re-state the sweep from a row the frontend
    /// actually reported instead of assuming its motions were delivered.
    last_in_pane: Spot,
    /// Last row reported at all, to drop a motion that does not change the
    /// row. Safe precisely because every sweep effect re-states the whole
    /// range: dropping a repeat cannot lose anything.
    last: Spot,
    /// Has the pointer left `origin`? A gesture that never does is a click.
    moved: bool,
    /// May this sweep become a transfer by crossing panes? True only for
    /// the plain press on an unmarked row; a shift-armed sweep is not
    /// promotable (see the module docs). Meaningless for
    /// [`DragKind::Transfer`], which is already one.
    promotable: bool,
    /// Is the pointer OUTSIDE the anchor's pane right now? Only so the
    /// crossing emits its [`Effect::RevertSweep`] once instead of on every
    /// row it passes over on the far side.
    outside: bool,
}

/// The pointer gesture state machine: pure, allocation-light, and blind to
/// every frontend type. Feed it [`Drag::press`], [`Drag::motion`] and
/// [`Drag::release`]; apply the [`Effect`]s it returns.
///
/// ```
/// use norte_frontend::mouse::{Drag, Effect, Mods, Press, Spot};
///
/// let mut drag = Drag::default();
/// // Press on an unmarked row: focus + cursor, arm the sweep, mark nothing.
/// let fx = drag.press(Press { at: Spot::new(0, 2), marked: false, cursor: 0, mods: Mods::NONE });
/// assert_eq!(fx, vec![
///     Effect::MoveCursor { pane: 0, index: 2 },
///     Effect::BeginSweep { pane: 0 },
/// ]);
/// // Sweeping down marks what it covers.
/// let fx = drag.motion(Spot::new(0, 4));
/// assert!(fx.contains(&Effect::SweepRange { pane: 0, from: 2, to: 4 }));
/// // The release re-states the final range: motions may be coalesced.
/// let fx = drag.release(Some(Spot::new(0, 4)), Mods::NONE);
/// assert!(fx.contains(&Effect::SweepRange { pane: 0, from: 2, to: 4 }));
/// ```
#[derive(Debug, Default)]
pub struct Drag {
    active: Option<Active>,
}

impl Drag {
    /// The button went down. Decides the gesture (see the module docs for
    /// the precedence) and returns its immediate effects: a ctrl toggle and
    /// a shift range act at once, while a plain press only moves the cursor
    /// — marking on a press would turn every click into a selection change.
    #[must_use]
    pub fn press(&mut self, p: Press) -> Vec<Effect> {
        let Press {
            at,
            marked,
            cursor,
            mods,
        } = p;
        // A new press replaces any armed gesture: if the frontend lost a
        // release (an unfocused window, a terminal that does not report the
        // button), the old gesture dies here instead of sweeping with a
        // stale anchor.
        self.active = None;
        if mods.ctrl {
            // A discrete toggle with no drag. Wins over EVERYTHING else
            // (including `marked`): otherwise a marked row would have no
            // way to be unmarked with the mouse, because any other reading
            // of a marked row arms a transfer.
            return vec![
                Effect::MoveCursor {
                    pane: at.pane,
                    index: at.index,
                },
                Effect::SetMark {
                    pane: at.pane,
                    index: at.index,
                    marked: !marked,
                },
            ];
        }
        if mods.shift {
            // Range from the CURSOR, which stays where it is: the anchor
            // stays visible and a second shift+click extends the SAME
            // range instead of collapsing it onto the previous row. The
            // `MoveCursor` onto the cursor itself moves nothing: it says
            // "focus this pane".
            //
            // Wins over `marked` on purpose: extending a range whose end
            // lands on an ALREADY marked row is the everyday gesture
            // (ctrl+click a few, shift+click past them), and reading it as
            // a transfer would not only drop the extension — with shift
            // still held at release over the other pane it would MOVE the
            // whole selection.
            let anchor = Spot::new(at.pane, cursor);
            self.active = Some(Active {
                kind: DragKind::MarkSweep,
                anchor,
                origin: at,
                last_in_pane: at,
                last: at,
                moved: false,
                // NOT promotable: shift means "extend the range", and the
                // end routinely lands on the other side of the pane
                // boundary with the modifier still held — reading that as
                // a drop would turn a marking gesture into a MOVE of the
                // whole selection.
                promotable: false,
                outside: false,
            });
            return vec![
                Effect::MoveCursor {
                    pane: at.pane,
                    index: cursor,
                },
                // Additive (not `SweepRange`): a shift+click extends what
                // was already marked by hand and must not give anything
                // back. The rubber-band only starts if the gesture becomes
                // a drag, and its baseline will already include this
                // range.
                Effect::MarkRange {
                    pane: at.pane,
                    from: cursor,
                    to: at.index,
                },
                Effect::BeginSweep { pane: at.pane },
            ];
        }
        if marked {
            // ALREADY marked row and no modifiers = transfer of the marks.
            // The copy/move modifier is read AT RELEASE.
            self.active = Some(Active {
                kind: DragKind::Transfer,
                anchor: at,
                origin: at,
                last_in_pane: at,
                last: at,
                moved: false,
                // Already a transfer: nothing to promote.
                promotable: false,
                outside: false,
            });
            return vec![Effect::MoveCursor {
                pane: at.pane,
                index: at.index,
            }];
        }
        // Unmarked row and no modifiers: arms the sweep but does NOT mark
        // yet. A plain click must keep being what it always was (focus +
        // cursor, marks untouched); only motion turns it into a selection.
        self.active = Some(Active {
            kind: DragKind::MarkSweep,
            anchor: at,
            origin: at,
            last_in_pane: at,
            last: at,
            moved: false,
            // The ONLY promotable gesture: crossing into the other pane
            // turns it into a transfer of this row (see the module docs).
            promotable: true,
            outside: false,
        });
        vec![
            Effect::MoveCursor {
                pane: at.pane,
                index: at.index,
            },
            Effect::BeginSweep { pane: at.pane },
        ]
    }

    /// The pointer moved onto `at` with the button still down. A motion
    /// outside every row is simply not reported (the frontend passes only
    /// what its hit test resolved) — passing over a header must not cancel
    /// a live gesture.
    ///
    /// A sweep re-states its whole range on every motion
    /// ([`Effect::SweepRange`]), so the result does not depend on how many
    /// motions the frontend delivers, and RETREATING gives back the rows
    /// the pointer pulled off: the pane rubber-bands against the baseline
    /// [`Effect::BeginSweep`] armed, so marks made before the gesture
    /// survive while the surplus of an overshoot does not.
    ///
    /// A motion that does not change the row returns nothing — at GPUI's
    /// per-pixel event rate, re-stating a range over a large listing is not
    /// free.
    #[must_use]
    pub fn motion(&mut self, at: Spot) -> Vec<Effect> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        if at == active.last {
            // Same row as the last event: nothing to re-emit. Safe
            // precisely because every sweep effect re-states the whole
            // range — dropping a repeat cannot lose anything.
            return Vec::new();
        }
        active.last = at;
        if at != active.origin {
            active.moved = true;
        }
        let at_home = at.pane == active.anchor.pane;
        if at_home {
            active.last_in_pane = at;
        }
        // First event outside the anchor's pane: the crossing (later ones,
        // already outside, are not crossings again).
        let crossing = !at_home && !active.outside;
        active.outside = !at_home;
        let active = *active;
        match active.kind {
            // A transfer produces no effect while passing over: the
            // destination pane's highlight is painted by the frontend with
            // its own event; there is no decision to make here until it is
            // released.
            DragKind::Transfer => Vec::new(),
            DragKind::MarkSweep => {
                // Outside the anchor's pane the sweep does not mark, and
                // the gesture is not cancelled: the pointer can come back
                // (and coming back, `SweepRange` re-states the whole range
                // against the same baseline, so nothing is lost).
                //
                // If the sweep is PROMOTABLE, the crossing turns it into a
                // transfer of its origin row — and it gives back whatever
                // it had marked, because a promotion changes what the
                // gesture DOES, not what is selected. What a release would
                // do from here on is what `pending` says.
                //
                // The jitter inside the press's own row (which would mark
                // that row on a plain click) needs no guard of its own:
                // `last` is seeded at `origin`, so the early return above
                // already discarded it. A `!moved` here would be a
                // condition no test could kill.
                if !at_home {
                    return if crossing && active.promotable {
                        vec![Effect::RevertSweep {
                            pane: active.anchor.pane,
                        }]
                    } else {
                        Vec::new()
                    };
                }
                vec![
                    Effect::MoveCursor {
                        pane: at.pane,
                        index: at.index,
                    },
                    Effect::SweepRange {
                        pane: at.pane,
                        from: active.anchor.index,
                        to: at.index,
                    },
                ]
            }
        }
    }

    /// The button came up. `at` is `None` when the release landed outside
    /// every row (a header, the status bar, another window): the gesture is
    /// then CANCELLED, never guessed — a transfer whose destination has to
    /// be inferred is a file operation nobody asked for.
    ///
    /// `mods` is read HERE for a transfer's copy/move flag, so the user can
    /// change their mind mid-drag.
    ///
    /// A sweep re-states its range one final time, from the last row seen
    /// inside the anchor's pane when the release landed elsewhere. It does
    /// NOT assume any motion was delivered: a frontend that coalesces a
    /// whole gesture into one event must end up with the same marks as one
    /// that reports every row. For the same reason a PROMOTED sweep (see
    /// the module docs) decides here, from where the release landed, and
    /// not from whether a crossing was ever reported.
    #[must_use]
    pub fn release(&mut self, at: Option<Spot>, mods: Mods) -> Vec<Effect> {
        let Some(active) = self.active.take() else {
            return Vec::new();
        };
        let Some(at) = at else {
            return Vec::new();
        };
        // A gesture that never left its row is a click: the press already
        // emitted its own effects (focus + cursor, or the modifiers'
        // toggle/range) and there is nothing left to do here.
        if !active.moved && at == active.origin {
            return Vec::new();
        }
        match active.kind {
            DragKind::MarkSweep if active.promotable && at.pane != active.anchor.pane => {
                // PROMOTED sweep: releasing over the other pane transfers
                // the row the button went down on, not the marks (which
                // can be others) and not the swept range (which the
                // pointer only passed over on the way). `RevertSweep` is
                // re-emitted here because the frontend may have coalesced
                // the whole gesture and never reported the crossing;
                // without an armed extent it does nothing.
                vec![
                    Effect::RevertSweep {
                        pane: active.anchor.pane,
                    },
                    Effect::Transfer {
                        from_pane: active.origin.pane,
                        to_pane: at.pane,
                        move_files: mods.shift,
                        promoted: Some(active.origin.index),
                    },
                ]
            }
            DragKind::MarkSweep => {
                // Releasing outside the anchor's pane: the sweep closes on
                // the LAST row seen inside that pane. Trusting that the
                // motions already marked left the gesture as NOTHING
                // whenever the frontend coalesced them — the same drag gave
                // different marks in the TUI and the GUI.
                let end = if at.pane == active.anchor.pane {
                    at
                } else {
                    active.last_in_pane
                };
                vec![
                    Effect::MoveCursor {
                        pane: end.pane,
                        index: end.index,
                    },
                    Effect::SweepRange {
                        pane: active.anchor.pane,
                        from: active.anchor.index,
                        to: end.index,
                    },
                ]
            }
            DragKind::Transfer => {
                if at.pane == active.origin.pane {
                    // Releasing over the source pane is an explicit no-op
                    // (not a copy of a dir onto itself).
                    return Vec::new();
                }
                vec![Effect::Transfer {
                    from_pane: active.origin.pane,
                    to_pane: at.pane,
                    move_files: mods.shift,
                    // The source pane's MARKS: this gesture was born over a
                    // marked row.
                    promoted: None,
                }]
            }
        }
    }

    /// Drops any armed gesture without effects — for Esc, a lost focus or a
    /// listing that changed under the pointer. The marks a sweep already
    /// applied STAY: cancelling the gesture is not an undo. The frontend
    /// may call [`crate::PaneState::end_sweep`] alongside to release the
    /// baseline, though the next [`Effect::BeginSweep`] re-arms it anyway.
    pub fn cancel(&mut self) {
        self.active = None;
    }

    /// What the armed gesture is doing, for feedback (highlighting the
    /// source rows, the drop target, a copy/move label). `None` = nothing
    /// armed.
    #[must_use]
    pub fn kind(&self) -> Option<DragKind> {
        self.active.map(|a| a.kind)
    }

    /// Where the armed gesture started, for feedback. `None` = nothing
    /// armed.
    #[must_use]
    pub fn origin(&self) -> Option<Spot> {
        self.active.map(|a| a.origin)
    }

    /// What a release RIGHT NOW would do, with `mods` held right now:
    /// [`Pending`]. `None` = nothing armed.
    ///
    /// This is the feedback contract. A drag means one thing over its own
    /// pane and another over the far one (see the module docs on
    /// promotion), and the copy/move flag is read at RELEASE — so the user
    /// cannot know what the drop will do unless the frontend renders this,
    /// and re-renders it when shift goes down or up mid-drag.
    ///
    /// It reads the last row the frontend REPORTED, so it is exactly as
    /// current as the motions delivered; and it answers with the same rules
    /// [`Drag::release`] applies, so the label and the drop cannot disagree.
    ///
    /// ```
    /// use norte_frontend::mouse::{Drag, Mods, Pending, Press, Spot};
    ///
    /// let mut drag = Drag::default();
    /// // Press on an UNMARKED row: still just marking.
    /// let _ = drag.press(Press { at: Spot::new(0, 2), marked: false, cursor: 2, mods: Mods::NONE });
    /// assert_eq!(drag.pending(Mods::NONE), Some(Pending::Marking { pane: 0 }));
    /// // Cross into the other pane: promoted to a transfer of that ONE row…
    /// let _ = drag.motion(Spot::new(1, 4));
    /// assert_eq!(
    ///     drag.pending(Mods::NONE),
    ///     Some(Pending::Drop { from_pane: 0, to_pane: 1, move_files: false, promoted: Some(2) }),
    /// );
    /// // …and shift, read live, turns the copy into a move.
    /// assert_eq!(
    ///     drag.pending(Mods::SHIFT),
    ///     Some(Pending::Drop { from_pane: 0, to_pane: 1, move_files: true, promoted: Some(2) }),
    /// );
    /// ```
    #[must_use]
    pub fn pending(&self, mods: Mods) -> Option<Pending> {
        let a = self.active?;
        let promoted = match a.kind {
            DragKind::Transfer => None,
            DragKind::MarkSweep => {
                if !(a.promotable && a.last.pane != a.anchor.pane) {
                    // Still a sweep: at home, or not promotable.
                    return Some(Pending::Marking {
                        pane: a.anchor.pane,
                    });
                }
                Some(a.origin.index)
            }
        };
        if a.last.pane == a.origin.pane {
            // Releasing at home is an explicit no-op, so it is not
            // announced as a drop: there is no destination to promise
            // anything about.
            return Some(Pending::Carrying {
                from_pane: a.origin.pane,
            });
        }
        Some(Pending::Drop {
            from_pane: a.origin.pane,
            to_pane: a.last.pane,
            move_files: mods.shift,
            promoted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dragging from an UNMARKED row sweeps, marking what it covers.
    #[test]
    fn dragging_from_an_unmarked_row_sweeps_and_marks() {
        let mut d = Drag::default();
        let fx = d.press(Press {
            at: Spot::new(0, 1),
            marked: false,
            cursor: 0,
            mods: Mods::NONE,
        });
        assert_eq!(
            fx,
            vec![
                Effect::MoveCursor { pane: 0, index: 1 },
                Effect::BeginSweep { pane: 0 },
            ],
            "the press arms the sweep but does not mark: a plain click does \
             not change the selection"
        );
        assert_eq!(d.kind(), Some(DragKind::MarkSweep));
        assert_eq!(
            d.motion(Spot::new(0, 3)),
            vec![
                Effect::MoveCursor { pane: 0, index: 3 },
                Effect::SweepRange {
                    pane: 0,
                    from: 1,
                    to: 3
                },
            ]
        );
        assert_eq!(
            d.release(Some(Spot::new(0, 3)), Mods::NONE),
            vec![
                Effect::MoveCursor { pane: 0, index: 3 },
                Effect::SweepRange {
                    pane: 0,
                    from: 1,
                    to: 3
                },
            ],
            "the release re-states the final range: the frontend may have \
             coalesced or lost motions"
        );
        assert_eq!(d.kind(), None, "the gesture dies on release");
    }

    /// Dragging from an ALREADY marked row is a transfer of the marks, not
    /// a sweep. It is the rule that lets ONE gesture do both jobs; if it
    /// were reversed, dragging a hand-made selection would extend it
    /// instead of moving it, and the user would copy files they never
    /// chose.
    #[test]
    fn dragging_from_a_marked_row_is_a_transfer() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        assert_eq!(d.kind(), Some(DragKind::Transfer));
        assert!(
            d.motion(Spot::new(1, 0)).is_empty(),
            "a transfer marks nothing along the way"
        );
        assert_eq!(
            d.release(Some(Spot::new(1, 0)), Mods::NONE),
            vec![Effect::Transfer {
                from_pane: 0,
                to_pane: 1,
                move_files: false,
                promoted: None,
            }]
        );
    }

    /// The copy/move flag is read AT RELEASE, not at press: it is the only
    /// way for someone who starts dragging and changes their mind halfway
    /// not to end up MOVING (a destructive mutation at the source) what
    /// they thought they were copying. Same press in both cases, different
    /// release.
    #[test]
    fn the_copy_move_flag_is_read_at_release_not_at_press() {
        let press = Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        };
        let mut d = Drag::default();
        let _ = d.press(press);
        assert_eq!(
            d.release(Some(Spot::new(1, 0)), Mods::NONE),
            vec![Effect::Transfer {
                from_pane: 0,
                to_pane: 1,
                move_files: false,
                promoted: None,
            }],
            "released without shift: copy"
        );

        let mut d = Drag::default();
        let _ = d.press(press);
        assert_eq!(
            d.release(Some(Spot::new(1, 0)), Mods::SHIFT),
            vec![Effect::Transfer {
                from_pane: 0,
                to_pane: 1,
                move_files: true,
                promoted: None,
            }],
            "SAME press, shift held only at release: it moves"
        );
    }

    /// shift WINS over the row's mark state. Marking a few with ctrl+click
    /// and shift+clicking past them leaves the range's end on an ALREADY
    /// marked row: reading that as a transfer would SILENTLY drop the
    /// extension and, since the gesture is done with shift held, a brush
    /// past into the other pane would release it as `move_files: true` — a
    /// MOVE of the whole selection born out of a marking gesture.
    #[test]
    fn shift_wins_over_an_already_marked_row() {
        let mut d = Drag::default();
        let fx = d.press(Press {
            at: Spot::new(0, 9),
            marked: true, // the range's end was already marked
            cursor: 2,
            mods: Mods::SHIFT,
        });
        assert_eq!(
            fx,
            vec![
                Effect::MoveCursor { pane: 0, index: 2 },
                Effect::MarkRange {
                    pane: 0,
                    from: 2,
                    to: 9
                },
                Effect::BeginSweep { pane: 0 },
            ],
            "extends the range; does NOT read it as a transfer"
        );
        assert_eq!(d.kind(), Some(DragKind::MarkSweep));
        assert!(
            !d.release(Some(Spot::new(1, 4)), Mods::SHIFT)
                .iter()
                .any(|e| matches!(e, Effect::Transfer { .. })),
            "and it never degenerates into moving files"
        );
    }

    /// Releasing outside every row CANCELS instead of guessing. Guessing a
    /// destination (the nearest pane, the last row) would turn an aborted
    /// gesture — releasing over the header, over the status bar, outside
    /// the window — into a copy or move of files nobody asked for.
    #[test]
    fn releasing_outside_every_row_cancels() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        assert!(d.release(None, Mods::SHIFT).is_empty());
        assert_eq!(d.kind(), None, "the gesture ends up disarmed, not hanging");

        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.motion(Spot::new(0, 5));
        assert!(
            d.release(None, Mods::NONE).is_empty(),
            "a sweep released into the void does not close the range either"
        );
    }

    /// A drag that never leaves its row degrades to a click: focus +
    /// cursor and nothing else. Otherwise every jitter of the mouse over a
    /// marked row would fire a transfer between panes.
    #[test]
    fn a_drag_that_never_leaves_its_row_degrades_to_a_click() {
        let mut d = Drag::default();
        let fx = d.press(Press {
            at: Spot::new(0, 4),
            marked: true,
            cursor: 4,
            mods: Mods::NONE,
        });
        assert_eq!(fx, vec![Effect::MoveCursor { pane: 0, index: 4 }]);
        assert!(
            d.motion(Spot::new(0, 4)).is_empty(),
            "moving within the same row is not motion"
        );
        assert!(d.release(Some(Spot::new(0, 4)), Mods::SHIFT).is_empty());

        // Same with a sweep: press + motion on the same row + release
        // marks NOTHING. Without the `moved` guard, the jitter would emit a
        // `SweepRange{origin, origin}` and a plain click would mark its row.
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(1, 0),
            marked: false,
            cursor: 0,
            mods: Mods::NONE,
        });
        assert!(
            d.motion(Spot::new(1, 0)).is_empty(),
            "a jitter inside the press's row does not mark"
        );
        assert!(d.release(Some(Spot::new(1, 0)), Mods::NONE).is_empty());
    }

    /// ctrl+click toggles A SINGLE entry (the `set_mark` primitive), both
    /// ways, and does not arm a drag. Wins over the transfer fork: without
    /// that a marked row could not be unmarked with the mouse.
    #[test]
    fn ctrl_click_toggles_a_single_entry() {
        let mut d = Drag::default();
        assert_eq!(
            d.press(Press {
                at: Spot::new(0, 3),
                marked: false,
                cursor: 0,
                mods: Mods::CTRL,
            }),
            vec![
                Effect::MoveCursor { pane: 0, index: 3 },
                Effect::SetMark {
                    pane: 0,
                    index: 3,
                    marked: true
                },
            ]
        );
        assert_eq!(d.kind(), None, "discrete gesture: does not arm a drag");
        assert_eq!(
            d.press(Press {
                at: Spot::new(0, 3),
                marked: true,
                cursor: 0,
                mods: Mods::CTRL,
            }),
            vec![
                Effect::MoveCursor { pane: 0, index: 3 },
                Effect::SetMark {
                    pane: 0,
                    index: 3,
                    marked: false
                },
            ],
            "over a marked row it UNMARKS instead of starting a transfer"
        );
    }

    /// shift+click marks the range from the CURSOR and leaves the cursor
    /// where it was: the anchor stays visible and a second shift+click
    /// extends the same range instead of collapsing it onto the previous
    /// row. The press's range is ADDITIVE (`MarkRange`): it extends what
    /// was marked by hand and gives nothing back.
    #[test]
    fn shift_click_marks_the_range_from_the_cursor() {
        let mut d = Drag::default();
        assert_eq!(
            d.press(Press {
                at: Spot::new(1, 6),
                marked: false,
                cursor: 2,
                mods: Mods::SHIFT,
            }),
            vec![
                Effect::MoveCursor { pane: 1, index: 2 },
                Effect::MarkRange {
                    pane: 1,
                    from: 2,
                    to: 6
                },
                Effect::BeginSweep { pane: 1 },
            ]
        );
        // And shift+drag keeps extending from the SAME anchor.
        assert_eq!(
            d.motion(Spot::new(1, 8)),
            vec![
                Effect::MoveCursor { pane: 1, index: 8 },
                Effect::SweepRange {
                    pane: 1,
                    from: 2,
                    to: 8
                },
            ]
        );
    }

    /// `Press.cursor` is the cursor of the pane in `at`, NOT the focused
    /// pane's — they differ exactly when shift+clicking on the pane that
    /// does not have focus, which is when it matters. Everything that comes
    /// out points at the pressed pane.
    #[test]
    fn the_press_cursor_is_the_pressed_panes_cursor() {
        let mut d = Drag::default();
        // Focus on pane 0; the user shift+clicks on pane 1, whose cursor
        // is at 7.
        let fx = d.press(Press {
            at: Spot::new(1, 3),
            marked: false,
            cursor: 7,
            mods: Mods::SHIFT,
        });
        assert_eq!(
            fx,
            vec![
                Effect::MoveCursor { pane: 1, index: 7 },
                Effect::MarkRange {
                    pane: 1,
                    from: 7,
                    to: 3
                },
                Effect::BeginSweep { pane: 1 },
            ],
            "anchor = the PRESSED PANE's cursor, and the range goes upward"
        );
    }

    /// Releasing a transfer over the SOURCE pane is a no-op: copying a
    /// directory onto itself is not what anyone who dragged and changed
    /// their mind halfway asked for.
    #[test]
    fn releasing_on_the_source_pane_does_not_transfer() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.motion(Spot::new(1, 1)); // wanders over the other pane…
        assert!(
            d.release(Some(Spot::new(0, 7)), Mods::NONE).is_empty(),
            "…but comes back and releases at home"
        );
    }

    /// The sweep RETREATS: coming back over its own steps re-states a
    /// shorter range (`SweepRange`, which the pane applies against its
    /// baseline), instead of leaving marked everything the pointer ever
    /// touched. An additive `MarkRange` here would leave the excess rows
    /// marked — and since the excess happens at the viewport's edge under
    /// autoscroll, they are exactly the ones that just scrolled off screen.
    #[test]
    fn the_sweep_retreats_instead_of_accumulating() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.motion(Spot::new(0, 20));
        assert_eq!(
            d.motion(Spot::new(0, 4)),
            vec![
                Effect::MoveCursor { pane: 0, index: 4 },
                Effect::SweepRange {
                    pane: 0,
                    from: 2,
                    to: 4
                },
            ],
            "a shorter AND non-additive range: rows 5..20 are released"
        );
        assert!(
            !d.release(Some(Spot::new(0, 4)), Mods::NONE)
                .iter()
                .any(|e| matches!(e, Effect::MarkRange { .. })),
            "a sweep NEVER emits the additive marker"
        );
    }

    /// Delivery does not change the outcome: the same gesture reported row
    /// by row and reported all at once leaves EXACTLY the same marks.
    /// crossterm reports per cell and GPUI per pixel, and both coalesce
    /// under load — without this, the same drag would mark different
    /// things in each frontend, which is exactly the drift this module
    /// exists to prevent.
    #[test]
    fn delivery_granularity_does_not_change_the_marks() {
        let ranges = |fx: &[Effect]| -> Vec<(usize, usize)> {
            fx.iter()
                .filter_map(|e| match e {
                    Effect::SweepRange { from, to, .. } => Some((*from, *to)),
                    _ => None,
                })
                .collect()
        };
        let press = Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        };

        // Granular delivery: every row the pointer passes over.
        let mut granular = Drag::default();
        let _ = granular.press(press);
        for i in [3, 4, 5] {
            let _ = granular.motion(Spot::new(0, i));
        }
        let granular_end = ranges(&granular.release(Some(Spot::new(0, 5)), Mods::NONE));

        // Coalesced delivery: a single motion at the end of the run.
        let mut coalesced = Drag::default();
        let _ = coalesced.press(press);
        let _ = coalesced.motion(Spot::new(0, 5));
        let coalesced_end = ranges(&coalesced.release(Some(Spot::new(0, 5)), Mods::NONE));

        assert_eq!(granular_end, vec![(2, 5)]);
        assert_eq!(
            granular_end, coalesced_end,
            "the final range is the same with and without intermediate motions"
        );
    }

    /// Releasing the sweep OUTSIDE the anchor's pane closes it on the last
    /// row seen inside that pane, instead of emitting nothing on the trust
    /// that the motions already marked: with coalesced delivery there were
    /// no motions inside the pane and the whole gesture was lost.
    ///
    /// This test's sweep is the shift one (NOT promotable): a plain press
    /// on an unmarked row DOES get promoted on crossing, and that is pinned
    /// by `a_sweep_that_crosses_panes_is_promoted_to_a_transfer`.
    #[test]
    fn releasing_the_sweep_outside_the_pane_closes_it_on_the_last_row_seen() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::SHIFT,
        });
        let _ = d.motion(Spot::new(0, 5));
        let _ = d.motion(Spot::new(1, 5)); // steps out to the other pane
        assert_eq!(
            d.release(Some(Spot::new(1, 5)), Mods::NONE),
            vec![
                Effect::MoveCursor { pane: 0, index: 5 },
                Effect::SweepRange {
                    pane: 0,
                    from: 2,
                    to: 5
                },
            ],
            "the range closes on row 5 of pane 0, not on pane 1"
        );
    }

    /// A sweep that leaves to the other pane does not cancel: the pointer
    /// can come back, and coming back it keeps sweeping from the SAME
    /// anchor — `SweepRange` re-states the whole range against the same
    /// baseline, so the walk through the other pane costs not a single
    /// mark.
    #[test]
    fn a_sweep_that_leaves_to_the_other_pane_sweeps_again_on_return() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 1),
            marked: false,
            cursor: 1,
            mods: Mods::NONE,
        });
        assert_eq!(
            d.motion(Spot::new(1, 4)),
            vec![Effect::RevertSweep { pane: 0 }],
            "the crossing promotes: gives back what was swept and marks nothing at the destination"
        );
        assert!(
            d.motion(Spot::new(1, 6)).is_empty(),
            "once outside, walking through the other pane re-emits nothing"
        );
        assert_eq!(d.kind(), Some(DragKind::MarkSweep), "still armed");
        assert_eq!(
            d.motion(Spot::new(0, 3)),
            vec![
                Effect::MoveCursor { pane: 0, index: 3 },
                Effect::SweepRange {
                    pane: 0,
                    from: 1,
                    to: 3
                },
            ],
            "back home it goes back to being a sweep"
        );
        assert_eq!(
            d.pending(Mods::NONE),
            Some(Pending::Marking { pane: 0 }),
            "and it says so: releasing here touches not a single file"
        );
    }

    /// A sweep born on an UNMARKED row that crosses to the other pane gets
    /// PROMOTED to a transfer of THAT row. It is the most common drag in
    /// any desktop file manager (grab a file and drop it on the other
    /// panel) and without this it would transfer nothing: it would sweep a
    /// row and mark it.
    ///
    /// It carries the PRESS's row, not the pane's marks (there can be
    /// eleven others) nor the swept range (which the pointer only passed
    /// over on the way), and it gives back whatever it had marked: a
    /// promotion changes what the gesture DOES, not what is selected.
    #[test]
    fn a_sweep_that_crosses_panes_is_promoted_to_a_transfer() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.motion(Spot::new(0, 5)); // sweeps 2..=5 along the way
        assert_eq!(
            d.motion(Spot::new(1, 3)),
            vec![Effect::RevertSweep { pane: 0 }],
            "crossing gives back the rows it swept along the way"
        );
        assert_eq!(
            d.release(Some(Spot::new(1, 3)), Mods::NONE),
            vec![
                Effect::RevertSweep { pane: 0 },
                Effect::Transfer {
                    from_pane: 0,
                    to_pane: 1,
                    move_files: false,
                    promoted: Some(2),
                },
            ],
            "transfers the press's row, not the swept range"
        );
    }

    /// Promotion survives coalesced delivery: a frontend that reports NO
    /// motion at all (the whole gesture in press + release) still
    /// transfers. And the copy/move flag is still read at release.
    #[test]
    fn promotion_does_not_depend_on_motions_arriving() {
        let press = Press {
            at: Spot::new(1, 7),
            marked: false,
            cursor: 7,
            mods: Mods::NONE,
        };
        let mut d = Drag::default();
        let _ = d.press(press);
        assert_eq!(
            d.release(Some(Spot::new(0, 0)), Mods::SHIFT),
            vec![
                Effect::RevertSweep { pane: 1 },
                Effect::Transfer {
                    from_pane: 1,
                    to_pane: 0,
                    move_files: true,
                    promoted: Some(7),
                },
            ],
            "with no motions: it is still a transfer, and shift MOVES"
        );
    }

    /// A CANCELLED promotion leaves nothing: no transfer (releasing
    /// outside every row does not guess a destination) and no new marks.
    /// It is half the contract that makes promotion acceptable — the
    /// gesture means two things depending on where it ends, so aborting it
    /// has to restore the exact prior state.
    #[test]
    fn a_cancelled_promotion_leaves_no_transfer_or_marks() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.motion(Spot::new(0, 6));
        let _ = d.motion(Spot::new(1, 1));
        assert!(
            d.release(None, Mods::NONE).is_empty(),
            "releasing over chrome neither transfers nor closes the range"
        );
        assert_eq!(d.kind(), None);
    }

    /// `pending` and `release` cannot disagree: it is the promise painted
    /// to the user BEFORE releasing (which rows, where to, copy or move)
    /// against what the drop really does. The three states of the same
    /// promoted gesture are checked, with and without shift.
    #[test]
    fn what_pending_promises_is_what_release_does() {
        for mods in [Mods::NONE, Mods::SHIFT] {
            let mut d = Drag::default();
            let _ = d.press(Press {
                at: Spot::new(0, 4),
                marked: false,
                cursor: 4,
                mods: Mods::NONE,
            });
            assert_eq!(
                d.pending(mods),
                Some(Pending::Marking { pane: 0 }),
                "at home: marking"
            );
            let _ = d.motion(Spot::new(1, 2));
            let promised = d.pending(mods);
            assert_eq!(
                promised,
                Some(Pending::Drop {
                    from_pane: 0,
                    to_pane: 1,
                    move_files: mods.shift,
                    promoted: Some(4),
                })
            );
            let done = d.release(Some(Spot::new(1, 2)), mods);
            let Some(Pending::Drop {
                from_pane,
                to_pane,
                move_files,
                promoted,
            }) = promised
            else {
                panic!("promised a drop");
            };
            assert!(
                done.contains(&Effect::Transfer {
                    from_pane,
                    to_pane,
                    move_files,
                    promoted,
                }),
                "the drop does EXACTLY what was promised"
            );
        }
    }

    /// A transfer gesture that has not yet left its pane is not announced
    /// as a drop: releasing there is an explicit no-op, and promising a
    /// copy that is not going to happen is worse than promising nothing.
    #[test]
    fn a_transfer_at_home_is_announced_as_such_not_as_a_drop() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(1, 3),
            marked: true,
            cursor: 3,
            mods: Mods::NONE,
        });
        assert_eq!(
            d.pending(Mods::SHIFT),
            Some(Pending::Carrying { from_pane: 1 })
        );
        let _ = d.motion(Spot::new(0, 9));
        assert_eq!(
            d.pending(Mods::NONE),
            Some(Pending::Drop {
                from_pane: 1,
                to_pane: 0,
                move_files: false,
                promoted: None,
            }),
            "over the other pane it is: and it carries the MARKS, not a row"
        );
    }

    /// With no armed gesture there is nothing to announce.
    #[test]
    fn with_no_armed_gesture_there_is_nothing_pending() {
        let mut d = Drag::default();
        assert_eq!(d.pending(Mods::NONE), None);
        let _ = d.press(Press {
            at: Spot::new(0, 0),
            marked: false,
            cursor: 0,
            mods: Mods::CTRL,
        });
        assert_eq!(
            d.pending(Mods::NONE),
            None,
            "ctrl+click is discrete: it does not arm a drag"
        );
    }

    /// A motion that does not change row emits nothing: at GPUI's
    /// per-pixel event rate, re-stating the range over a large listing
    /// costs milliseconds per event. Safe because every sweep effect
    /// re-states the whole range.
    #[test]
    fn a_repeated_motion_emits_nothing() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        assert!(!d.motion(Spot::new(0, 6)).is_empty(), "first time: emits");
        assert!(
            d.motion(Spot::new(0, 6)).is_empty(),
            "same row again: nothing"
        );
        assert!(
            !d.motion(Spot::new(0, 7)).is_empty(),
            "new row: emits again"
        );
    }

    /// Motions and releases with no prior press do nothing: a frontend that
    /// reports mouse movement continuously (or that lost the press) must
    /// not mark on its own.
    #[test]
    fn events_with_no_press_do_nothing() {
        let mut d = Drag::default();
        assert!(d.motion(Spot::new(0, 3)).is_empty());
        assert!(d.release(Some(Spot::new(1, 3)), Mods::SHIFT).is_empty());
    }

    /// A new press replaces the armed gesture: if the frontend lost a
    /// release (an unfocused window, a terminal that does not report the
    /// button), the stale anchor must not survive the next click.
    #[test]
    fn a_new_press_disarms_the_previous_gesture() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.press(Press {
            at: Spot::new(0, 9),
            marked: false,
            cursor: 9,
            mods: Mods::NONE,
        });
        assert_eq!(d.kind(), Some(DragKind::MarkSweep));
        assert_eq!(d.origin(), Some(Spot::new(0, 9)));
    }
}
