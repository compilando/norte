//! The query to the semantic index that feeds the section of the same name
//! in "go to anywhere" (phase 6).
//!
//! It is spawned rather than inline for the usual reason: behind the index
//! there is an embeddings model, and sometimes a remote provider. Asking for
//! it inside the key handler would freeze the screen between one keystroke
//! and the next.
//!
//! It is the SIBLING of [`crate::jobs::ai::harvest_semantic`], not the same
//! thing: the `ai.search` search opens a MODAL with its results, while this
//! one fills a section of a screen that stays open. They share the ingestion
//! belt (`validate_semantic_hits`), which is what actually mattered to
//! share.

use norte_core::backend::Backend;
use norte_frontend::goto::SECCION_INDICE;
use norte_proto::Error;

use crate::app::App;
use crate::goto::MINIMO_PARA_EL_INDICE;
use crate::jobs::{GotoIndexRun, InFlight};

/// How many results are requested: the same number in both frontends.
const CAP: u32 = norte_frontend::goto::TOPE_DEL_INDICE;

/// Queries the index with whatever is typed right now, if it's worth it.
///
/// Relaunching ABORTS the previous request, the same cancellation contract
/// as the rest of the long requests: typing fast doesn't leave three
/// requests alive, and the answer to a query that is no longer typed is of
/// no use to anyone.
///
/// Below [`MINIMO_PARA_EL_INDICE`] no query is made AND the section is
/// CLEARED: leaving there what answered a longer query would be showing an
/// answer to a question that is no longer being asked.
pub fn pedir_al_indice(app: &mut App, backend: &Backend, work: &mut InFlight) {
    let Some(goto) = &mut app.goto else {
        olvidar(work);
        return;
    };
    let q = goto.query().to_owned();
    // A typed PATH doesn't count either: it's not a semantic query, and
    // sending it to an embeddings provider — maybe remote — is sending it
    // the name of a directory of the reader's.
    if q.chars().count() < MINIMO_PARA_EL_INDICE || norte_frontend::goto::parece_ruta(&q).is_some()
    {
        olvidar(work);
        goto.reemplazar_seccion(SECCION_INDICE, Vec::new(), true);
        return;
    }
    let b = backend.clone();
    let query = q.clone();
    // No root: against EVERYTHING indexed, like the `ai.search` semantic
    // search. "Go to anywhere" is literally that, and restricting it to the
    // focused pane would make the same query give different things
    // depending on where you were.
    let handle = tokio::spawn(async move { b.index_search_semantic(None, &query, CAP).await });
    if let Some(old) = work.goto_index.replace(GotoIndexRun { handle, query: q }) {
        old.handle.abort();
    }
}

/// Abandons the in-flight request, if there is one.
///
/// Called when closing the screen and when confirming a row: whatever comes
/// after wins, and a late answer no longer has anywhere to land.
pub fn olvidar(work: &mut InFlight) {
    if let Some(old) = work.goto_index.take() {
        old.handle.abort();
    }
}

/// Puts the index's answer into its section.
///
/// Three things it does NOT do, and each is a failure that has been seen in
/// this repository:
///
/// - It doesn't open anything or write to the status bar. It's a section of
///   a screen the reader is looking at; a message for every answer would
///   cover up what they're reading, and a disabled index isn't an error to
///   announce — `Unsupported` is the normal answer from whoever doesn't have
///   one.
/// - It doesn't trust the size of the answer: it goes through the SAME
///   `validate_semantic_hits` as the `ai.search` search, because a hostile
///   daemon can answer whatever it wants and the contractual cap belongs to
///   the client.
/// - It doesn't touch anything if the screen already closed, nor if what was
///   typed changed while the index was thinking: the answer is to ANOTHER
///   query, and setting it would be showing results for something the
///   reader no longer has typed.
pub fn harvest_goto_index(
    app: &mut App,
    work: &mut InFlight,
    res: Result<Result<Vec<norte_proto::methods::SemanticHit>, Error>, tokio::task::JoinError>,
) {
    let requested = work.goto_index.take().map(|r| r.query);
    let Ok(Ok(hits)) = res else {
        return;
    };
    let Some(requested) = requested else { return };
    let Some(goto) = &app.goto else { return };
    if goto.query() != requested {
        return;
    }
    let Some(hits) = norte_frontend::validate_semantic_hits(hits) else {
        return;
    };
    crate::goto::poner_indice(app, &hits);
}
