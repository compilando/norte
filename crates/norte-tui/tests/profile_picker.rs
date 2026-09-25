//! The profile picker seen from `App`: what it asks for on confirm and
//! what it does not, and that its box paints without overflowing.

use std::ffi::OsString;

use norte_frontend::profile_picker::UserProfile;
use norte_proto::VPath;
use norte_tui::app::{App, Pane, PickerAction};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn test_app() -> App {
    App::new(
        Pane::new(vp("file:///left"), Vec::new()),
        Pane::new(vp("file:///right"), Vec::new()),
    )
}

fn profile(name: &str) -> UserProfile {
    UserProfile {
        name: OsString::from(name),
        title: None,
        problem: None,
    }
}

/// **Save as profile opens PRE-FILLED with the active profile** (#306).
///
/// The normal thing is to start from the one you have set, so "save as"
/// over the same name is saving over it — which is what any program does.
/// With no profile the field is born empty: there is no default name that
/// is not a fabrication.
#[test]
fn save_as_profile_starts_from_the_active_one() {
    let mut app = test_app();
    app.open_profile_save_as();
    assert!(
        matches!(&app.modal, Some(norte_tui::app::Modal::ProfileSaveAs { name, .. }) if name.is_empty()),
        "no profile, empty: {:?}",
        app.modal
    );

    app.modal = None;
    app.active_profile = Some(OsString::from("photos"));
    app.open_profile_save_as();
    assert!(
        matches!(&app.modal, Some(norte_tui::app::Modal::ProfileSaveAs { name, .. }) if name == "photos"),
        "with a profile, its own: {:?}",
        app.modal
    );
}

/// And a name that cannot be a directory leaves the modal open with its
/// diagnostic: what was typed survives so it can be fixed, which is the
/// discipline of this screen's prompts.
#[tokio::test]
async fn an_invalid_profile_name_does_not_close_the_modal() {
    let mut app = test_app();
    app.open_profile_save_as();
    let Some(norte_tui::app::Modal::ProfileSaveAs { name, .. }) = &mut app.modal else {
        panic!("the modal is open");
    };
    name.push_str("../other");

    norte_tui::screens::profile_save_as(&mut app).await;

    assert!(
        matches!(
            &app.modal,
            Some(norte_tui::app::Modal::ProfileSaveAs { name, error: Some(_) }) if name == "../other"
        ),
        "still open, with the name and the reason: {:?}",
        app.modal
    );
}

/// Confirming leaves the REQUESTED change and closes the picker. It does
/// not do it here: the change reloads config, and doing that from a key
/// handler is rule 2 again.
#[test]
fn confirming_requests_the_change_and_closes() {
    let mut app = test_app();
    app.open_profile_picker(vec![profile("work"), profile("photos")]);
    app.profile_picker_input(PickerAction::Down);
    app.profile_picker_input(PickerAction::Confirm);

    assert!(app.profile_picker.is_none(), "the picker closes");
    assert_eq!(
        app.pending_profile.as_deref(),
        Some(std::ffi::OsStr::new("photos"))
    );
}

/// Choosing the profile that is ALREADY active requests nothing: a change
/// that changes nothing would tear down and reload the screen just to
/// leave it the same.
#[test]
fn confirming_the_active_one_requests_nothing() {
    let mut app = test_app();
    app.active_profile = Some(OsString::from("work"));
    app.open_profile_picker(vec![profile("work")]);
    app.profile_picker_input(PickerAction::Confirm);

    assert!(app.profile_picker.is_none());
    assert_eq!(app.pending_profile, None);
}

/// Cancelling closes and requests nothing.
#[test]
fn cancelling_requests_nothing() {
    let mut app = test_app();
    app.open_profile_picker(vec![profile("work")]);
    app.profile_picker_input(PickerAction::Cancel);
    assert!(app.profile_picker.is_none());
    assert_eq!(app.pending_profile, None);
}

/// The box paints without overflowing in a small terminal, with an empty
/// list and with rows that need a note. An empty list is not an error: it
/// just means you have not created one yet.
#[test]
fn it_paints_without_overflowing() {
    for profiles in [
        Vec::new(),
        vec![profile("work")],
        vec![
            profile("orthodox"),
            UserProfile {
                name: OsString::from("broken"),
                title: Some("A genuinely long title".to_owned()),
                problem: Some("line 3: unknown field `them`".to_owned()),
            },
        ],
    ] {
        for (w, h) in [(24_u16, 6_u16), (80, 24), (200, 60)] {
            let mut app = test_app();
            app.open_profile_picker(profiles.clone());
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut term = ratatui::Terminal::new(backend).expect("terminal");
            term.draw(|f| norte_tui::ui::draw(f, &app)).expect("paint");
        }
    }
}
