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
        // Una pulsación nueva sustituye a cualquier gesto armado: si el
        // frontend perdió un release (ventana desenfocada, terminal que no
        // reporta el botón), el gesto viejo muere aquí en vez de barrer con
        // un ancla rancia.
        self.active = None;
        if mods.ctrl {
            // Toggle discreto y sin arrastre. Gana sobre TODO lo demás
            // (incluido `marked`): si no, una fila marcada no tendría forma
            // de desmarcarse con el ratón, porque cualquier otra lectura de
            // una fila marcada arma una transferencia.
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
            // Rango desde el CURSOR, que se queda donde está: el ancla
            // sigue visible y un segundo shift+click extiende el MISMO
            // rango en vez de colapsarlo sobre la fila anterior. El
            // `MoveCursor` sobre el propio cursor no mueve nada: dice
            // «enfoca este pane».
            //
            // Gana sobre `marked` a propósito: extender un rango cuyo
            // extremo cae sobre una fila YA marcada es el gesto de todos
            // los días (ctrl+click a unos cuantos, shift+click más allá),
            // y leerlo como transferencia no solo perdería la extensión —
            // con shift todavía pulsado al soltar sobre el otro pane
            // MOVERÍA la selección entera.
            let anchor = Spot::new(at.pane, cursor);
            self.active = Some(Active {
                kind: DragKind::MarkSweep,
                anchor,
                origin: at,
                last_in_pane: at,
                last: at,
                moved: false,
                // NO promovible: shift dice «extiende el rango», y el
                // extremo cae rutinariamente al otro lado del borde entre
                // panes con el modificador todavía pulsado — leer eso como
                // un drop convertiría un gesto de marcar en un MOVIMIENTO
                // de toda la selección.
                promotable: false,
                outside: false,
            });
            return vec![
                Effect::MoveCursor {
                    pane: at.pane,
                    index: cursor,
                },
                // Aditivo (no `SweepRange`): un shift+click extiende lo que
                // ya había marcado a mano y no debe devolver nada. El
                // rubber-band solo empieza si el gesto se convierte en
                // arrastre, y su baseline incluirá ya este rango.
                Effect::MarkRange {
                    pane: at.pane,
                    from: cursor,
                    to: at.index,
                },
                Effect::BeginSweep { pane: at.pane },
            ];
        }
        if marked {
            // Fila YA marcada y sin modificadores = transferencia de las
            // marcas. El modificador de copiar/mover se lee AL SOLTAR.
            self.active = Some(Active {
                kind: DragKind::Transfer,
                anchor: at,
                origin: at,
                last_in_pane: at,
                last: at,
                moved: false,
                // Ya es una transferencia: no hay nada que promover.
                promotable: false,
                outside: false,
            });
            return vec![Effect::MoveCursor {
                pane: at.pane,
                index: at.index,
            }];
        }
        // Fila sin marcar y sin modificadores: arma el barrido pero NO
        // marca todavía. Un click suelto debe seguir siendo lo que siempre
        // fue (foco + cursor, marcas intactas); solo el movimiento lo
        // convierte en selección.
        self.active = Some(Active {
            kind: DragKind::MarkSweep,
            anchor: at,
            origin: at,
            last_in_pane: at,
            last: at,
            moved: false,
            // El ÚNICO gesto promovible: cruzar al otro pane lo convierte en
            // una transferencia de esta fila (ver la cabecera del módulo).
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
            // Misma fila que el último evento: nada que re-emitir. Seguro
            // precisamente porque cada efecto de barrido re-enuncia el
            // rango entero — tirar un repetido no puede perder nada.
            return Vec::new();
        }
        active.last = at;
        if at != active.origin {
            active.moved = true;
        }
        let en_casa = at.pane == active.anchor.pane;
        if en_casa {
            active.last_in_pane = at;
        }
        // Primer evento fuera del pane del ancla: el cruce (los siguientes,
        // ya fuera, no vuelven a serlo).
        let cruce = !en_casa && !active.outside;
        active.outside = !en_casa;
        let active = *active;
        match active.kind {
            // La transferencia no produce efecto al pasar por encima: el
            // resaltado del pane de destino lo pinta el frontend con su
            // propio evento; aquí no hay ninguna decisión que tomar hasta
            // que se suelta.
            DragKind::Transfer => Vec::new(),
            DragKind::MarkSweep => {
                // Fuera del pane del ancla el barrido no marca, y el gesto
                // no se cancela: el puntero puede volver (y al volver,
                // `SweepRange` re-enuncia el rango entero contra la misma
                // baseline, así que no se pierde nada).
                //
                // Si el barrido es PROMOVIBLE, el cruce lo convierte en una
                // transferencia de su fila de origen — y devuelve lo que
                // llevara marcado, porque una promoción cambia lo que el
                // gesto HACE, no lo que está seleccionado. Lo que un
                // release haría a partir de aquí lo dice `pending`.
                //
                // El temblor dentro de la fila del press (que marcaría esa
                // fila en un simple click) no necesita guard propio: `last`
                // nace en `origin`, así que el early-return de arriba ya lo
                // ha descartado. Un `!moved` aquí sería una condición que
                // ningún test puede tumbar.
                if !en_casa {
                    return if cruce && active.promotable {
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
        // Un gesto que nunca salió de su fila es un click: la pulsación ya
        // emitió lo suyo (foco + cursor, o el toggle/rango de los
        // modificadores) y aquí no queda nada que hacer.
        if !active.moved && at == active.origin {
            return Vec::new();
        }
        match active.kind {
            DragKind::MarkSweep if active.promotable && at.pane != active.anchor.pane => {
                // Barrido PROMOVIDO: soltar sobre el otro pane transfiere la
                // fila donde bajó el botón, no las marcas (que pueden ser
                // otras) y no el rango barrido (por el que el puntero solo
                // pasó de camino). El `RevertSweep` se re-emite aquí porque
                // el frontend pudo coalescer el gesto entero y no haber
                // reportado nunca el cruce; sin extent armado no hace nada.
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
                // Soltar fuera del pane del ancla: el barrido se cierra
                // sobre la ÚLTIMA fila vista dentro de ese pane. Confiar en
                // que las motions ya marcaron dejaba el gesto en NADA
                // cuando el frontend las coalescía — el mismo arrastre daba
                // marcas distintas en la TUI y en la GUI.
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
                    // Soltar sobre el pane de origen es un no-op explícito
                    // (no una copia de un dir sobre sí mismo).
                    return Vec::new();
                }
                vec![Effect::Transfer {
                    from_pane: active.origin.pane,
                    to_pane: at.pane,
                    move_files: mods.shift,
                    // Las MARCAS del pane de origen: este gesto nació sobre
                    // una fila marcada.
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
                    // Sigue siendo un barrido: en casa, o no promovible.
                    return Some(Pending::Marking {
                        pane: a.anchor.pane,
                    });
                }
                Some(a.origin.index)
            }
        };
        if a.last.pane == a.origin.pane {
            // Soltar en casa es un no-op explícito, así que no se anuncia
            // como drop: no hay destino sobre el que prometer nada.
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

    /// Arrastrar desde una fila SIN marcar barre marcando lo que cubre.
    #[test]
    fn arrastre_desde_fila_sin_marcar_barre_marcando() {
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
            "la pulsación arma el barrido pero no marca: un click suelto no \
             cambia la selección"
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
            "el release re-emite el rango final: el frontend puede haber \
             coalescido o perdido motions"
        );
        assert_eq!(d.kind(), None, "el gesto muere al soltar");
    }

    /// Arrastrar desde una fila YA marcada es una transferencia de las
    /// marcas, no un barrido. Es la regla que deja que UN solo gesto haga
    /// los dos trabajos; si se invirtiera, arrastrar una selección hecha a
    /// mano la ampliaría en vez de moverla y el usuario copiaría ficheros
    /// que nunca eligió.
    #[test]
    fn arrastre_desde_fila_marcada_es_transferencia() {
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
            "una transferencia no marca nada por el camino"
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

    /// El flag copiar/mover se lee AL SOLTAR, no al pulsar: es la única
    /// forma de que quien empieza a arrastrar y cambia de idea a mitad no
    /// acabe MOVIENDO (mutación destructiva en el origen) lo que creía
    /// copiar. Mismo press en los dos casos, distinto release.
    #[test]
    fn el_flag_copiar_mover_se_lee_al_soltar_no_al_pulsar() {
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
            "soltó sin shift: copia"
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
            "MISMA pulsación, shift pulsado solo al soltar: mueve"
        );
    }

    /// shift GANA sobre el estado de marca de la fila. Marcar unos cuantos
    /// con ctrl+click y shift+click más allá deja el extremo del rango
    /// sobre una fila YA marcada: leer eso como transferencia perdería la
    /// extensión EN SILENCIO y, como el gesto se hace con shift pulsado,
    /// un roce hasta el otro pane lo soltaría como `move_files: true` — un
    /// MOVIMIENTO de toda la selección salido de un gesto de marcar.
    #[test]
    fn shift_gana_sobre_una_fila_ya_marcada() {
        let mut d = Drag::default();
        let fx = d.press(Press {
            at: Spot::new(0, 9),
            marked: true, // el extremo del rango ya estaba marcado
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
            "extiende el rango; NO lo lee como transferencia"
        );
        assert_eq!(d.kind(), Some(DragKind::MarkSweep));
        assert!(
            !d.release(Some(Spot::new(1, 4)), Mods::SHIFT)
                .iter()
                .any(|e| matches!(e, Effect::Transfer { .. })),
            "y jamás degenera en un movimiento de ficheros"
        );
    }

    /// Soltar fuera de toda fila CANCELA en vez de adivinar. Adivinar un
    /// destino (el pane más cercano, la última fila) convertiría un gesto
    /// abortado — soltar sobre la cabecera, sobre la barra de estado,
    /// fuera de la ventana — en una copia o un movimiento de ficheros que
    /// nadie pidió.
    #[test]
    fn soltar_fuera_de_toda_fila_cancela() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        assert!(d.release(None, Mods::SHIFT).is_empty());
        assert_eq!(d.kind(), None, "el gesto queda desarmado, no colgado");

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
            "un barrido soltado en el vacío tampoco cierra el rango"
        );
    }

    /// Un arrastre que no sale de su fila degrada a click: foco + cursor y
    /// nada más. Si no, cada temblor del ratón sobre una fila marcada
    /// dispararía una transferencia entre panes.
    #[test]
    fn arrastre_que_no_sale_de_su_fila_degrada_a_click() {
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
            "moverse dentro de la misma fila no es movimiento"
        );
        assert!(d.release(Some(Spot::new(0, 4)), Mods::SHIFT).is_empty());

        // Igual con un barrido: press + motion en la misma fila + release
        // no marca NADA. Sin el guard de `moved`, el temblor emitiría un
        // `SweepRange{origin, origin}` y un simple click marcaría su fila.
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(1, 0),
            marked: false,
            cursor: 0,
            mods: Mods::NONE,
        });
        assert!(
            d.motion(Spot::new(1, 0)).is_empty(),
            "un temblor dentro de la fila del press no marca"
        );
        assert!(d.release(Some(Spot::new(1, 0)), Mods::NONE).is_empty());
    }

    /// ctrl+click togglea UNA sola entrada (el primitivo `set_mark`), en
    /// los dos sentidos, y no arma arrastre. Gana sobre la bifurcación de
    /// la transferencia: sin eso una fila marcada no podría desmarcarse
    /// con el ratón.
    #[test]
    fn ctrl_click_togglea_una_sola_entrada() {
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
        assert_eq!(d.kind(), None, "gesto discreto: no arma arrastre");
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
            "sobre una fila marcada DESMARCA en vez de arrancar transferencia"
        );
    }

    /// shift+click marca el rango desde el CURSOR y deja el cursor donde
    /// estaba: el ancla sigue a la vista y un segundo shift+click extiende
    /// el mismo rango en vez de colapsarlo sobre la fila anterior. El
    /// rango del press es ADITIVO (`MarkRange`): extiende lo marcado a
    /// mano y no devuelve nada.
    #[test]
    fn shift_click_marca_el_rango_desde_el_cursor() {
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
        // Y shift+arrastre sigue extendiendo desde el MISMO ancla.
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

    /// `Press.cursor` es el cursor del pane de `at`, NO el del pane con
    /// foco — difieren exactamente cuando se hace shift+click sobre el
    /// pane que no tiene el foco, que es cuando importa. Todo lo que sale
    /// apunta al pane pulsado.
    #[test]
    fn el_cursor_del_press_es_el_del_pane_pulsado() {
        let mut d = Drag::default();
        // Foco en el pane 0; el usuario hace shift+click en el pane 1,
        // cuyo cursor está en 7.
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
            "ancla = cursor DEL PANE PULSADO, y el rango va hacia arriba"
        );
    }

    /// Soltar una transferencia sobre el pane de ORIGEN es un no-op: copiar
    /// un directorio sobre sí mismo no es lo que pidió nadie que arrastró y
    /// se arrepintió a medio camino.
    #[test]
    fn soltar_en_el_pane_de_origen_no_transfiere() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.motion(Spot::new(1, 1)); // pasea por el otro pane…
        assert!(
            d.release(Some(Spot::new(0, 7)), Mods::NONE).is_empty(),
            "…pero vuelve y suelta en casa"
        );
    }

    /// El barrido RETROCEDE: al volver sobre sus pasos re-enuncia un rango
    /// más corto (`SweepRange`, que el pane aplica contra su baseline), en
    /// vez de dejar marcado todo lo que el puntero llegó a tocar. Un
    /// `MarkRange` aditivo aquí dejaría marcadas las filas del exceso — y
    /// como el exceso ocurre en el borde del viewport bajo autoscroll, son
    /// justo las que acaban de salir de la pantalla.
    #[test]
    fn el_barrido_retrocede_en_vez_de_acumular() {
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
            "rango más corto Y no-aditivo: las filas 5..20 se sueltan"
        );
        assert!(
            !d.release(Some(Spot::new(0, 4)), Mods::NONE)
                .iter()
                .any(|e| matches!(e, Effect::MarkRange { .. })),
            "un barrido no emite JAMÁS el marcador aditivo"
        );
    }

    /// La entrega no cambia el resultado: el mismo gesto reportado fila a
    /// fila y reportado de una sola vez deja EXACTAMENTE las mismas
    /// marcas. crossterm reporta por celda y GPUI por píxel, y ambos
    /// coalescen bajo carga — sin esto, el mismo arrastre marcaría cosas
    /// distintas en cada frontend, que es justo la deriva que este módulo
    /// existe para evitar.
    #[test]
    fn la_granularidad_de_la_entrega_no_cambia_las_marcas() {
        let rangos = |fx: &[Effect]| -> Vec<(usize, usize)> {
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

        // Entrega granular: cada fila por la que pasa el puntero.
        let mut granular = Drag::default();
        let _ = granular.press(press);
        for i in [3, 4, 5] {
            let _ = granular.motion(Spot::new(0, i));
        }
        let fin_granular = rangos(&granular.release(Some(Spot::new(0, 5)), Mods::NONE));

        // Entrega coalescida: un único motion al final del recorrido.
        let mut coalescida = Drag::default();
        let _ = coalescida.press(press);
        let _ = coalescida.motion(Spot::new(0, 5));
        let fin_coalescida = rangos(&coalescida.release(Some(Spot::new(0, 5)), Mods::NONE));

        assert_eq!(fin_granular, vec![(2, 5)]);
        assert_eq!(
            fin_granular, fin_coalescida,
            "el rango final es el mismo con y sin motions intermedias"
        );
    }

    /// Soltar el barrido FUERA del pane del ancla lo cierra sobre la
    /// última fila vista dentro de ese pane, en vez de no emitir nada
    /// fiándose de que las motions ya marcaron: con la entrega coalescida
    /// no hubo motions dentro del pane y el gesto entero se perdía.
    ///
    /// El barrido de este test es el de shift (NO promovible): el de una
    /// pulsación limpia sobre fila sin marcar sí se promueve al cruzar, y
    /// eso lo clava `un_barrido_que_cruza_de_pane_se_promueve_a_transferencia`.
    #[test]
    fn soltar_el_barrido_fuera_del_pane_lo_cierra_sobre_la_ultima_fila_vista() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::SHIFT,
        });
        let _ = d.motion(Spot::new(0, 5));
        let _ = d.motion(Spot::new(1, 5)); // se sale al otro pane
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
            "el rango se cierra en la fila 5 del pane 0, no en el pane 1"
        );
    }

    /// Un barrido que se sale al otro pane no cancela: el puntero puede
    /// volver, y al volver sigue barriendo desde el MISMO ancla — el
    /// `SweepRange` re-enuncia el rango entero contra la misma baseline, así
    /// que el paseo por el otro pane no cuesta ni una marca.
    #[test]
    fn el_barrido_que_sale_al_otro_pane_vuelve_a_barrer_al_regresar() {
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
            "el cruce promueve: devuelve lo barrido y no marca en el destino"
        );
        assert!(
            d.motion(Spot::new(1, 6)).is_empty(),
            "una vez fuera, pasear por el otro pane no re-emite nada"
        );
        assert_eq!(d.kind(), Some(DragKind::MarkSweep), "sigue armado");
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
            "de vuelta en casa vuelve a ser un barrido"
        );
        assert_eq!(
            d.pending(Mods::NONE),
            Some(Pending::Marking { pane: 0 }),
            "y lo dice: soltar aquí no toca ni un fichero"
        );
    }

    /// Un barrido que nace en una fila SIN marcar y cruza al otro pane se
    /// PROMUEVE a transferencia de ESA fila. Es el arrastre más común de
    /// cualquier file manager de escritorio (coger un fichero y tirarlo al
    /// otro panel) y sin esto no transferiría nada: barrería una fila y la
    /// marcaría.
    ///
    /// Lleva la fila del PRESS, no las marcas del pane (pueden ser otras
    /// once) ni el rango barrido (por el que el puntero solo pasó de
    /// camino), y devuelve lo que hubiera marcado: una promoción cambia lo
    /// que el gesto HACE, no lo que está seleccionado.
    #[test]
    fn un_barrido_que_cruza_de_pane_se_promueve_a_transferencia() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        let _ = d.motion(Spot::new(0, 5)); // barre 2..=5 de camino
        assert_eq!(
            d.motion(Spot::new(1, 3)),
            vec![Effect::RevertSweep { pane: 0 }],
            "al cruzar devuelve las filas que barrió por el camino"
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
            "transfiere la fila del press, no el rango barrido"
        );
    }

    /// La promoción sobrevive a la entrega coalescida: un frontend que no
    /// reporta NINGUNA motion (todo el gesto en press + release) transfiere
    /// igual. Y el flag copiar/mover se sigue leyendo al soltar.
    #[test]
    fn la_promocion_no_depende_de_que_lleguen_motions() {
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
            "sin motions: sigue siendo una transferencia, y con shift MUEVE"
        );
    }

    /// Una promoción CANCELADA no deja nada: ni transferencia (soltar fuera
    /// de toda fila no adivina destino) ni marcas nuevas. Es la mitad del
    /// contrato que hace la promoción aceptable — el gesto significa dos
    /// cosas según dónde acabe, así que abortarlo tiene que devolver el
    /// estado exacto de antes.
    #[test]
    fn una_promocion_cancelada_no_deja_transferencia_ni_marcas() {
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
            "soltar sobre el cromo no transfiere ni cierra rango"
        );
        assert_eq!(d.kind(), None);
    }

    /// `pending` y `release` no pueden discrepar: es la promesa que se le
    /// pinta al usuario ANTES de soltar (qué filas, a dónde, copiar o
    /// mover) frente a lo que el drop hace de verdad. Se comprueban los
    /// tres estados de un mismo gesto promovido, con y sin shift.
    #[test]
    fn lo_que_pending_promete_es_lo_que_release_hace() {
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
                "en casa: marcando"
            );
            let _ = d.motion(Spot::new(1, 2));
            let prometido = d.pending(mods);
            assert_eq!(
                prometido,
                Some(Pending::Drop {
                    from_pane: 0,
                    to_pane: 1,
                    move_files: mods.shift,
                    promoted: Some(4),
                })
            );
            let hecho = d.release(Some(Spot::new(1, 2)), mods);
            let Some(Pending::Drop {
                from_pane,
                to_pane,
                move_files,
                promoted,
            }) = prometido
            else {
                panic!("prometía un drop");
            };
            assert!(
                hecho.contains(&Effect::Transfer {
                    from_pane,
                    to_pane,
                    move_files,
                    promoted,
                }),
                "el drop hace EXACTAMENTE lo prometido"
            );
        }
    }

    /// Un gesto de transferencia que todavía no ha salido de su pane no se
    /// anuncia como drop: soltar ahí es un no-op explícito, y prometer una
    /// copia que no va a ocurrir es peor que no prometer nada.
    #[test]
    fn una_transferencia_en_casa_se_anuncia_como_tal_y_no_como_drop() {
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
            "sobre el otro pane sí: y lleva las MARCAS, no una fila"
        );
    }

    /// Sin gesto armado no hay nada que anunciar.
    #[test]
    fn sin_gesto_armado_no_hay_nada_pendiente() {
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
            "ctrl+click es discreto: no arma arrastre"
        );
    }

    /// Un motion que no cambia de fila no emite nada: a ritmo de evento
    /// por píxel (GPUI), re-enunciar el rango sobre un listado grande
    /// cuesta milisegundos por evento. Seguro porque cada efecto de
    /// barrido re-enuncia el rango entero.
    #[test]
    fn un_motion_repetido_no_emite_nada() {
        let mut d = Drag::default();
        let _ = d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        assert!(!d.motion(Spot::new(0, 6)).is_empty(), "primera vez: emite");
        assert!(
            d.motion(Spot::new(0, 6)).is_empty(),
            "misma fila otra vez: nada"
        );
        assert!(
            !d.motion(Spot::new(0, 7)).is_empty(),
            "fila nueva: vuelve a emitir"
        );
    }

    /// Motions y releases sin pulsación previa no hacen nada: un frontend
    /// que reporta el movimiento del ratón continuamente (o que perdió el
    /// press) no debe marcar por su cuenta.
    #[test]
    fn eventos_sin_pulsacion_no_hacen_nada() {
        let mut d = Drag::default();
        assert!(d.motion(Spot::new(0, 3)).is_empty());
        assert!(d.release(Some(Spot::new(1, 3)), Mods::SHIFT).is_empty());
    }

    /// Una pulsación nueva sustituye al gesto armado: si el frontend perdió
    /// un release (ventana desenfocada, terminal que no reporta el botón),
    /// el ancla rancia no debe sobrevivir al siguiente click.
    #[test]
    fn una_pulsacion_nueva_desarma_el_gesto_anterior() {
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
