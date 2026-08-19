//! Quién se come la tecla cuando hay overlays apilados.
//!
//! Dos preguntas y nada más: si el modal gana sobre cualquier otro overlay
//! ([`modal_wins`]) y si una página de ayuda ABIERTA ENCIMA de un modal le ha
//! quitado el teclado ([`help_owns_keys`]). Y las dos escrituras que mantienen
//! esa respuesta honesta: [`settle_help_over_modal`], que borra el recuerdo al
//! empezar cada vuelta, y [`close_stale_overlays`], que cierra lo que un modal
//! nuevo acaba de dejar obsoleto. Las lecturas las consultaba la cadena de
//! teclado del binario, y también el enrutador de pegado
//! ([`crate::paste::route_paste`]) — que es lo que hacía imposible sacar el
//! pegado de `main.rs` sin sacar esto antes.

use crate::app::App;

/// MINOR-4 (H1 close): un modal puede llegar de forma ASÍNCRONA (p. ej.
/// `Modal::ApproveAgentOp`, vía `ConnEvent` — un agente pide aprobación en
/// cualquier momento) mientras la palette está abierta. Sin este guard, el
/// run loop resolvía la tecla contra la palette PRIMERO (`app.palette.is_some()`
/// se comprobaba antes que `app.modal.is_some()`): un Enter pulsado para
/// responder al modal en realidad despachaba la fila resaltada de la
/// palette EN SILENCIO, y el modal de seguridad seguía esperando una
/// respuesta que nunca llegó por esa tecla. El modal SIEMPRE gana: la rama
/// de la palette del run loop excluye este caso de su condición (deja de
/// consumir la tecla) y la rama del modal cierra la palette, ahora obsoleta,
/// nada más entrar — la MISMA tecla cae al modal en la misma iteración.
///
/// GENERALIZADO a TODOS los overlays: el guard valía solo para la palette y
/// los ajustes, pero el modal se pinta el ÚLTIMO —por encima de CUALQUIER
/// overlay ([`crate::ui::draw`])— mientras la cadena de teclado del run
/// loop resolvía ANTES contra el selector de tema, el picker de columnas, el
/// gestor de extensiones, el popup de navegación, el diálogo de búsqueda y la
/// ayuda. Los píxeles decían «responde al modal» y la tecla se iba a otra
/// parte: en el diálogo de búsqueda y en el campo de nombre del popup se
/// colaba como TEXTO tecleado, y en el gestor de extensiones como un
/// `dialog.toggle-enabled`/`dialog.remove` sobre el plugin resaltado — la
/// misma edición silenciosa de MINOR-4, con peor desenlace.
#[must_use]
pub fn modal_wins(app: &App) -> bool {
    app.modal.is_some()
}

/// Does the open help overlay own this key press? (H3c)
///
/// The ONE hole in [`modal_wins`], and it is shaped by which of the two
/// arrived first — the flag `HelpView::over_modal` is what remembers:
///
/// * help opened FROM a modal keeps the keys. Otherwise `F1` over a dialog
///   would open a page whose cursor keys all belong to the dialog underneath:
///   an overlay the reader asked for and cannot use.
/// * a modal that ARRIVED over an already-open help does NOT lose the key. The
///   help is the stale one there, and [`close_stale_overlays`] is what retires
///   it — same treatment the palette and the settings overlay already get.
///
/// While the help owns the keys the modal's own verbs are unreachable, which is
/// the point: nothing gets approved through a page covering it. The modal is
/// still painted on top ([`crate::ui::draw`] paints it last), so the
/// question is never HIDDEN — only unanswerable until the help closes, and its
/// TTL running out denies the agent.
///
/// The flag decides ONLY while a modal is live, and it cannot be stale by the
/// time it is read: [`settle_help_over_modal`] clears it at the top of every
/// turn of the run loop, so it always describes the modal that is on screen NOW
/// rather than one that has since been answered (review MINOR-1).
#[must_use]
pub fn help_owns_keys(app: &App) -> bool {
    match app.help.as_ref() {
        // Ownership expressed as the two cases rather than as one boolean: with
        // a modal live the flag decides; with none there is nobody to compete
        // with and the help owns the key anyway.
        Some(help) => {
            if app.modal.is_some() {
                help.over_modal
            } else {
                debug_assert!(
                    !help.over_modal,
                    "`over_modal` con `app.modal` vacío: \
                     `settle_help_over_modal` no corrió esta vuelta"
                );
                true
            }
        }
        None => false,
    }
}

/// Makes `HelpView::over_modal` a fact about the PRESENT (review MINOR-1).
///
/// The flag is set once, when the overlay opens, and [`help_owns_keys`] and
/// [`close_stale_overlays`] both trust it later. That trust is only sound while
/// it still describes the modal on screen: if the modal an `over_modal` help was
/// opened over went away and a DIFFERENT one arrived, the help would keep the
/// keys and `close_stale_overlays` would never retire it — the new prompt
/// unanswerable until the reader closes a page about a dialog that no longer
/// exists.
///
/// Unreachable today (every async writer refuses to touch a live modal, and the
/// paths that close one are the paths that answer it), but the argument for that
/// spanned four functions. Clearing the flag whenever no modal is live makes it
/// one line: the memory cannot outlive what it is a memory OF, so `over_modal`
/// being `true` means a modal was there on the previous turn AND is there now.
///
/// Called at the top of the run loop, BEFORE the retained modals (the AI plan,
/// the semantic hits) are planted: one of those arriving must find the flag
/// already cleared, so it is treated as a modal arriving over an open help.
pub fn settle_help_over_modal(app: &mut App) {
    if app.modal.is_none()
        && let Some(help) = app.help.as_mut()
    {
        help.over_modal = false;
    }
}

/// Retires the overlays a modal has made obsolete (MINOR-4, H1 close; extended
/// to the help in H3c).
///
/// Called from the modal arm of the run loop's key chain — i.e. exactly when a
/// modal has the key and some overlay is still on screen. The palette and the
/// settings overlay are dropped because their rows EXPIRE (they were built
/// against a state the modal is about to change) and because that same key must
/// reach the modal instead of vanishing into a filter.
///
/// The help is dropped too, but only when it did not open over this modal:
/// `over_modal` help is the reader's deliberate "explain this dialog to me",
/// and it owns the keys ([`help_owns_keys`]), so this function is never even
/// reached while one is open. The guard states that, rather than relying on the
/// caller to.
///
/// The other overlays (theme selector, column picker, extensions, nav popup,
/// search dialog) yield the key but SURVIVE: their rows do not expire and the
/// reader gets them back intact after answering.
pub fn close_stale_overlays(app: &mut App) {
    app.palette = None;
    app.settings = None;
    // K3c: y con ajustes se va el editor de atajos, que vive ENCIMA de él —
    // dejarlo huérfano sobre un overlay cerrado haría que `Esc` cayera a los
    // panes en vez de volver donde el lector estaba. Además sus filas expiran
    // por la misma razón que las de la palette: el modal va a cambiar el estado
    // contra el que se construyeron.
    app.shortcuts = None;
    if app.help.as_ref().is_some_and(|help| !help.over_modal) {
        app.help = None;
    }
}
