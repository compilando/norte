//! The three long-running tasks a pane shows while they run: search, compare
//! and sync.
//!
//! All three have the same shape — they get launched, results arrive over a
//! channel that gets drained, and while they are alive their pane eats the
//! keyboard with a key table of its own — and all three used to live in the
//! `ntc` binary's root, a crate DIFFERENT from this lib.
//!
//! There is a cycle between this module and [`crate::navigate`] — `cd` has to
//! drop a live search when leaving the virtual pane, and launching a search
//! needs the `cd`. A cycle between modules of the SAME crate is legal in
//! Rust, so the exit order doesn't matter; what could not be done was leaving
//! half of it in the binary, which IS another crate.
//!
//! The three `*Run` types are the handle: the cancelable Task (rule 3), the
//! channel, and the generation that lets a batch arriving late be discarded
//! instead of getting mixed with the next plan.
//!
//! One file per domain and a pure-facade `mod.rs`, which is the pattern
//! `norte-frontend/src/layout/` already demonstrates in this repo: no
//! production file over a thousand lines.

mod ai;
mod compare;
mod diskmap;
// Public, unlike the others: its two verbs — request and forget — are called
// by the key handler on every keystroke, and reading them as
// `jobs::goto::ask_the_index` says which screen they are for.
pub mod goto;
mod inflight;
mod search;
mod sync;

pub use ai::{
    harvest_ai_rename, harvest_checksum, harvest_organize, harvest_rename_batch, harvest_semantic,
    spawn_organize_plan, spawn_renamer_plan,
};
pub use compare::{
    COMPARE_PAGE_STEP, CompareKey, CompareRun, compare_key, drain_compare, launch_compare,
    on_compare_enter, on_compare_key,
};
pub use diskmap::{harvest as harvest_disk_map, launch as launch_disk_map};
pub use goto::harvest_goto_index;
pub use inflight::{
    AiRenameRun, ChecksumRun, DiskMapRun, GotoIndexRun, InFlight, OrganizeRun, PendingAiPlan,
    Published, RenameBatchRun, SemanticRun,
};
pub use search::{
    SEARCH_MAX_HITS, SearchRun, drain_search, finalize_search_state, launch_search,
    on_search_dialog_key, on_search_enter, on_search_escape, search_params,
};
pub use sync::{
    SyncKey, SyncRun, SyncTick, approve_sync, drain_sync_plan, harvest_sync_apply,
    launch_sync_apply, launch_sync_plan, on_sync_key, submit_sync, sync_key,
};
