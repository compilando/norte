use super::*;

// ---------------------------------------------------------------------------
// The canonical corpus against the new surfaces.
// ---------------------------------------------------------------------------

/// No surface lets a terminal hazard through, and whichever one masks it
/// SAYS so.
///
/// A table over `norte-testkit`'s corpus, which was missing: this phase's
/// surfaces were written without any of it touching them, and every flag
/// that got computed and thrown away would have come from here. The property
/// is a PAIR: what is painted carries no hazard AND the mark is set.
/// Checking only the first is what lets a surface that silently masks
/// through.
///
/// THREE surfaces, and they are named because the earlier doc promised nine
/// and exercised two (#277): a FAVORITE's name (the user writes it in their
/// `norte.toml`), a VOLUME's label (the system gives it and it is bytes) and
/// an ATTRIBUTE's value (the name of the entry under the cursor). The
/// APPROVAL dialog has its own table, because its paths arrive from the
/// daemon already redacted and have to be passed through the same lossy step
/// first.
#[tokio::test]
// Long by TABLE, not by logic: each surface is a block with its own
// assertion and its own message, and splitting it would hide which ones are
// covered.
#[expect(
    clippy::too_many_lines,
    reason = "surface table: one assertion and its message per block"
)]
pub(super) async fn no_surface_masks_silently() {
    let corpus = norte_testkit::corpus::hostile_names();
    assert!(
        corpus.len() >= 48,
        "the canonical corpus stands at: {}",
        corpus.len()
    );

    // The ones that REALLY alter the screen. A long name or one in NFD does
    // not get masked — nor should it — so requiring a mark for it would be
    // requiring a lie.
    let alter: Vec<&norte_testkit::corpus::HostileName> = corpus
        .iter()
        .filter(|n| norte_frontend::display_name(&n.bytes).1)
        .collect();
    assert!(
        alter.len() >= 8,
        "the corpus brings real hazards: {}",
        alter.len()
    );

    for n in alter {
        // A favorite's name lives in a `String` from `norte.toml`, so it can
        // only carry what is valid UTF-8. Converting the rest with
        // `from_utf8_lossy` would be doing here the conversion the host has
        // to mark, and the test would say the host does not mark it when it
        // was the test that did it: it is the double-lossy trap, which the
        // flag can no longer recover from because U+FFFD is not a hazard.
        let text = match std::str::from_utf8(&n.bytes) {
            Ok(t) => t.to_owned(),
            Err(_) => String::new(),
        };

        // 1. A FAVORITE's name: the user writes it, and the project layer
        //    is "I have opened this repo", not "I vouch for this string".
        let mut cfg = test_settings();
        cfg.common.hotlist = vec![norte_config::HotlistItem {
            name: text.clone(),
            target: norte_proto::VPath::parse("mem:///casa").map_err(|_| "err".to_owned()),
        }];
        let mut f = Fake::default();
        // The entry under the cursor is the hostile one: its name is the
        // ATTRIBUTES SHEET's first field, which is the third surface.
        f.put("mem:///casa", vec![(n.bytes.clone(), false)]);
        // 2. A VOLUME's label: the system gives it and it is bytes.
        f.volumes = vec![norte_proto::methods::Volume {
            label: Some(n.bytes.clone()),
            ..volume("mem:///casa", "ext4", false)
        }];
        let (h, snap) = UiHost::start(UiHostOptions {
            backend: Arc::new(f),
            initial_dir: dir(),
            initial_dir_requested: false,
            attach: false,
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
            layout: norte_frontend::layout::presets::tree("full").expect("layout"),
            viewport: (200, 60),
            settings: cfg,
            paths: norte_ui_host::settings::HostPaths::default(),
            theme: norte_ui_host::pickers::HostTheme::default(),
            user_layouts: Vec::new(),
            profile: None,
            columns: norte_ui_host::default_columns(),
            effects: norte_ui_host::commands::Effects::Full,
            log_ring: None,
        })
        .await
        .expect("starts");
        let mut sub = h.subscribe();

        let bar = places(&snap).expect("`full` places the bar").clone();
        if !text.is_empty() {
            let favorite = bar
                .rows
                .iter()
                .find_map(|r| match r {
                    norte_ui_host::dto::PlaceRowView::Favorite { name, hostile, .. } => {
                        Some((name.clone(), *hostile))
                    }
                    _ => None,
                })
                .expect("the favorite is there");
            sin_peligro(&favorite.0, &n.id, "a favorite's name");
            assert!(
                favorite.1,
                "[{}] the favorite is masked and does NOT say so: {:?}",
                n.id, favorite.0
            );
        }

        // The side bar, once the volumes arrive.
        for _ in 0..20 {
            h.dispatch(UiAction::Resync).await.expect("host alive");
            let snap = next_snapshot(&mut sub).await;
            let v = places(&snap).expect("placed");
            let drive = v.rows.iter().find_map(|r| match r {
                norte_ui_host::dto::PlaceRowView::Drive { label, hostile, .. } => {
                    Some((label.clone(), *hostile))
                }
                _ => None,
            });
            if let Some((label, hostile)) = drive {
                sin_peligro(&label, &n.id, "a volume's label in the bar");
                assert!(
                    hostile,
                    "[{}] the volume's label is masked and does NOT say so: {label:?}",
                    n.id
                );
                break;
            }
        }

        // 3. An ATTRIBUTE's value: the sheet's first field is the name of
        //    the entry under the cursor, i.e. bytes from the provider
        //    (#277). This test's doc named it from the start and nobody
        //    exercised it.
        let snap = wait_snapshot(&h, &mut sub, "the sheet has the name", |f| {
            sheet(f).is_some_and(|m| !m.fields.is_empty())
        })
        .await;
        let field = sheet(&snap)
            .expect("the `full` layout places the sheet")
            .fields
            .first()
            .expect("the first field is the name")
            .clone();
        sin_peligro(&field.value, &n.id, "an attribute's value");
        assert!(
            field.hostile,
            "[{}] the attribute's value is masked and does NOT say so: {:?}",
            n.id, field.value
        );
    }
}

/// No paintable string carries a terminal hazard.
pub(super) fn sin_peligro(painted: &str, id: &str, where_: &str) {
    for c in painted.chars() {
        assert!(
            !norte_encoding::is_terminal_hazard(c),
            "[{id}] {where_} carries {c:?} unmasked: {painted:?}"
        );
    }
}
