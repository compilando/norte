//! Every integration test of norte-core, in ONE binary.
//!
//! Each file used to be its own test binary: 73 of them, each compiling and
//! linking the whole core plus wasmtime — most of the gate's test-build CPU
//! and ~35 MB of disk apiece per build. The files stay where they were
//! (`autotests = false` in `Cargo.toml`), so their relative `include_*!`
//! paths and nested modules resolve as before. nextest still runs each test
//! in its own process, so no test shares state it did not share before.
//!
//! A new test file is a new line here: without it, it is never compiled.

#[path = "ai_chat.rs"]
mod ai_chat;
#[path = "ai_rename.rs"]
mod ai_rename;
#[path = "backend_embedded.rs"]
mod backend_embedded;
#[path = "backend_embedded_anchor.rs"]
mod backend_embedded_anchor;
#[path = "backend_parity.rs"]
mod backend_parity;
#[path = "backend_remote.rs"]
mod backend_remote;
#[path = "caps_gating.rs"]
mod caps_gating;
#[path = "columns_age_e2e.rs"]
mod columns_age_e2e;
#[path = "columns_git_e2e.rs"]
mod columns_git_e2e;
#[path = "columns_media_e2e.rs"]
mod columns_media_e2e;
#[path = "columns_size_bar_e2e.rs"]
mod columns_size_bar_e2e;
#[path = "connect.rs"]
mod connect;
#[path = "connect_real.rs"]
mod connect_real;
#[path = "daemon/main.rs"]
mod daemon;
#[path = "decorator_icons_e2e.rs"]
mod decorator_icons_e2e;
#[path = "e2e_exit_criterion.rs"]
mod e2e_exit_criterion;
#[path = "e2e_rename_batch.rs"]
mod e2e_rename_batch;
#[path = "e2e_semantic.rs"]
mod e2e_semantic;
#[path = "embedded_journal.rs"]
mod embedded_journal;
#[path = "engine.rs"]
mod engine;
#[path = "engine_archive.rs"]
mod engine_archive;
#[path = "engine_case_only_rename.rs"]
mod engine_case_only_rename;
#[path = "engine_checksum.rs"]
mod engine_checksum;
#[path = "engine_close_connection.rs"]
mod engine_close_connection;
#[path = "engine_compare.rs"]
mod engine_compare;
#[path = "engine_copy_in_flight.rs"]
mod engine_copy_in_flight;
#[path = "engine_dest_anchor.rs"]
mod engine_dest_anchor;
#[path = "engine_dir_size.rs"]
mod engine_dir_size;
#[path = "engine_dir_usage.rs"]
mod engine_dir_usage;
#[path = "engine_gc.rs"]
mod engine_gc;
#[path = "engine_journal.rs"]
mod engine_journal;
#[path = "engine_leaf_confined.rs"]
mod engine_leaf_confined;
#[path = "engine_local_kernel_copy.rs"]
mod engine_local_kernel_copy;
#[path = "engine_local_lazy_list.rs"]
mod engine_local_lazy_list;
#[path = "engine_m2_phase1.rs"]
mod engine_m2_phase1;
#[path = "engine_pack.rs"]
mod engine_pack;
#[path = "engine_resume.rs"]
mod engine_resume;
#[path = "engine_search.rs"]
mod engine_search;
#[path = "engine_set_mode.rs"]
mod engine_set_mode;
#[path = "engine_sync_apply.rs"]
mod engine_sync_apply;
#[path = "engine_sync_confined.rs"]
mod engine_sync_confined;
#[path = "engine_sync_plan.rs"]
mod engine_sync_plan;
#[path = "engine_sync_vanishing_destination.rs"]
mod engine_sync_vanishing_destination;
#[path = "engine_undo_of_failed_copy.rs"]
mod engine_undo_of_failed_copy;
#[path = "engine_vanishing_destination.rs"]
mod engine_vanishing_destination;
#[path = "ftp_provider_contract.rs"]
mod ftp_provider_contract;
#[path = "hooks_rename_log_e2e.rs"]
mod hooks_rename_log_e2e;
#[path = "index_e2e.rs"]
mod index_e2e;
#[path = "index_embed.rs"]
mod index_embed;
#[path = "organizer_by_extension_e2e.rs"]
mod organizer_by_extension_e2e;
#[path = "plugin_config_backend_e2e.rs"]
mod plugin_config_backend_e2e;
#[path = "plugin_install.rs"]
mod plugin_install;
#[path = "plugin_provider_e2e.rs"]
mod plugin_provider_e2e;
#[path = "plugin_provider_installed_e2e.rs"]
mod plugin_provider_installed_e2e;
#[path = "plugin_provider_rw_e2e.rs"]
mod plugin_provider_rw_e2e;
#[path = "plugin_template_e2e.rs"]
mod plugin_template_e2e;
#[path = "plugin_timeout_e2e.rs"]
mod plugin_timeout_e2e;
#[path = "plugins_config_e2e.rs"]
mod plugins_config_e2e;
#[path = "plugins_decorate_columns_e2e.rs"]
mod plugins_decorate_columns_e2e;
#[path = "plugins_preview_e2e.rs"]
mod plugins_preview_e2e;
#[path = "plugins_preview_image_e2e.rs"]
mod plugins_preview_image_e2e;
#[path = "plugins_preview_markdown_e2e.rs"]
mod plugins_preview_markdown_e2e;
#[path = "plugins_preview_syntect_e2e.rs"]
mod plugins_preview_syntect_e2e;
#[path = "plugins_run_e2e.rs"]
mod plugins_run_e2e;
#[path = "policy.rs"]
mod policy;
#[path = "rename_batch.rs"]
mod rename_batch;
#[path = "rename_case_only.rs"]
mod rename_case_only;
#[path = "renamer_date_prefix_e2e.rs"]
mod renamer_date_prefix_e2e;
#[path = "rpc_catalogue.rs"]
mod rpc_catalogue;
#[path = "scheduler.rs"]
mod scheduler;
#[path = "spans_in_spawn.rs"]
mod spans_in_spawn;
#[path = "thumbnail_image_e2e.rs"]
mod thumbnail_image_e2e;
#[path = "undo.rs"]
mod undo;

/// Shared by the two `*_vanishing_destination` suites: declared ONCE here,
/// because a module file loaded twice is two copies of every type in it.
#[cfg(unix)]
#[path = "origin_on_request/mod.rs"]
mod origin_on_request;

/// A file in `tests/` with no line above is never compiled, and its tests
/// pass by not existing.
#[test]
fn every_test_file_is_in_this_binary() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let me = include_str!("it.rs");
    for entry in std::fs::read_dir(&dir).expect("tests/") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let wanted = if path.is_dir() && path.join("main.rs").exists() {
            format!("#[path = \"{name}/main.rs\"]")
        } else if path.extension().is_some_and(|e| e == "rs") && name != "it.rs" {
            format!("#[path = \"{name}\"]")
        } else {
            continue;
        };
        assert!(me.contains(&wanted), "tests/{name} is not in tests/it.rs");
    }
}
