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
//! The gesture is decided at PRESS, from the state of the row the pointer
//! went down on:
//!
//! - ctrl+press toggles that one entry ([`Effect::SetMark`]) and arms
//!   nothing: it is a discrete gesture, and it wins over everything else so
//!   that an already-marked row can always be unmarked.
//! - press on an UNMARKED row arms a [`DragKind::MarkSweep`]: what the
//!   pointer sweeps gets marked.
//! - press on a MARKED row arms a [`DragKind::Transfer`]: the pane's marks
//!   travel to the other pane. That fork — the row's mark state, not a
//!   modifier — is what lets ONE gesture do both jobs.
//! - shift+press on an unmarked row marks the range from the pane's cursor
//!   ([`Effect::MarkRange`]) and arms a sweep anchored at that cursor, so
//!   shift+drag keeps extending the same range.
//!
//! Two rules exist to keep the pointer from acting on the user's behalf:
//! a release that lands outside every row CANCELS instead of guessing a
//! destination, and a drag that never leaves the row it started on is just
//! a click. And the copy/move decision of a transfer is read from the
//! modifiers held at RELEASE, not at press, so that a user who starts
//! dragging and changes their mind does not move files they meant to copy.

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
    /// order ([`crate::PaneState::mark_range`]). It only ever ADDS: see
    /// [`Drag::motion`] for why retreating over a sweep does not unmark.
    MarkRange {
        /// Pane that owns the entries.
        pane: usize,
        /// One end of the range (the anchor).
        from: usize,
        /// The other end (the pointer).
        to: usize,
    },
    /// Send the marks of `from_pane` to `to_pane`. The frontend routes this
    /// through the SAME task submission as the keyboard copy/move — same
    /// confirmation, same policy gate, same journal entry, same undo. A
    /// drop is a mutation, not a quieter second path.
    Transfer {
        /// Pane the marks come from.
        from_pane: usize,
        /// Pane they land in.
        to_pane: usize,
        /// `true` = move, `false` = copy. Read from the modifiers held at
        /// RELEASE.
        move_files: bool,
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
    /// Has the pointer left `origin`? A gesture that never does is a click.
    moved: bool,
}

/// The pointer gesture state machine: pure, allocation-light, and blind to
/// every frontend type. Feed it [`Drag::press`], [`Drag::motion`] and
/// [`Drag::release`]; apply the [`Effect`]s it returns.
///
/// ```
/// use norte_frontend::mouse::{Drag, Effect, Mods, Press, Spot};
///
/// let mut drag = Drag::default();
/// // Press on an unmarked row: focus + cursor, nothing marked yet.
/// let fx = drag.press(Press { at: Spot::new(0, 2), marked: false, cursor: 0, mods: Mods::NONE });
/// assert_eq!(fx, vec![Effect::MoveCursor { pane: 0, index: 2 }]);
/// // Sweeping down marks what it covers.
/// let fx = drag.motion(Spot::new(0, 4));
/// assert!(fx.contains(&Effect::MarkRange { pane: 0, from: 2, to: 4 }));
/// // The release re-states the final range: motions may be coalesced.
/// let fx = drag.release(Some(Spot::new(0, 4)), Mods::NONE);
/// assert!(fx.contains(&Effect::MarkRange { pane: 0, from: 2, to: 4 }));
/// ```
#[derive(Debug, Default)]
pub struct Drag {
    active: Option<Active>,
}

impl Drag {
    /// The button went down. Decides the gesture and returns its immediate
    /// effects (a ctrl toggle and a shift range act at once; a plain press
    /// only moves the cursor — marking on a press would turn every click
    /// into a selection change).
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
        if marked {
            // Fila YA marcada = transferencia de las marcas. `shift` NO se
            // mira aquí: en una transferencia el modificador se lee al
            // soltar, así que arrastrar con shift desde el principio (lo
            // natural para quien quiere mover) sigue siendo una
            // transferencia y no se convierte en un barrido.
            self.active = Some(Active {
                kind: DragKind::Transfer,
                anchor: at,
                origin: at,
                moved: false,
            });
            return vec![Effect::MoveCursor {
                pane: at.pane,
                index: at.index,
            }];
        }
        if mods.shift {
            // Rango desde el CURSOR, que se queda donde está: el ancla
            // sigue visible y un segundo shift+click extiende el MISMO
            // rango en vez de colapsarlo sobre la fila anterior. El
            // `MoveCursor` sobre el propio cursor no mueve nada: dice
            // «enfoca este pane».
            let anchor = Spot::new(at.pane, cursor);
            self.active = Some(Active {
                kind: DragKind::MarkSweep,
                anchor,
                origin: at,
                moved: false,
            });
            return vec![
                Effect::MoveCursor {
                    pane: at.pane,
                    index: cursor,
                },
                Effect::MarkRange {
                    pane: at.pane,
                    from: cursor,
                    to: at.index,
                },
            ];
        }
        // Fila sin marcar y sin modificadores: arma el barrido pero NO
        // marca todavía. Un click suelto debe seguir siendo lo que siempre
        // fue (foco + cursor, marcas intactas); solo el movimiento lo
        // convierte en selección.
        self.active = Some(Active {
            kind: DragKind::MarkSweep,
            anchor: at,
            origin: at,
            moved: false,
        });
        vec![Effect::MoveCursor {
            pane: at.pane,
            index: at.index,
        }]
    }

    /// The pointer moved onto `at` with the button still down. A motion
    /// outside every row is simply not reported (the frontend passes only
    /// what its hit test resolved) — passing over a header must not cancel
    /// a live gesture.
    ///
    /// A sweep re-states its whole range on every motion, so the result
    /// does not depend on how many motions the frontend delivers. It only
    /// ever ADDS: retreating back up a sweep leaves the rows it already
    /// covered marked, because the machine cannot tell which of them the
    /// user had marked BEFORE the drag started — unmarking them would
    /// silently shrink a selection built by hand, and the next bulk
    /// operation would act on less than the user believes it will.
    pub fn motion(&mut self, at: Spot) -> Vec<Effect> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        if at != active.origin {
            active.moved = true;
        }
        let active = *active;
        match active.kind {
            // La transferencia no produce efecto al pasar por encima: el
            // resaltado del pane de destino lo pinta el frontend con su
            // propio evento; aquí no hay ninguna decisión que tomar hasta
            // que se suelta.
            DragKind::Transfer => Vec::new(),
            DragKind::MarkSweep => {
                // Barrer hacia el otro pane no significa nada: se ignora sin
                // cancelar, porque el puntero puede volver.
                if at.pane != active.anchor.pane || !active.moved {
                    return Vec::new();
                }
                vec![
                    Effect::MoveCursor {
                        pane: at.pane,
                        index: at.index,
                    },
                    Effect::MarkRange {
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
            DragKind::MarkSweep => {
                if at.pane != active.anchor.pane {
                    // Soltar el barrido sobre el otro pane: lo barrido
                    // dentro del pane de origen ya está marcado por las
                    // motions; nada que añadir.
                    return Vec::new();
                }
                vec![
                    Effect::MoveCursor {
                        pane: at.pane,
                        index: at.index,
                    },
                    Effect::MarkRange {
                        pane: at.pane,
                        from: active.anchor.index,
                        to: at.index,
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
                }]
            }
        }
    }

    /// Drops any armed gesture without effects — for Esc, a lost focus or a
    /// listing that changed under the pointer.
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
            vec![Effect::MoveCursor { pane: 0, index: 1 }],
            "la pulsación sola no marca: un click suelto no cambia la selección"
        );
        assert_eq!(d.kind(), Some(DragKind::MarkSweep));
        assert_eq!(
            d.motion(Spot::new(0, 3)),
            vec![
                Effect::MoveCursor { pane: 0, index: 3 },
                Effect::MarkRange {
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
                Effect::MarkRange {
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
        d.press(Press {
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
            mods: Mods::SHIFT, // shift YA pulsado al empezar
        };
        let mut d = Drag::default();
        d.press(press);
        assert_eq!(
            d.kind(),
            Some(DragKind::Transfer),
            "shift al pulsar no convierte una fila marcada en barrido"
        );
        assert_eq!(
            d.release(Some(Spot::new(1, 0)), Mods::NONE),
            vec![Effect::Transfer {
                from_pane: 0,
                to_pane: 1,
                move_files: false,
            }],
            "soltó SIN shift: copia, aunque arrancara con shift"
        );

        let mut d = Drag::default();
        d.press(Press {
            mods: Mods::NONE,
            ..press
        });
        assert_eq!(
            d.release(Some(Spot::new(1, 0)), Mods::SHIFT),
            vec![Effect::Transfer {
                from_pane: 0,
                to_pane: 1,
                move_files: true,
            }],
            "soltó CON shift: mueve, aunque arrancara sin shift"
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
        d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        assert!(d.release(None, Mods::SHIFT).is_empty());
        assert_eq!(d.kind(), None, "el gesto queda desarmado, no colgado");

        let mut d = Drag::default();
        d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        d.motion(Spot::new(0, 5));
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

        // Igual con un barrido: press + release en la misma fila no marca.
        let mut d = Drag::default();
        d.press(Press {
            at: Spot::new(1, 0),
            marked: false,
            cursor: 0,
            mods: Mods::NONE,
        });
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
    /// el mismo rango en vez de colapsarlo sobre la fila anterior.
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
            ]
        );
        // Y shift+arrastre sigue extendiendo desde el MISMO ancla.
        assert_eq!(
            d.motion(Spot::new(1, 8)),
            vec![
                Effect::MoveCursor { pane: 1, index: 8 },
                Effect::MarkRange {
                    pane: 1,
                    from: 2,
                    to: 8
                },
            ]
        );
    }

    /// Soltar una transferencia sobre el pane de ORIGEN es un no-op: copiar
    /// un directorio sobre sí mismo no es lo que pidió nadie que arrastró y
    /// se arrepintió a medio camino.
    #[test]
    fn soltar_en_el_pane_de_origen_no_transfiere() {
        let mut d = Drag::default();
        d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        d.motion(Spot::new(1, 1)); // pasea por el otro pane…
        assert!(
            d.release(Some(Spot::new(0, 7)), Mods::NONE).is_empty(),
            "…pero vuelve y suelta en casa"
        );
    }

    /// El barrido solo AÑADE: retroceder no desmarca. La máquina no sabe
    /// cuáles de esas filas ya estaban marcadas ANTES del gesto, y
    /// desmarcarlas encogería en silencio una selección hecha a mano — la
    /// siguiente operación masiva actuaría sobre menos de lo que el usuario
    /// cree.
    #[test]
    fn el_barrido_solo_anade_al_retroceder() {
        let mut d = Drag::default();
        d.press(Press {
            at: Spot::new(0, 2),
            marked: false,
            cursor: 2,
            mods: Mods::NONE,
        });
        d.motion(Spot::new(0, 6));
        assert_eq!(
            d.motion(Spot::new(0, 4)),
            vec![
                Effect::MoveCursor { pane: 0, index: 4 },
                Effect::MarkRange {
                    pane: 0,
                    from: 2,
                    to: 4
                },
            ],
            "el rango se re-emite más corto, pero NADA desmarca 5 y 6"
        );
    }

    /// Un barrido que se sale al otro pane se ignora sin cancelar: el
    /// puntero puede volver.
    #[test]
    fn el_barrido_ignora_el_otro_pane_sin_cancelar() {
        let mut d = Drag::default();
        d.press(Press {
            at: Spot::new(0, 1),
            marked: false,
            cursor: 1,
            mods: Mods::NONE,
        });
        assert!(d.motion(Spot::new(1, 4)).is_empty());
        assert_eq!(d.kind(), Some(DragKind::MarkSweep), "sigue armado");
        assert_eq!(
            d.motion(Spot::new(0, 3)),
            vec![
                Effect::MoveCursor { pane: 0, index: 3 },
                Effect::MarkRange {
                    pane: 0,
                    from: 1,
                    to: 3
                },
            ]
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
        d.press(Press {
            at: Spot::new(0, 2),
            marked: true,
            cursor: 2,
            mods: Mods::NONE,
        });
        d.press(Press {
            at: Spot::new(0, 9),
            marked: false,
            cursor: 9,
            mods: Mods::NONE,
        });
        assert_eq!(d.kind(), Some(DragKind::MarkSweep));
        assert_eq!(d.origin(), Some(Spot::new(0, 9)));
    }
}
