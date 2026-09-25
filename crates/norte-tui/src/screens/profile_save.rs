//! Save the current workspace as a PROFILE (#306, ADR 0079).
//!
//! This is the other half of profiles: up to here you could pick one and
//! switch on the fly, but only ones somebody had written by hand existed.
//!
//! What gets saved is what you SEE, and that phrase decides the content:
//!
//! - the live layout, in `profiles/<name>/layouts/workspace.toml`, with the
//!   profile's `[ui] layout` pointing at it;
//! - each slot's directory in `[profile.start]`, which is what makes a
//!   profile useful on its FIRST launch, before it has saved state;
//! - the `[ui]` scalars that DIFFER from the user layer. Only those: copying
//!   the ones that match fills the file with lines nobody can later tell
//!   apart from noise;
//! - the starting profile's `keymap.toml`, if there was one. "Save as"
//!   produces a profile that behaves the same as the one you had: if you
//!   changed shortcuts, the new one carries them.
//!
//! What is NOT saved: favorites, connections, and the rest of the sections
//! that are not `[ui]`. A profile is a WORK space, not a copy of the entire
//! configuration, and duplicating the user's hotlist into every profile would
//! freeze it — the user's still shows through underneath.

use norte_i18n::{t, ta};

use crate::app::{App, Modal};

/// Enter in the "save as profile" modal.
///
/// Validates the name BEFORE touching disk — it ends up as a directory — and
/// leaves the modal open with the diagnosis if it is not valid: what was
/// typed survives so it can be corrected, which is the discipline of the ten
/// prompts.
///
/// The disk work goes through `spawn_blocking` (rule 2): it is three files
/// with a lock and tmp+rename.
pub async fn profile_save_as(app: &mut App) {
    let Some(Modal::ProfileSaveAs { name, .. }) = &app.modal else {
        return;
    };
    let name_os = std::ffi::OsString::from(name.trim());
    if !norte_config::valid_profile_name(&name_os) {
        app.prompt_error_profile_save(t("msg-profile-name-invalid"));
        return;
    }
    let Some(dir) = norte_config::profiles_dir_from(&|k| std::env::var_os(k)) else {
        app.prompt_error_profile_save(t("msg-no-config-dir"));
        return;
    };
    let snap = snapshot_of(app);
    let n = name_os.clone();
    let res =
        tokio::task::spawn_blocking(move || norte_config::save_profile(&dir, &n, &snap)).await;
    match res {
        Ok(Ok(_)) => {
            app.prompt_submitted_profile_save();
            app.message = Some(ta(
                "msg-profile-saved",
                &[(
                    "name",
                    &crate::app::detail_for_bar(&name_os.to_string_lossy()),
                )],
            ));
        }
        Ok(Err(e)) => {
            app.prompt_error_profile_save(ta(
                "msg-profile-save-failed",
                &[("error", &crate::app::io_error_category(&e))],
            ));
        }
        // A panic while writing is OUR bug: let it blow up visibly, as with
        // the rest of the binary's `persist_*` functions.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// What is on screen, in the shape `norte-config` writes.
///
/// The CONTENT is decided by `norte_frontend::config::profile_snapshot`,
/// which is the same one the window calls (#318): here we only answer where
/// each listing is. See its rustdoc for why there are not two copies of
/// this — it is the lesson of ADR 0077, and this would be the worst place to
/// forget it.
fn snapshot_of(app: &App) -> norte_config::ProfileSnapshot {
    norte_frontend::config::profile_snapshot(
        &app.layout,
        &|id| app.panes.browser(id).map(|p| p.dir().clone()),
        active_profile_keymap(app),
    )
}

/// The ACTIVE profile's `keymap.toml`, verbatim, or `None` if there is no
/// profile or it does not have one.
///
/// Byte for byte and without rewriting it: it is the reader's file, with
/// their comments.
fn active_profile_keymap(app: &App) -> Option<Vec<u8>> {
    let dir = app.config_write_dir()?;
    std::fs::read(dir.join("keymap.toml")).ok()
}
