//! El rastro de navegación: adónde vuelve `nav.back`, y qué se rebobina cuando
//! un `cd` no llega a ninguna parte.
//!
//! Un paso solo cuenta si el lector lo dio: un `cd` que falla, que se cancela o
//! que aterriza en el MISMO directorio no deja rastro, porque si lo dejara el
//! `nav.back` siguiente no haría nada visible. Eso es lo que decide [`Rewind`],
//! y por eso [`rewind_for`] necesita el [`crate::navigate::Cd`] entero y no un
//! booleano.
//!
//! Vivía en el root del binario `ntc`, un crate DISTINTO de esta lib, partido en
//! dos por una función de propiedad del teclado que no tiene nada que ver.

use norte_core::backend::Backend;
use norte_i18n::t;
use norte_proto::{EntryKind, Error, VPath};

use crate::app::{App, Trail, TrailStep};
use crate::keymap::Command;
use crate::nav;
use crate::navigate::{Cd, cd_in};

/// One step back for the focused pane, or `None` when the trail is empty.
///
/// Takes `&mut App` because asking IS the step: the trail hands the target
/// over and moves the current directory to the forward branch in one
/// operation, so a caller cannot peek and then forget to walk.
///
/// Reads `dir()` with NO `virtual_search` veto, unlike the mirror and pull
/// gestures, and on purpose. Those need a location to HAND OVER, and a list
/// of hits is not one. This one needs the directory to leave BEHIND on the
/// forward branch, and a results pane has a perfectly good one: its `dir()`
/// is the root the search walked, which is the pane's own directory at the
/// moment the reader pressed the search key — `launch_search` stores the very
/// same value as `SearchRun::prev_dir` to restore on `Esc`, and
/// `pane_gestures_tests::la_raiz_de_la_busqueda_es_el_dir_del_pane_que_la_lanza`
/// pins the equivalence at the seam that could break it. So a step back out
/// of a results pane goes where the reader really was, and the forward branch
/// keeps the directory they really searched from — as a listing, because the
/// hits died with the run this step reaps. Vetoing instead would strand the
/// reader in a results pane, taking away the one key that reads as "get me
/// out of here and back where I came from".
pub fn back_target(app: &mut App) -> Option<VPath> {
    let pane = app.focus();
    let current = app.panes[pane].dir().clone();
    app.history[pane].step_back(current)
}

/// One step forward, undoing a [`back_target`].
pub fn forward_target(app: &mut App) -> Option<VPath> {
    let pane = app.focus();
    let current = app.panes[pane].dir().clone();
    app.history[pane].step_forward(current)
}

/// Rewinds the step [`back_target`]/[`forward_target`] took, because the cd
/// it aimed at FAILED and the reader never actually left where they were.
///
/// The inverse of a step IS the step the other way: `step_forward(target)`
/// pops the `current` that `step_back` pushed onto the forward branch and
/// puts `target` back where it came from. Rewinding through the same two
/// methods is why the two stacks cannot drift — there is no second piece of
/// bookkeeping to get wrong.
pub fn untake_step(app: &mut App, pane: usize, step: TrailStep, target: VPath) {
    let _ = match step {
        TrailStep::Back => app.history[pane].step_forward(target),
        TrailStep::Forward => app.history[pane].step_back(target),
    };
}

/// What a finished trail step should do to the trail it was walking.
///
/// The WHOLE policy of `walk_trail`, in one value the tests can ask for
/// directly. It used to live inline in `walk_trail`, where the only way to
/// pin it was to re-enact the effect in the test — which pins [`nav::History`],
/// not the policy: `walk_trail` could stop rewinding altogether and every
/// test stayed green.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rewind {
    /// Leave the trail as the step left it: the pane really did move, or
    /// something is about to resume the very same navigation.
    No,
    /// Put the step back — the reader never left where they were.
    Step,
    /// Put the step back AND retire the destination from the whole history:
    /// it proved not to be there.
    StepAndRetire,
}

/// Decides, from how a trail step ENDED, what the trail owes the reader.
///
/// A `Failed` never moved the pane, so the step is put back; when the reason
/// is that the directory is GONE it also leaves the history entirely — the
/// same treatment the nav popup already gives a `NotFound`, so the reader is
/// never left with a key that can only aim at a directory that proved not to
/// be there.
///
/// A `Cancelled` rewinds TOO. It is the outcome of `Esc` during a slow
/// listing and of an event stream that died: nothing resumes those, and the
/// pane never moved, so a trail that kept the step would believe the reader
/// left a directory they are still looking at — and the next `nav.forward`
/// would "return" them to the listing already on screen while `back` grew a
/// phantom that eats the following `nav.back` as well. The one outcome that
/// must NOT rewind is `Suspended`: the TOFU modal resumes this very
/// navigation (it carries the pane and the trail mode), and a rewound trail
/// would count the successful retry twice.
///
/// Everything else — the two landings and the outcomes that reach `apply_cd`
/// from elsewhere — means the pane moved or the trail was never involved.
#[must_use]
pub fn rewind_for(outcome: &Cd) -> Rewind {
    match outcome {
        Cd::Failed(Error::NotFound) => Rewind::StepAndRetire,
        Cd::Failed(_) | Cd::Cancelled => Rewind::Step,
        Cd::Suspended | Cd::Filling { .. } | Cd::Replaced(_) | Cd::Refreshed(..) | Cd::Swapped => {
            Rewind::No
        }
    }
}

/// Carries out what [`rewind_for`] decided, on the trail of `pane`.
///
/// Split from the decision so the decision can be read (and tested) without a
/// backend, and joined to it at the ONE call site in `walk_trail` — the
/// tests drive this pair, which is the pair the production path drives.
pub fn rewind_trail(app: &mut App, pane: usize, step: TrailStep, dir: &VPath, rewind: Rewind) {
    match rewind {
        Rewind::No => {}
        Rewind::Step => untake_step(app, pane, step, dir.clone()),
        Rewind::StepAndRetire => {
            untake_step(app, pane, step, dir.clone());
            app.history[pane].remove(dir);
        }
    }
}

/// Whether a REPEATED navigation must stop because this step did not land.
///
/// `nav.back`/`nav.forward` are the only two `counts: true` commands that
/// reach the network (ADR 0044), and [`rewind_for`] puts a `Failed` or
/// `Cancelled` step BACK on the trail — the pane never moved, so the trail
/// must not claim it did. That is right for the trail and fatal for a count:
/// the next turn would take the SAME step and issue the SAME listing, turning
/// one keystroke into up to 9 999 sequential remote calls on a slow or dead
/// host. Worse, `Esc` during a listing IS `Cd::Cancelled`, so the key the
/// reader presses to stop it would be rewound into the next retry and the
/// only way out would be killing norte.
///
/// Asked ONLY of the two trail commands, and that is not tidiness:
/// `Cd::Cancelled` is also the outcome of every command that is not a `cd`
/// (`dispatch` starts from it), so a blanket break on it would stop `5j`
/// after one row.
#[must_use]
pub fn nav_stalled(cmd: Command, outcome: &Cd) -> bool {
    matches!(cmd, Command::NavBack | Command::NavForward)
        && matches!(outcome, Cd::Failed(_) | Cd::Cancelled)
}

/// `nav.back` / `nav.forward`: replays the focused pane's trail one step.
///
/// What a finished step owes the trail is [`rewind_for`]'s call, applied by
/// [`rewind_trail`]; this body only walks.
///
/// A `Cd::Suspended` leaves the step taken on purpose — see
/// [`crate::navigate::settle_suspended_trail`], which is who finishes it.
pub async fn walk_trail(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    step: TrailStep,
) -> Cd {
    let pane = app.focus();
    let target = match step {
        TrailStep::Back => back_target(app),
        TrailStep::Forward => forward_target(app),
    };
    let Some(dir) = target else {
        app.message = Some(t(step.empty_message()));
        return Cd::Cancelled;
    };
    // `Trail::Replay`: el rastro se está recorriendo a sí mismo. Si esto
    // registrara, volver de B a A grabaría «estuve en B» y el siguiente atrás
    // devolvería a B — la misma oscilación que el rastro existe para evitar,
    // un nivel más arriba. LLEVA el paso: si la navegación se SUSPENDE (TOFU),
    // quien responda al modal es quien tendrá que rebobinarlo, y para eso
    // necesita saber en qué sentido iba.
    let outcome = cd_in(app, backend, events, pane, dir.clone(), Trail::Replay(step)).await;
    rewind_trail(app, pane, step, &dir, rewind_for(&outcome));
    outcome
}

/// Whether `nav.enter` on the cursor's current entry navigates anywhere, and
/// to what. También symlinks: si apunta a un dir, el provider listará; si
/// no, el cd falla y se absorbe — qué es "entrable" lo decide el core, no el
/// TUI (regla 7). Un File .zip/.tar entra como directorio virtual (ADR
/// 0018): el TUI solo COMPONE el path (azúcar de navegación); listar/validar
/// sigue siendo del core.
///
/// Factored out of `Command::NavEnter` (S2, `--pick`) because the picker's
/// Enter override needs the exact same answer to a different question: "is
/// there anything here for Enter to DO", without wanting the `VPath` or
/// running the `cd`. Two call sites computing this independently is two call
/// sites that can quietly disagree about what a cursor "on a directory"
/// means.
#[must_use]
pub fn nav_enter_target(app: &App) -> Option<VPath> {
    // La fila `..` no es un operando —`selected()` contesta `None` sobre
    // ella, que es lo que la hace inofensiva— así que subir se pregunta
    // aparte. Es lo único que esa fila sabe hacer.
    let pane = app.focused();
    if pane.cursor_is_parent_row() {
        return pane.parent_target().cloned();
    }
    pane.selected()
        .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Symlink))
        .map(|e| e.path.clone())
        .or_else(|| app.focused().selected().and_then(nav::archive_root_for))
}
