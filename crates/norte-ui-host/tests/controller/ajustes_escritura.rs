use super::*;

// ---------------------------------------------------------------------------
// Settings get WRITTEN from the window.
//
// Until now F11 was a showcase: it displayed the shared registry and said it
// did not write. The editing machine is the same as the terminal's
// (`norte_frontend::settings::SettingsState`), and what these tests pin down
// is the window's wiring around it: cycling with Enter, prompting for a
// value in a dialog, writing to the right layer and applying it live.
// ---------------------------------------------------------------------------

/// A registry entry's position in the FLAT list of settings.
///
/// It counts across ALL sections, in their order: since there are seven, the
/// index within a section is no longer the whole list's.
fn fila_de(a: &norte_ui_host::dto::SettingsView, id: &str) -> u32 {
    let mut flat = 0usize;
    for s in &a.sections {
        match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => {
                for r in rows {
                    if r.id == id {
                        return u32::try_from(flat).expect("fits");
                    }
                    flat += 1;
                }
            }
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => flat += rows.len(),
        }
    }
    panic!("entry {id} is in the registry")
}

/// The value the settings view shows for an entry.
fn valor_de(a: &norte_ui_host::dto::SettingsView, id: &str) -> String {
    a.sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => {
                rows.iter().find(|r| r.id == id).map(|r| r.value.clone())
            }
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .unwrap_or_else(|| panic!("entry {id} is in the view"))
}

/// Opens settings and puts the cursor on `id`.
async fn ajustes_sobre(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    id: &str,
) -> norte_ui_host::dto::SettingsView {
    h.dispatch(tecla("F11")).await.expect("host alive");
    let a = siguiente_ajustes(sub).await.expect("opens");
    let row = fila_de(&a, id);
    h.dispatch(UiAction::SettingsSelectRow { row })
        .await
        .expect("host alive");
    let a = siguiente_ajustes(sub).await.expect("still open");
    assert_eq!(a.cursor, u64::from(row), "the cursor is on {id}");
    a
}

/// What is in the user layer's `norte.toml`, if it already exists.
fn toml_de(root: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(root.join("norte.toml")).ok()
}

/// With the search box set, the cursor points to the row that IS VISIBLE.
///
/// This window's cursor used to be flat over `rows ++ paths`, and that only
/// worked because it did not filter: there was a `debug_assert` saying
/// exactly that. With a filter, the screen's third row is not the
/// registry's third one, and an Enter would activate something else.
#[tokio::test]
async fn with_a_filter_the_cursor_points_to_the_visible_row() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("opens");
    let total = a.total;
    assert_eq!(a.shown, total, "with no filter, all are seen");

    h.dispatch(UiAction::SettingsQuery {
        text: "show-hidden".to_owned(),
    })
    .await
    .expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("still open");
    assert!(
        a.shown < total,
        "the filter hides rows: {} of {total}",
        a.shown
    );
    assert_eq!(
        fila_de(&a, "ui.show-hidden"),
        0,
        "the row that is left is the list's first"
    );
    // And the index still lists the sections the filter emptied.
    assert!(
        a.index.iter().any(|s| s.visible == 0),
        "an empty section stays in the index: {:?}",
        a.index
    );
}

/// With a filter set, the dialog asks about the setting THAT IS VISIBLE.
///
/// The name used to come from `rows()[row]` with `row` counting VISIBLE
/// ones: filtering to "Open with", Enter on the first row opened the
/// editor's edit box and the dialog said "Theme". The reader thought they
/// were changing the theme and rewrote their command line.
#[tokio::test]
async fn with_a_filter_the_dialog_asks_about_the_right_setting() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    siguiente_ajustes(&mut sub).await.expect("opens");
    h.dispatch(UiAction::SettingsQuery {
        text: "@section:open-with".to_owned(),
    })
    .await
    .expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("still open");
    // The first visible row is `ui.editor`, a text one.
    assert_eq!(fila_de(&a, "ui.editor"), 0);
    h.dispatch(UiAction::SettingsSelectRow { row: 0 })
        .await
        .expect("host alive");
    siguiente_ajustes(&mut sub).await.expect("still open");
    h.dispatch(tecla("Enter")).await.expect("host alive");

    let body = foto_hasta(
        &h,
        &mut sub,
        "the dialog says which setting it is about",
        |s| {
            s.dialogs
                .first()
                .and_then(|d| d.body.first().map(|t| t.text.clone()))
        },
    )
    .await;
    let editor_name = norte_i18n::t_in(norte_i18n::Lang::Es, "setting-ui-editor-name");
    assert_eq!(
        body, editor_name,
        "the dialog has to name the setting that was activated"
    );
}

/// `tab` switches sides, and with the keyboard on the index the arrows move
/// through SECTIONS instead of rows — like help's side bar.
#[tokio::test]
async fn tab_moves_the_keyboard_to_the_index_and_arrows_switch_section() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("opens");
    assert_eq!(a.focus, "list", "the keyboard starts on the list");

    h.dispatch(tecla("Tab")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("still open");
    assert_eq!(a.focus, "index");

    // Down: the next section, and the cursor to its first row.
    h.dispatch(tecla("Down")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("still open");
    assert_eq!(
        a.cursor,
        u64::from(fila_de(&a, "ui.menu-bar")),
        "\"Panes and listing\" starts on its FIRST row, the menu bar"
    );

    // And back: the arrows move rows again.
    h.dispatch(tecla("Tab")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("still open");
    assert_eq!(a.focus, "list");
    let before = a.cursor;
    h.dispatch(tecla("Down")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("still open");
    assert_eq!(a.cursor, before + 1);
}

/// A click on the index takes the cursor to that section.
#[tokio::test]
async fn jumping_to_a_section_puts_the_cursor_on_its_first_row() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    siguiente_ajustes(&mut sub).await.expect("opens");

    h.dispatch(UiAction::SettingsJumpSection {
        section: "open-with".to_owned(),
    })
    .await
    .expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("still open");
    assert_eq!(
        a.cursor,
        u64::from(fila_de(&a, "ui.editor")),
        "\"Open with\" starts on the editor"
    );
}

/// Resetting removes the key, and if ANOTHER layer sets it, it says so: the
/// value does not go back to the factory one, and staying silent about it
/// would send the reader hunting for a bug.
#[tokio::test]
async fn resetting_with_another_layer_underneath_says_so() {
    let system = tempfile::tempdir().expect("temp");
    let user = tempfile::tempdir().expect("temp");
    // The system sets the theme; the user covers it with another.
    std::fs::write(system.path().join("norte.toml"), "[ui]\ntheme = \"nord\"\n")
        .expect("write system");
    std::fs::write(
        user.path().join("norte.toml"),
        "[ui]\ntheme = \"tokyonight\"\n",
    )
    .expect("write user");
    let (h, _snap) = host_con_capas_apiladas(system.path(), user.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.theme").await;
    let row = fila_de(&a, "ui.theme");

    h.dispatch(UiAction::SettingsReset { row })
        .await
        .expect("host alive");

    // The user's key goes away...
    let written = foto_hasta(&h, &mut sub, "theme removed from the user's", |_| {
        std::fs::read_to_string(user.path().join("norte.toml"))
            .ok()
            .filter(|s| !s.contains("tokyonight"))
    })
    .await;
    assert!(written.contains("[ui]"), "the section stays: {written}");
    // ...and the notice says another layer still sets it.
    let said = foto_hasta(&h, &mut sub, "it says so", |s| {
        s.status
            .message
            .clone()
            .filter(|m| m.contains("otra capa") || m.contains("another layer"))
    })
    .await;
    assert!(!said.is_empty());
}

/// Enter on a boolean cycles it, writes it to `norte.toml`, and the row shows
/// the new value without closing anything.
#[tokio::test]
async fn cycling_a_setting_with_enter_writes_it_and_refreshes_the_row() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;
    assert_eq!(valor_de(&a, "ui.show-hidden"), "false");

    h.dispatch(tecla("Enter")).await.expect("host alive");

    // The write comes back through `spawn_blocking`: the FILE is checked by
    // polling the actor, not by sleeping a fixed time.
    let written = foto_hasta(&h, &mut sub, "show_hidden written", |_| {
        toml_de(root.path()).filter(|s| s.contains("show_hidden = true"))
    })
    .await;
    assert!(written.contains("[ui]"), "in its section: {written}");
    let a = foto_hasta(&h, &mut sub, "the row shows the new value", |s| {
        s.settings
            .clone()
            .filter(|a| valor_de(a, "ui.show-hidden") == "true")
    })
    .await;
    assert_eq!(
        a.cursor,
        u64::from(fila_de(&a, "ui.show-hidden")),
        "the cursor does not move"
    );
}

/// A TEXT entry does not cycle: it asks for the value in a field's dialog,
/// prefilled with the current one, and confirming writes it.
#[tokio::test]
async fn a_text_setting_asks_for_the_value_in_a_dialog_and_confirming_writes_it() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    ajustes_sobre(&h, &mut sub, "ui.font").await;

    h.dispatch(tecla("Enter")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d.len(), 1, "one dialog, the value's");
    assert_eq!(d[0].title_key, "modal-setting-edit");
    assert_eq!(
        d[0].input.as_deref(),
        Some(""),
        "prefilled with the current value, which is empty"
    );

    h.dispatch(UiAction::DialogInput {
        id: d[0].id,
        text: "Fira Code".to_owned(),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d[0].id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "a valid value is accepted: {ack:?}"
    );

    let written = foto_hasta(&h, &mut sub, "font written", |_| {
        toml_de(root.path()).filter(|s| s.contains("font = \"Fira Code\""))
    })
    .await;
    assert!(written.contains("[ui]"), "in its section: {written}");
    foto_hasta(&h, &mut sub, "the row shows the new value", |s| {
        s.settings
            .clone()
            .filter(|a| valor_de(a, "ui.font") == "Fira Code")
    })
    .await;
}

/// An out-of-range integer is REJECTED with the reason, and touches no file.
#[tokio::test]
async fn an_out_of_range_integer_is_not_written_and_says_so() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    ajustes_sobre(&h, &mut sub, "ui.font-size").await;

    h.dispatch(tecla("Enter")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d[0].title_key, "modal-setting-edit");
    h.dispatch(UiAction::DialogInput {
        id: d[0].id,
        text: "99".to_owned(),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d[0].id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-settings-invalid-range".to_owned()
        },
        "it is rejected as such, not as applied"
    );
    asentar().await;
    assert!(
        toml_de(root.path()).is_none_or(|s| !s.contains("font_size")),
        "nothing was written: {:?}",
        toml_de(root.path())
    );
}

/// With no user layer there is nowhere to write, and it says so instead of
/// staying silent.
#[tokio::test]
async fn with_no_user_layer_it_does_not_write_and_says_so() {
    let (h, _snap) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;

    h.dispatch(tecla("Enter")).await.expect("host alive");
    assert_eq!(siguiente_aviso(&mut sub).await, "host-no-config-dir");
}

/// Cycling the theme from settings writes it AND applies it: the window
/// reloads its configuration through the same path as a profile change, so
/// the new theme reaches whoever hosts it without a restart.
#[tokio::test]
async fn cycling_the_theme_from_settings_applies_it_live() {
    use norte_ui_host::dto::NativeEffect;
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    let mut native = h.native_effects();
    let a = ajustes_sobre(&h, &mut sub, "ui.theme").await;
    let before = valor_de(&a, "ui.theme");

    h.dispatch(tecla("Enter")).await.expect("host alive");

    let effect = tokio::time::timeout(std::time::Duration::from_secs(5), native.recv())
        .await
        .expect("the theme notice comes out")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = effect else {
        panic!("the effect is the theme's: {effect:?}")
    };
    assert_ne!(name, before, "the next one in the list, not the same one");
    let written = foto_hasta(&h, &mut sub, "theme written", |_| {
        toml_de(root.path()).filter(|s| s.contains(&format!("theme = \"{name}\"")))
    })
    .await;
    assert!(written.contains("[ui]"), "in its section: {written}");
}

/// With a profile set FROM THE PICKER, the setting is written to the
/// profile, not to the user layer.
///
/// The review found it: the startup layers only carry the profile if it was
/// started with `--profile`, and `dir_de_escritura` only looked there.
/// Written to the user layer, the profile would cover it on re-read and the
/// bar would say "saved" over a value with no effect.
#[tokio::test]
async fn with_a_profile_set_the_setting_is_written_to_the_profile() {
    let root = tempfile::tempdir().expect("temp");
    let photos = root.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&photos).expect("mkdir");
    std::fs::write(
        photos.join("norte.toml"),
        "[profile]\ntitle = \"Fotos\"\n\n[ui]\nshow_hidden = false\n",
    )
    .expect("write");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "profile.pick").await;
    foto_hasta(&h, &mut sub, "the profile picker with its list", |s| {
        s.profiles
            .as_ref()
            .filter(|p| !p.rows.is_empty())
            .map(|_| ())
    })
    .await;
    h.dispatch(tecla("Enter")).await.expect("host alive");
    foto_hasta(&h, &mut sub, "the profile set", |s| {
        s.profiles.is_none().then_some(())
    })
    .await;

    ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;
    h.dispatch(tecla("Enter")).await.expect("host alive");

    let written = foto_hasta(&h, &mut sub, "show_hidden written to the profile", |_| {
        std::fs::read_to_string(photos.join("norte.toml"))
            .ok()
            .filter(|s| s.contains("show_hidden = true"))
    })
    .await;
    assert!(
        written.contains("title = \"Fotos\""),
        "the rest of the profile stays: {written}"
    );
    assert!(
        toml_de(root.path()).is_none_or(|s| !s.contains("show_hidden")),
        "and the user layer is untouched: {:?}",
        toml_de(root.path())
    );
    // And the row says so after re-reading: the profile no longer covers it.
    foto_hasta(&h, &mut sub, "the row shows the profile's value", |s| {
        s.settings
            .clone()
            .filter(|a| valor_de(a, "ui.show-hidden") == "true")
    })
    .await;
}

/// With the value prompt open, a double click writes nothing nor stacks
/// another prompt: the mouse honors the dialog just like the keyboard.
#[tokio::test]
async fn with_the_prompt_open_a_double_click_does_nothing() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.font").await;
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d.len(), 1);

    let ack = h
        .dispatch(UiAction::SettingsActivate {
            row: fila_de(&a, "ui.show-hidden"),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "with a dialog in front, the click is stale: {ack:?}"
    );
    asentar().await;
    assert!(
        toml_de(root.path()).is_none(),
        "nothing was written: {:?}",
        toml_de(root.path())
    );
    let snap = foto_hasta(&h, &mut sub, "a snapshot", |s| Some(s.dialogs.len())).await;
    assert_eq!(snap, 1, "still ONE dialog, the value's");
}

/// A double click on a row does what Enter does: cycle it.
#[tokio::test]
async fn a_double_click_activates_the_row_like_enter() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(root.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;

    h.dispatch(UiAction::SettingsActivate {
        row: fila_de(&a, "ui.show-hidden"),
    })
    .await
    .expect("host alive");

    foto_hasta(&h, &mut sub, "show_hidden written by the mouse", |_| {
        toml_de(root.path()).filter(|s| s.contains("show_hidden = true"))
    })
    .await;
}
