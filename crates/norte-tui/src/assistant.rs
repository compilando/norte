//! The assistant panel in the terminal (ADR 0186): typing, sending a turn as
//! a job of the loop, and Apply — which hands the order to the box's own
//! executor (`crate::orders::run`), so every confirmation still asks.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_proto::Error;
use norte_proto::methods::AiChatTurn;

use crate::app::{App, KeyOwner};
use crate::console::Console;
use crate::jobs::InFlight;
use crate::navigate::{cd, settle_cd};

/// The panel's kind id.
pub const KIND: &str = "assistant";

/// Whether this key belongs to the panel's input: plain keys the input uses,
/// with the panel holding the keyboard. A chord with ctrl/alt writes
/// nothing, so `alt+Y` still closes the panel from inside; Tab and the F
/// keys reach the panel commands, as in the window.
#[must_use]
pub fn types_text(app: &App, mods: KeyModifiers, code: KeyCode) -> bool {
    app.key_owner() == KeyOwner::Assistant
        && (mods.is_empty() || mods == KeyModifiers::SHIFT)
        && !matches!(code, KeyCode::Tab | KeyCode::BackTab | KeyCode::F(_))
}

/// Sends what is typed, if anything and if nothing is in flight.
pub fn submit(app: &mut App, backend: &Backend, work: &mut InFlight) {
    let here = app.focused().dir().clone();
    let other = app.target_index().map(|i| app.panes[i].dir().clone());
    let Some(params) = app.assistant.submit(&here, other.as_ref()) else {
        return;
    };
    send(backend, work, params);
}

/// Sends `text` as the reader's message without touching the input (`@` in
/// the box); while a turn is in flight it is refused and said.
pub fn ask(app: &mut App, backend: &Backend, work: &mut InFlight, text: &str) {
    let here = app.focused().dir().clone();
    let other = app.target_index().map(|i| app.panes[i].dir().clone());
    match app.assistant.ask(text, &here, other.as_ref()) {
        Some(params) => send(backend, work, params),
        None if app.assistant.pending() => {
            app.message = Some(norte_i18n::t("assistant-busy"));
        }
        None => {}
    }
}

fn send(backend: &Backend, work: &mut InFlight, params: norte_proto::methods::AiChatParams) {
    let b = backend.clone();
    let handle = tokio::spawn(async move { b.ai_chat(&params).await });
    if let Some(old) = work.assistant.replace(handle) {
        old.abort();
    }
}

/// The turn arrived (or why not).
pub fn harvest(
    app: &mut App,
    work: &mut InFlight,
    res: Result<Result<AiChatTurn, Error>, tokio::task::JoinError>,
) {
    work.assistant = None;
    let lang = norte_i18n::active();
    let turn = match res {
        Ok(Ok(turn)) => Ok(turn),
        Ok(Err(e)) => Err(norte_frontend::assistant::failure_text(&e, lang)),
        // The task died: said, or the panel would wait forever.
        Err(_) => Err(norte_i18n::t_in(lang, "err-internal")),
    };
    app.assistant.land(turn);
    app.needs_frame = true;
}

/// A key with the panel holding the keyboard. Enter sends what is typed;
/// with nothing typed and the cursor on an order, Enter APPLIES it.
pub async fn on_key(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    work: &mut InFlight,
    mods: KeyModifiers,
    code: KeyCode,
) {
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    app.needs_frame = true;
    match code {
        KeyCode::Char(c) if plain => app.assistant.push_char(c),
        KeyCode::Backspace if plain => app.assistant.backspace(),
        KeyCode::Up if plain => app.assistant.up(),
        KeyCode::Down if plain => app.assistant.down(),
        // Esc first stops a turn in flight; with none, it gives the keys
        // back.
        KeyCode::Esc if plain => {
            if app.assistant.cancel() {
                if let Some(h) = work.assistant.take() {
                    h.abort();
                }
            } else {
                app.return_keys_to_panes();
            }
        }
        KeyCode::Enter if plain => {
            // Strictly empty: a line of spaces is not "apply".
            if app.assistant.input().is_empty() {
                apply(app, backend, events, work).await;
            } else {
                submit(app, backend, work);
            }
        }
        _ => {}
    }
}

/// Runs the order under the cursor through the box's executor, against the
/// CURRENT pane: the same preview and confirmations as typing it with `:`.
pub async fn apply(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    work: &mut InFlight,
) {
    let Some(text) = app.assistant.selected_order().map(str::to_owned) else {
        return;
    };
    let Ok(order) = norte_orders::parse(&text) else {
        // Vetted by the core's grammar; only a core newer than this
        // frontend's gets here, and silence would read as a dead key.
        app.message = Some(norte_i18n::t("assistant-unparsable"));
        return;
    };
    app.return_keys_to_panes();
    if let Some(norte_frontend::goto::Action::Ir(dir)) =
        crate::orders::run(app, backend, &order).await
    {
        let outcome = cd(app, backend, events, dir).await;
        settle_cd(
            app,
            backend,
            &mut work.fill,
            &mut work.decorate,
            &mut work.probed,
            &mut work.search,
            outcome,
        );
    }
}
