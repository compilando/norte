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

use norte_core::backend::Backend;
use norte_i18n::t;

use crate::app::{App, HelpView, Modal, Palette};
use crate::keymap::parse_plugin_key;

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

/// up/down sobre los modales con ventana propia — el scroll del plan IA
/// (M4-IA, audit MAJOR-3) y el cursor de los hits semánticos (M4-IA-2).
/// Mueven la VENTANA o el CURSOR y JAMÁS confirman/cancelan: mismo par de
/// comandos que los pickers (`ALLOW_PICKER`); para `dialog_action` up/down
/// están FUERA del allowlist de decisión de estos modales (devuelve `None`,
/// pin en tests/modal.rs), así que el enrutado vive aquí, como el dispatch
/// de los pickers vive en su `on_*_key`. `true` = comando CONSUMIDO.
/// `F1` (o lo que el keymap ate a `app.help`) SOBRE un modal abierto: abre la
/// ayuda del contexto de ESE modal. `true` = comando CONSUMIDO.
///
/// Vive aquí por lo mismo que [`modal_scroll`]: `app.help` es un comando de
/// `[global]`, no un verbo `dialog.*`, así que el allowlist del modal concreto
/// ([`crate::app::dialog_action`]) lo deja caer — y sin esta rama la única tecla que el
/// lector tiene garantizada sería inerte justo donde más falta hace, delante de
/// una pregunta que no entiende. Es el gemelo del interruptor de
/// `on_help_key`: la misma tecla que abre la ayuda la cierra, y lo
/// hardcodeado es el SIGNIFICADO, jamás la tecla.
///
/// Qué modales lo admiten es una DECISIÓN, no la resaca del enrutado: lo dice
/// [`crate::help_context::help_over_modal_allowed`], exhaustivo sobre
/// `Modal` y sin comodín (review MINOR-3). Los seis editores de TEXTO LIBRE
/// (`Mkdir`, `MarkPattern`, `CommandLine`, `AiRenameInstruction`,
/// `SemanticQuery`, `TransferName`) y el TOFU de Lua responden `false`: hoy
/// tampoco llegan aquí
/// —el run loop los intercepta antes para leer teclas CRUDAS (decisión 8 del
/// plan H1: el keymap no puede reinterpretar lo que se está escribiendo)—, y
/// preguntarlo AQUÍ es lo que impide que mover uno de ellos al keymap `dialog`
/// abra el agujero en silencio. Sus contextos existen en el vocabulario y sus
/// páginas se alcanzan por el índice.
pub fn modal_help_toggle(
    app: &mut App,
    cmd: &str,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
) -> bool {
    if cmd != "app.help" {
        return false;
    }
    // No consumir la tecla cuando la ayuda no puede abrirse: quien decide
    // vuelve a ser el allowlist del modal (`dialog_action`), que deja caer
    // `app.help` — la tecla queda INERTE, que es lo que se quiere.
    if app
        .modal
        .as_ref()
        .is_some_and(|m| !crate::help_context::help_over_modal_allowed(m))
    {
        return false;
    }
    // Sin foto de plugins (H3e): esta rama es SÍNCRONA — la cadena de teclas
    // del modal lo es — y pedirla cuesta una ida y vuelta al daemon. La
    // degradación es exactamente la documentada para `plugins: None`: la ayuda
    // que se abre sobre un diálogo no ofrece filas de extensión. Es la
    // superficie donde menos se echa en falta: el lector está contestando una
    // pregunta, no explorando el catálogo, y el grupo entero está a un `Esc` y
    // un F1 de distancia.
    open_contextual_help(app, lang, help_lines, None);
    true
}

/// `dialog.pane-up/down` sobre un modal con ventana propia: mueve la ventana
/// o el cursor y JAMÁS confirma ni cancela. `true` = comando CONSUMIDO.
pub fn modal_scroll(app: &mut App, cmd: &str) -> bool {
    if !matches!(cmd, "dialog.up" | "dialog.down") {
        return false;
    }
    let down = cmd == "dialog.down";
    match app.modal {
        Some(Modal::AiRenamePlan { .. }) => {
            app.ai_plan_scroll(down);
            true
        }
        Some(Modal::SemanticHits { .. }) => {
            app.semantic_cursor(down);
            true
        }
        // #311: sin esto, un lote de cuarenta ficheros con el que NO cuadra en
        // la fila doce enseñaba cinco «correcto» y «… y 35 más», y no había
        // tecla que llegara al malo.
        Some(Modal::Checksums { .. }) => {
            app.checksums_scroll(down);
            true
        }
        _ => false,
    }
}

/// `Command::AppHelp`: opens the overlay on the page about where the reader IS.
///
/// The context comes from [`crate::help_context::help_context`] (the TUI's
/// closed vocabulary, anchored on `Modal`) and the page from the CORPUS, so
/// moving an explanation between pages is an edit to prose.
///
/// A word on the overlays that are NOT in that vocabulary — the palette, the
/// settings overlay, the theme and column pickers, the extension manager, the
/// nav popup, the search dialog. `help_context` answers `browse` for all of
/// them, and that answer is UNREACHABLE: each of those arms sits ahead of this
/// dispatch in the run loop's key chain with its own fixed keys, so `F1` there
/// is inert and never gets here. The one exception proves it — the palette's
/// `Enter` can dispatch `app.help`, and it clears `app.palette` BEFORE
/// dispatching, so by the time this runs the palette is gone and `browse` (or
/// `viewer`) is the honest answer. Growing the vocabulary for those overlays
/// would be vocabulary for a state that cannot happen.
///
/// `lang` is the NEGOTIATED language (`NORTE_LANG` > `[ui] lang` >
/// environment — the value `main` handed to `norte_i18n::force`), never
/// `Lang::from_env()`: the corpus is per-locale and a page in another language
/// than the chrome around it is the same bug as a half-translated dialog.
/// `help_lines` is the body of the synthetic keyboard entry, snapshotted from
/// the VIGENTE keymap (rebuilt by the hot reload, which also closes an open
/// overlay so no snapshot survives a rebind).
///
/// OVER A MODAL, a context with no page opens NOTHING (review MAJOR-2). The
/// index is the least surprising landing from a pane or the viewer — nothing
/// there is waiting on a decision — but over a dialog it covers a live question
/// with "Welcome to norte — norte is an orthodox file manager. Two panes…",
/// freezes the dialog's verbs, replaces its footer, and lets the reader walk
/// from the index into another dialog's `y`/`n` prose while an agent approval
/// waits behind it. So the reader is TOLD and the prompt stays answerable —
/// the same decision [`palette_help`] already makes for an undocumented row,
/// applied where it matters more. The pages still missing are on the
/// documentation gate's shrinking allowlist, so this is temporary by
/// construction.
/// `plugins` is the catalogue as of the moment the reader pressed the key
/// (H3e), or `None` when it could not be asked for. It arrives as a PARAMETER
/// because this function is SYNC — its callers are async and its tests are not
/// — and it is taken ONCE, on the open path, never while painting. `None` and
/// an empty catalogue land in the same place: no plugin rows in the sidebar and
/// every `plugin:` command dimmed, which is the honest answer to "I could not
/// find out".
/// Whether `F1` must refuse to open here: over a modal, with no page for this
/// context.
///
/// Pulled out of [`open_contextual_help`] so the refusal can be tested for what
/// it IS rather than through whichever modal happens to be undocumented. Since
/// H3h no context is: the documentation gate has no allowlist left, so a new
/// context arrives with its page or fails the build. That makes this guard
/// unreachable through the UI today and worth keeping anyway — it is the
/// fail-safe for the one way a context could still lose its page, which is
/// somebody deleting the page.
#[must_use]
pub fn refuses_over_modal(lang: norte_help::Lang, context: &str, over_modal: bool) -> bool {
    over_modal && norte_help::topic_for_context(lang, context).is_none()
}

/// Abre la ayuda en la página del CONTEXTO en el que está el lector.
///
/// El contexto lo decide [`crate::help_context::help_context`] y no esta
/// función: qué página corresponde a qué pantalla es una decisión del corpus,
/// exhaustiva y sin comodín. Si ese contexto no tiene página y hay un modal
/// delante, se dice y no se abre nada ([`refuses_over_modal`]).
pub fn open_contextual_help(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
    plugins: Option<&norte_proto::methods::PluginListResult>,
) {
    let context = crate::help_context::help_context(app);
    let over_modal = app.modal.is_some();
    if refuses_over_modal(lang, context, over_modal) {
        app.message = Some(t("msg-help-no-dialog-page"));
        return;
    }
    // H3d: los hechos del contexto se CONGELAN aquí, antes de la primera
    // maquetación — un veredicto no puede cambiar bajo el cursor del lector a
    // mitad de página (`App::freeze_help_facts`).
    app.freeze_help_facts();
    app.help = Some(HelpView::new_at(
        lang,
        help_lines.to_vec(),
        context,
        over_modal,
    ));
    // H3e: el estado de los plugins se CONGELA con el resto de los hechos, en
    // las dos mitades a la vez (barra lateral y resolver) —
    // `App::freeze_help_plugins`.
    //
    // SIEMPRE, incluso sin catálogo: el resolver vive en `App` y sobrevive al
    // cierre del overlay, así que no congelar aquí dejaría en pie la foto de la
    // ayuda ANTERIOR. Un catálogo vacío es la respuesta honesta a «no lo pude
    // averiguar» — ninguna fila de extensión, y todo comando `plugin:`
    // atenuado — y fail-closed es la dirección en la que equivocarse.
    app.freeze_help_plugins(plugins.map_or(&[], |l| l.plugins.as_slice()));
}

/// Fetches the page of the plugin node the reader just opened (H3e).
///
/// On demand and once: 64 KiB per plugin must not ride every `plugin.list`, and
/// a page already installed is never asked for again within one overlay. The
/// "once" is [`HelpView::claim_plugin_fetch`]'s job — `plugin_needs_fetch` is a
/// POLLING question and this runs on every turn of the run loop, so without the
/// claim a dead daemon would be re-asked at frame rate.
///
/// A failure is SILENT on purpose — an empty page with the plugin's name is a
/// better answer than an error toast over a help overlay, and a daemon N-1
/// without the handler lands here too (`plugin.help` is 0.34.0). The page stays
/// blank for the life of the overlay; closing and reopening the help is the
/// retry.
///
/// The overlay is re-borrowed AFTER the await: the reader may have closed it, or
/// moved to another page, while the answer was in flight.
pub async fn fetch_plugin_page(backend: &Backend, app: &mut App) {
    let Some(id) = app.help.as_mut().and_then(HelpView::claim_plugin_fetch) else {
        return;
    };
    let Ok(res) = backend.plugin_help(&id).await else {
        return;
    };
    let Some(help) = app.help.as_mut() else {
        return;
    };
    // The publisher comes from the SNAPSHOT, already masked and capped, never
    // from the page: a plugin does not get to say who published it.
    // `parse_untrusted` masks it again, which is harmless.
    let publisher = help.publisher_of(&id);
    // `fold_flags` is not optional: the text arrives already short and already
    // decoded, so this parse comes out clean and the badge — the whole
    // user-facing mitigation for a hostile `help.md` — would go dark.
    let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), &id, publisher)
        .fold_flags(res.truncated, res.lossy);
    help.state.install_plugin_topic(parsed.topic);
}

/// The page that documents the palette row under the cursor, if one does (H3c).
///
/// The other direction of the bridge H3b built: from the help, `Ctrl+P` carries
/// the filter into the palette; from the palette, `F1` opens the page about the
/// highlighted command. Two views of one model at two densities, so crossing
/// between them should not cost the reader a re-type.
///
/// A PLUGIN row is answered `None` explicitly. Its key is
/// `plugin:{id}:{command}` ([`parse_plugin_key`]), which no corpus page
/// documents and which is not a host command either — the `command_id` half
/// comes from a third-party manifest with no validated charset, so it must never
/// be handed to a lookup as if it were one of ours. The corpus lookup would also
/// answer `None` on its own; the guard is what makes that a decision instead of
/// a coincidence, and it is the same `key`/`text` split the palette already
/// makes between dispatch and paint.
fn palette_help_target(app: &App, lang: norte_help::Lang) -> Option<&'static norte_help::Topic> {
    let key = app.palette.as_ref().and_then(Palette::selected)?;
    if parse_plugin_key(&key).is_some() {
        return None;
    }
    norte_help::topic_for_command(lang, &key)
}

/// `F1` inside the command palette: open the page for the highlighted row, or
/// say that no page documents it (H3c).
///
/// On success the palette CLOSES — the help takes the screen and the next key
/// belongs to what the reader is looking at — and the page arrives as the root
/// of the trail ([`HelpView::new_at_topic`]), so one `Esc` leaves it.
///
/// On failure the palette STAYS and the status bar says so. Opening the index
/// instead would be worse than nothing: the reader asked about one command and
/// would land on a table of contents, with no way to tell whether their command
/// is in there somewhere or simply undocumented.
///
/// `over_modal` is `false` and not `app.modal.is_some()`: the palette's arm of
/// the key chain only runs when no modal is on screen (`modal_wins`), so there
/// is no modal for this help to have been opened over.
pub fn palette_help(
    app: &mut App,
    lang: norte_help::Lang,
    help_lines: &[ratatui::text::Line<'static>],
) {
    match palette_help_target(app, lang).map(|topic| topic.id.clone()) {
        Some(id) => {
            app.palette = None;
            // H3d: mismo congelado que `open_contextual_help` — la ayuda que
            // se abre desde la palette es la misma ayuda.
            app.freeze_help_facts();
            app.help = Some(HelpView::new_at_topic(
                lang,
                help_lines.to_vec(),
                &id,
                false,
            ));
        }
        None => app.message = Some(t("msg-palette-no-help")),
    }
}

/// ¿Puede un evento de vigilancia disparar un refresh AHORA? (#106,
/// review MAJOR-2): con cualquier overlay abierto o un quick search
/// tecleándose, `refresh_panes` consumiría las teclas del usuario (su loop
/// de cancelación descarta todo lo que no sea Esc/Ctrl-C) y Esc pasaría a
/// significar «abandona el refresh» — jamás pisar la interacción en curso.
/// El evento queda encolado (capacidad 1) y dispara al despejarse.
#[must_use]
pub fn watch_refresh_allowed(app: &App) -> bool {
    app.modal.is_none()
        && app.palette.is_none()
        && app.settings.is_none()
        && app.help.is_none()
        && app.viewer.is_none()
        && app.theme_picker.is_none()
        && app.columns_picker.is_none()
        && app.extensions.is_none()
        && app.nav_popup.is_none()
        && app.search_dialog.is_none()
        && app.panes.iter().all(|p| p.quick().is_none())
}

#[cfg(test)]
mod palette_modal_guard_tests {
    use super::*;
    use crate::app::{App, Modal, Palette, Pane, Settings};
    use crate::nav;
    use norte_proto::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    fn approval_modal() -> Modal {
        Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        }
    }

    /// MINOR-4 (H1 close): con SOLO la palette abierta, no hay nada que
    /// preceder — el guard no dispara. Con AMBOS abiertos (un modal llegó
    /// asíncronamente encima de la palette), el modal debe ganar.
    #[test]
    fn modal_preempts_palette_solo_cuando_ambos_estan_abiertos() {
        let mut a = app();
        assert!(!modal_wins(&a), "sin modal, nadie precede a nadie");
        a.palette = Some(Palette::new(Vec::new()));
        assert!(
            !modal_wins(&a),
            "solo la palette abierta: la palette maneja sus teclas normalmente"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "un modal en vuelo con la palette abierta DEBE ganarle"
        );
    }

    /// El guard vale para CUALQUIER overlay, no solo palette/ajustes: el
    /// modal se pinta el último (por encima de todos), así que la tecla que
    /// el usuario dirige a lo que VE tiene que llegarle. Antes el selector
    /// de tema, el picker de columnas, el gestor de extensiones, el popup de
    /// navegación, el diálogo de búsqueda y la ayuda resolvían PRIMERO y se
    /// comían la respuesta al modal (en los dos con campo de texto, como
    /// texto tecleado; en extensiones, como toggle/borrado del plugin
    /// resaltado).
    #[test]
    fn el_modal_gana_a_todos_los_overlays() {
        let mut a = app();
        a.theme_picker = Some(crate::app::ThemePicker {
            names: Vec::new(),
            cursor: 0,
            original: a.theme.clone(),
        });
        a.extensions = Some(crate::app::ExtensionManager {
            plugins: Vec::new(),
            errors: Vec::new(),
            cursor: 0,
            config: None,
        });
        a.help = Some(crate::app::HelpView::new(norte_i18n::Lang::En, Vec::new()));
        assert!(!modal_wins(&a), "sin modal, cada overlay manda en su tecla");
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "con overlays abiertos, el modal sigue ganando la tecla"
        );
    }

    /// #106 (review MAJOR-2): un evento de vigilancia JAMÁS refresca con
    /// un overlay abierto o un quick search tecleándose — `refresh_panes`
    /// se comería las teclas y Esc cambiaría de significado. El evento
    /// queda encolado y dispara al despejarse.
    #[test]
    fn watch_refresh_gateado_por_overlays() {
        let mut a = app();
        assert!(watch_refresh_allowed(&a), "sin overlays: permitido");
        a.modal = Some(approval_modal());
        assert!(!watch_refresh_allowed(&a), "modal abierto: encolado");
        a.modal = None;
        a.help = Some(crate::app::HelpView::new(norte_i18n::Lang::En, Vec::new()));
        assert!(!watch_refresh_allowed(&a), "ayuda abierta: encolado");
        a.help = None;
        a.panes[0].quick_start(nav::Mode::Filter);
        assert!(
            !watch_refresh_allowed(&a),
            "quick search tecleándose: encolado"
        );
    }

    /// S3: el mismo caso para `app.settings` — un modal en vuelo (p.ej. una
    /// aprobación de policy) gana sobre el overlay de ajustes abierto.
    #[test]
    fn modal_preempts_settings_solo_cuando_ambos_estan_abiertos() {
        let mut a = app();
        assert!(!modal_wins(&a));
        a.settings = Some(Settings::new(Vec::new()));
        assert!(
            !modal_wins(&a),
            "solo el overlay de ajustes abierto: maneja sus teclas normalmente"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_wins(&a),
            "un modal en vuelo con ajustes abierto DEBE ganarle"
        );
    }
}

#[cfg(test)]
mod palette_help_tests {
    use super::*;
    use crate::app::{App, Palette, Pane};
    use norte_proto::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// La palette abierta con UNA fila, la de `key`, bajo el cursor. Las filas
    /// se construyen a mano y no del keymap efectivo a propósito: lo que se
    /// prueba es qué hace `F1` con la clave de despacho de la fila resaltada, y
    /// una fila de plugin no sale de `COMMANDS`.
    fn app_with_palette_on(key: &str) -> App {
        let mut app = app();
        app.palette = Some(Palette::new(vec![crate::palette::Row {
            key: key.to_owned(),
            text: key.to_owned(),
            desc: "descripción de prueba".to_owned(),
            chord: "—".to_owned(),
            hostile: false,
        }]));
        app
    }

    /// `F1` sobre una fila de la palette abre la página que documenta ese
    /// comando: los dos son vistas del mismo modelo a dos densidades, así que
    /// cruzar de la rápida a la que explica no debería costar re-teclear.
    #[test]
    fn f1_en_la_palette_abre_la_pagina_del_comando_bajo_el_cursor() {
        let app = app_with_palette_on("pane.copy");
        let open = palette_help_target(&app, norte_help::Lang::En).expect("pane.copy tiene página");
        assert_eq!(open.id.as_str(), "copying");
    }

    /// …y la abre de verdad: la palette se cierra (la tecla siguiente es de la
    /// ayuda, que es lo que se ve) y la página llega como RAÍZ del rastro —
    /// `Esc` cierra el overlay en vez de caminar a un índice que el lector no
    /// pidió, igual que la ayuda contextual de un modal.
    #[test]
    fn abrir_la_pagina_cierra_la_palette_y_llega_sin_historial() {
        let mut app = app_with_palette_on("pane.copy");
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.palette.is_none(), "la palette se cierra");
        let help = app.help.as_mut().expect("la ayuda se abrió");
        assert_eq!(help.state.current().as_str(), "copying");
        assert!(
            !help.over_modal,
            "la rama de la palette solo corre sin modal en pantalla"
        );
        assert!(!help.state.back(), "sin historial: Esc cierra");
        assert!(app.message.is_none(), "y nada que disculparse");
    }

    /// Una fila SIN página no abre nada y lo dice: mejor que abrir el índice y
    /// dejar al lector buscando qué tenía que ver con lo que pidió.
    #[test]
    fn una_fila_sin_pagina_lo_dice() {
        // Un id SINTÉTICO, y no un comando real de la allowlist: desde H3h no
        // queda ninguno sin página, así que un test que se apoyara en ese
        // hueco mediría el corpus y no la rama. Esta rama sigue existiendo —
        // `topic_for_command` puede contestar `None` — y lo que se pinta
        // entonces es lo que hay que fijar.
        let mut app = app_with_palette_on("app.no-such-command");
        assert!(palette_help_target(&app, norte_help::Lang::En).is_none());
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none(), "no se abre el índice por consolar");
        assert!(app.palette.is_some(), "y la palette se queda donde estaba");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-palette-no-help").as_str())
        );
    }

    /// La `key` de una fila de PLUGIN es `plugin:{id}:{command}` (P1): ningún
    /// tema del corpus la documenta y no es un comando del host. Toma el camino
    /// de «sin página» — ni pánico, ni una página ajena, ni un `Command::parse`
    /// que no le corresponde.
    #[test]
    fn una_fila_de_plugin_toma_el_camino_de_sin_pagina() {
        let mut app = app_with_palette_on("plugin:dev.norte.demo:greet");
        assert!(palette_help_target(&app, norte_help::Lang::En).is_none());
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none());
        assert!(app.palette.is_some());
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-palette-no-help").as_str())
        );
    }

    /// Sin ninguna fila visible (un filtro que no casa nada) no hay comando que
    /// documentar: mismo camino, sin `unwrap` de por medio.
    #[test]
    fn sin_fila_visible_no_hay_pagina() {
        let mut app = app_with_palette_on("pane.copy");
        for c in "zzzz".chars() {
            app.palette.as_mut().expect("abierta").push_char(c);
        }
        assert!(app.palette.as_ref().expect("abierta").visible().is_empty());
        assert!(palette_help_target(&app, norte_help::Lang::En).is_none());
        palette_help(&mut app, norte_help::Lang::En, &[]);
        assert!(app.help.is_none());
        assert!(app.palette.is_some());
    }
}
