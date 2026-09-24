use super::*;

// ---------------------------------------------------------------------------
// Renaming ONE entry (task 5.2).
// ---------------------------------------------------------------------------

/// `shift+F6` opens the EDITABLE name, seeded with what the row paints.
#[tokio::test]
async fn renaming_opens_the_name_for_editing() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // The cursor, on `notes.txt`.
    let b = listing(&snap);
    let notes = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("it is there");
    let (key, generation) = (notes.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");

    h.dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    let d = next_dialogs(&mut sub).await[0].clone();
    assert_eq!(d.title_key, "modal-rename-title");
    assert_eq!(
        d.input.as_deref(),
        Some("notas.txt"),
        "the field is born with the current name"
    );
    assert!(
        d.destination.is_none(),
        "a rename does not go anywhere else"
    );
    assert!(
        backend.transfers.lock().expect("transferencias").is_empty(),
        "opening the dialog does not rename anything"
    );
}

/// Without touching the field NOTHING is renamed, and that is the
/// protection.
///
/// It is rule 1 in the seam: the field is seeded with what the row PAINTS,
/// and for a name that is not UTF-8 that carries a U+FFFD. Without touching
/// it, the ORIGINAL bytes are rebuilt — which are the current ones, that is
/// "same name, same place" — so the seed can never turn into the operand.
/// Sending the text as-is would write real mojibake.
#[tokio::test]
async fn an_untouched_name_renames_nothing() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listing(&snap);
    let hostile = b.rows.iter().find(|r| r.hostile).expect("there is one");
    let (key, generation) = (hostile.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    let d = next_dialogs(&mut sub).await[0].clone();
    assert!(
        d.input_hostile,
        "the field says what was seeded is not faithful"
    );

    // It confirms WITHOUT typing anything. There is no possible rename — the
    // destination would be the same — and that is SAID in the ack, not just
    // in the bar.
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d.id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-transfer-name-same".to_owned()
        },
        "{ack:?}"
    );
    asentar().await;
    assert!(
        backend.transfers.lock().expect("transferencias").is_empty(),
        "the same name in the same place is not an operation"
    );
}

/// A TOUCHED name that still carries the replacement character is REJECTED:
/// confirming it would write the mojibake the screen invented.
#[tokio::test]
async fn a_touched_name_with_fffd_is_rejected() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listing(&snap);
    let hostile = b.rows.iter().find(|r| r.hostile).expect("there is one");
    let (key, generation, painted) = (hostile.key, b.generation, hostile.display_name.clone());
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;

    // It gets edited: the renderer returns what was there PLUS one letter,
    // and what was there carries the U+FFFD the screen put in.
    h.dispatch(UiAction::DialogInput {
        id,
        text: format!("{painted}x"),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    asentar().await;
    assert!(
        backend.transfers.lock().expect("transferencias").is_empty(),
        "a name the screen made up is not written"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(
        snapshot
            .status
            .message
            .is_some_and(|m| !m.starts_with("msg-")),
        "and it is said, translated"
    );
}

/// A new name goes out as an `fs.move` inside the SAME directory.
#[tokio::test]
async fn a_new_name_goes_out_as_a_move_to_the_same_place() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listing(&snap);
    let notes = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("it is there");
    let (key, generation) = (notes.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "apuntes.md".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = anotados(&backend, "the rename queued", 1, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 1);
    let (from, to, mover, collision) = &ts[0];
    assert!(mover, "renaming is moving");
    assert_eq!(from.to_wire(), "mem:///casa/notas.txt");
    assert_eq!(
        to.to_wire(),
        "mem:///casa/apuntes.md",
        "to the SAME directory"
    );
    assert_eq!(*collision, norte_proto::CollisionPolicy::Fail);
}

/// With SEVERAL marks, this window refuses: nobody says which one to
/// rename. It is the asymmetry `Facts::rename_single` documents, and this
/// host already declared it in `facts()`.
#[tokio::test]
async fn renaming_with_several_marks_is_refused() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let b = listing(&snap);
    let generation = b.generation;
    for r in b.rows.iter().take(2) {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: r.key,
            generation,
        })
        .await
        .expect("host alive");
    }
    let ack = h
        .dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "reason-wrong-target".to_owned()
        },
        "{ack:?}"
    );
}

/// And in read-only, it does not even open.
#[tokio::test]
async fn in_read_only_renaming_does_not_open_anything() {
    let backend = fake_tree();
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    asentar().await;
    assert!(backend.transfers.lock().expect("transferencias").is_empty());
}

// ---------------------------------------------------------------------------
// The rename plan a model proposes (task 5.2).
// ---------------------------------------------------------------------------

pub(super) fn test_hash() -> norte_proto::methods::PlanHash {
    norte_proto::methods::PlanHash::parse(&"a".repeat(norte_proto::methods::PLAN_HASH_LEN))
        .expect("valid hex")
}

/// A verdict from the core: applicable, with `n` real steps.
pub(super) fn verdict_ok(pares: &[(&str, &str)]) -> norte_proto::methods::FsRenameBatchPlanResult {
    norte_proto::methods::FsRenameBatchPlanResult {
        steps: pares
            .iter()
            .map(|(f, t)| norte_proto::methods::RenameStep {
                from: norte_proto::Segment::new(f.as_bytes().to_vec()).expect("seg"),
                to: norte_proto::Segment::new(t.as_bytes().to_vec()).expect("seg"),
                temp: false,
            })
            .collect(),
        collisions: Vec::new(),
        executable: true,
        plan_hash: test_hash(),
    }
}

/// A backend with an AI plan and its verdict.
pub(super) fn fake_with_plan(
    pares: &[(&str, &str)],
    verdict: Option<norte_proto::methods::FsRenameBatchPlanResult>,
) -> Arc<Fake> {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        pares
            .iter()
            .map(|(from, _)| (from.as_bytes().to_vec(), false))
            .collect::<Vec<_>>(),
    );
    f.plan_ia = Some(
        pares
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    );
    f.verdict = verdict;
    Arc::new(f)
}

/// Waits for the next update carrying the plan's review.
pub(super) async fn next_revision(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::AiRenameView> {
    for _ in 0..40 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update, not a hang")
            .expect("the host is still alive");
        match next {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::AiRename { ai_rename } = c {
                            return ai_rename.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload
                    && s.ai_rename.is_some()
                {
                    return s.ai_rename.clone();
                }
            }
            Update::Lagged => panic!("no lag in this test"),
        }
    }
    panic!("the review never arrived");
}

/// Asks for a plan: opens the instruction prompt and answers it.
///
/// No factory preset binds `pane.ai-rename`, so it arrives through the
/// palette, the catalogue's other door.
pub(super) async fn request_plan(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    by_palette(h, sub, "ai-rename").await;
    let id = next_dialogs(sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "number the episodes".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
}

/// #310: batch renaming by TEMPLATE in the window. The prompt fixes the
/// operand (what is marked), the template is validated with the human
/// present and the prompt comes back with what was typed and the diagnosis
/// in the bar, and the plan — deterministic, no model — enters through the
/// SAME review as the AI's, with the core's verdict along for the ride.
#[tokio::test]
async fn a_template_batch_is_reviewed_like_the_ais() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "ep02.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // The batch acts on what is MARKED: both of them.
    by_palette(&h, &mut sub, "mark.all").await;
    by_palette(&h, &mut sub, "rename-batch").await;
    let prompt = snapshot_until(&h, &mut sub, "the template prompt", |s| {
        s.dialogs
            .iter()
            .find(|d| d.title_key == "modal-rename-batch")
            .cloned()
    })
    .await;
    assert_eq!(
        prompt.input.as_deref(),
        Some("[N].[E]"),
        "prefilled with the identity, like the TUI"
    );

    // A template that would leave a `/` inside is explained and the prompt
    // COMES BACK with what was typed, instead of discarding it.
    h.dispatch(UiAction::DialogInput {
        id: prompt.id,
        text: "a/[N]".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: prompt.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let reabierto = snapshot_until(
        &h,
        &mut sub,
        "the prompt reopened with the diagnosis",
        |s| {
            s.dialogs
                .iter()
                .find(|d| d.title_key == "modal-rename-batch" && d.id != prompt.id)
                .cloned()
                .filter(|_| s.status.message.as_deref().is_some_and(|m| m.contains('/')))
        },
    )
    .await;
    assert_eq!(reabierto.input.as_deref(), Some("a/[N]"));
    assert!(
        backend.instrucciones.lock().expect("mutex").is_empty(),
        "the model was not asked for anything"
    );

    // The good one: the plan is generated here and reviewed like the AI's.
    h.dispatch(UiAction::DialogInput {
        id: reabierto.id,
        text: "ep0[C].[E]".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: reabierto.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let mut v = next_revision(&mut sub).await.expect("the review opens");
    assert_eq!(v.total, 2, "{v:?}");
    assert_eq!(v.pairs[0].from.text, "ep1.mkv");
    assert_eq!(v.pairs[0].to.text, "ep01.mkv");
    assert_eq!(v.pairs[1].to.text, "ep02.mkv");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }
    assert!(
        v.confirmable,
        "the core gave its verdict on the template's plan"
    );
    assert!(
        backend.instrucciones.lock().expect("mutex").is_empty(),
        "still no model involved"
    );
    assert_eq!(
        backend.verdicts_pedidos.lock().expect("mutex").len(),
        1,
        "one verdict requested, for the template's plan"
    );
    assert!(
        backend.batches.lock().expect("lotes").is_empty(),
        "reviewing applies nothing"
    );
}

/// Over a location that refuses to write, the window DIMS the delete.
///
/// `source_read_only` and `dest_read_only` were wired to `false` with a
/// comment declaring the host does not keep that count. It can keep it: it
/// ASKS each slot's `capabilities` on landing — it has done so since #268 —
/// and was throwing away everything but the fold mode, with the `READ_ONLY`
/// flag one field away. The terminal does look at it (`App::pane_read_only`),
/// so inside a zip the terminal dimmed F5/F8 and the window offered them lit
/// up: help was inviting impossible writes.
///
/// The EXECUTABLE rows of the corpus page are the ones checked, since that
/// is where these facts arrive. The keyboard sheet is no good for this and
/// it pays to not confuse them: its `avail` is a BUILD fact — "this frontend
/// implements the command" — and does not change with where the reader is.
///
/// The test uses the FLAG and not the scheme on purpose. `scheme_is_read_only`
/// answers yes for a `zip+file://` without asking anyone, so a test built
/// over a container would pass with the syntactic fallback in place and the
/// capabilities check thrown away regardless. A `mem:///` that announces
/// `READ_ONLY` — a read-only SFTP export, an `ro` mount — is only known
/// through the flag.
#[tokio::test]
async fn a_location_that_refuses_to_write_dims_the_delete() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    fake.capabilities.insert(
        "mem:///casa".to_owned(),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        },
    );
    let (h, _snap) = host_en(Arc::new(fake), "mem:///casa").await;
    let mut sub = h.subscribe();
    // Capabilities are requested when the listing lands and come back on
    // their own: help freezes its facts WHEN OPENED, so opening it before
    // they arrive would freeze the usual "unknown" forever.
    asentar().await;

    // F8 deletes in the ACTIVE slot, which is the source: it is the key
    // that asks about `source_read_only` and only about it.
    let page = copy_page(&h, &mut sub).await;
    let row = action(&page, "F8");
    assert!(
        !row.enabled,
        "deleting is not possible here and the page offers it turned off: {row:?}"
    );
    assert_eq!(
        row.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "reason-read-only"),
        "and it says WHY, not just that it can't: {row:?}"
    );
}

/// And a re-listing underneath does NOT bring help back to "unknown".
///
/// Capabilities were being ERASED when requested, and landing re-freezes
/// help's facts three lines later: meaning every re-freeze coming from a
/// listing read `None` — always, not sometimes — and the row would light up
/// again. With help open, it is enough for a task to finish or for the
/// watcher to fire for F8 to go from dimmed to lit without the location
/// having changed.
///
/// Now the answer is TIED to its path and is not discarded when another is
/// requested: only a directory change invalidates it, which is the only
/// thing that truly invalidates it.
#[tokio::test]
async fn a_relisting_does_not_relight_what_the_location_still_refuses() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    fake.capabilities.insert(
        "mem:///casa".to_owned(),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        },
    );
    let (h, _snap) = host_en(Arc::new(fake), "mem:///casa").await;
    let mut sub = h.subscribe();
    asentar().await;
    let before = copy_page(&h, &mut sub).await;
    assert!(
        !action(&before, "F8").enabled,
        "the premise: with the capabilities set, off"
    );

    // A re-listing of the slot, which is what a task's end or the watcher
    // does underneath while help stays open.
    h.dispatch(UiAction::RefreshSlot { slot_id: 1 })
        .await
        .expect("host alive");
    asentar().await;

    let after = snapshot_until(&h, &mut sub, "help after the re-listing", |s| {
        s.help.clone()
    })
    .await;
    assert!(
        !action(&after, "F8").enabled,
        "the location has not changed: re-listing cannot light up what it \
         refuses to write ({:?})",
        action(&after, "F8")
    );
}

/// And the DESTINATION is asked of the destination slot, not the one that
/// has focus.
///
/// The other half of the fact, and the one a single slot cannot test: with
/// a writable source and a read-only destination, F5 — which writes there —
/// turns off and F8 — which writes here — stays lit. A `source_read_only`
/// copied onto `dest_read_only` would pass the test above and fail this one.
#[tokio::test]
async fn a_read_only_destination_dims_the_copy_and_not_the_delete() {
    let mut fake = Fake::default();
    fake.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    fake.put("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    fake.capabilities.insert(
        "mem:///casa/docs".to_owned(),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        },
    );
    let (h, _snap) = two_panes_with_separate_destination(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    // The helper leaves focus on the slot that navigated. The case here is
    // the other one: the reader is in `/home`, which writes, looking at
    // `/home/docs`, which does not.
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    asentar().await;

    let page = copy_page(&h, &mut sub).await;
    let copy = action(&page, "F5");
    assert!(
        !copy.enabled,
        "the destination does not accept the copy: {copy:?}"
    );
    assert_eq!(
        copy.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "reason-read-only"),
        "{copy:?}"
    );

    let delete = action(&page, "F8");
    assert!(
        delete.enabled,
        "deleting happens at the SOURCE, which does write: dimming it would \
         be counting the wrong slot's impediment ({delete:?})"
    );
}

/// With the cursor on a `.zip`, help offers `Enter`: it is what it does.
///
/// The other fact that told a different story than the key. `enterable`
/// asked `kind == Dir`, so the archives page — whose first sentence is
/// literally "Enter on a compressed file enters it" — offered that very row
/// off, with "does not apply here". Now the shared site answers it, the
/// same one that navigates (ADR 0077).
#[tokio::test]
async fn with_the_cursor_on_a_zip_help_offers_enter() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"cosas.zip".to_vec(), false)]);
    let (h, snap) = host_en(Arc::new(fake), "mem:///casa").await;
    let mut sub = h.subscribe();
    let b = listing_of(&snap, 1);
    let zip = b
        .rows
        .iter()
        .find(|r| r.display_name == "cosas.zip")
        .expect("the file is there");
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key: zip.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    let page = help_page(&h, &mut sub, "archives").await;
    let enter = action(&page, "Enter");
    assert!(
        enter.enabled,
        "the page says Enter goes into a compressed file, and the row \
         offered it off: {enter:?}"
    );
}

/// Opens help on the copy, delete and rename page.
///
/// It is the corpus page documenting the four keys these facts dim, and its
/// EXECUTABLE rows are where the frozen facts arrive — what the reader sees.
/// The keyboard sheet is no good for this: its `avail` is a build fact, not
/// about where the reader is.
pub(super) async fn copy_page(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::HelpView {
    help_page(h, sub, "copying").await
}

/// Opens help and walks the sidebar to a page, like the reader would.
pub(super) async fn help_page(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    topico: &str,
) -> norte_ui_host::dto::HelpView {
    let mut page = open_help(h, sub).await;
    let mut row = 0;
    // Over the CURRENT length: the sidebar grows once the extensions
    // catalogue lands, so the first snapshot's falls short.
    while row < u32::try_from(page.sidebar.len()).expect("fits") {
        if page.topic_id == topico {
            return page;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host alive");
        page = next_help(sub).await.expect("still open");
        row += 1;
    }
    assert_eq!(page.topic_id, topico, "the sidebar carries the page");
    page
}

/// The executable row of a chord on an already-open page.
pub(super) fn action<'p>(
    page: &'p norte_ui_host::dto::HelpView,
    chord: &str,
) -> &'p norte_ui_host::dto::HelpActionView {
    page.actions
        .iter()
        .find(|a| !a.opens_topic && a.chord == chord)
        .unwrap_or_else(|| {
            panic!(
                "page `{}` documents {chord}: {:?}",
                page.topic_id, page.actions
            )
        })
}

/// `Enter` on a compressed FILE enters it, it does not open it outside.
///
/// Parity inventory divergence number one: the terminal navigates to
/// `zip+file://…/!/` and the window was handing it to `xdg-open`. The host
/// looked at `kind != Dir` and that is where the question ended — while its
/// own comment claimed it made "the same decision as the TUI" (ADR 0077),
/// which is exactly the false claim that ADR exists to prevent.
///
/// The window ALREADY knew `archive_root_for`: it uses it to unpack and to
/// check a container. What was missing was asking it on open.
#[tokio::test]
async fn entering_a_compressed_file_navigates_inside() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"cosas.zip".to_vec(), false)]);
    let (h, snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let first_one = listing(&snap);

    h.dispatch(UiAction::Activate {
        slot_id: first_one.slot_id,
        key: norte_ui_host::RowKey(0),
        generation: first_one.generation,
    })
    .await
    .expect("host alive");

    let inside = snapshot_until(&h, &mut sub, "the listing inside the archive", |s| {
        let b = listing(s);
        b.path_display
            .contains("zip")
            .then(|| b.path_display.clone())
    })
    .await;
    assert!(
        inside.contains("cosas.zip"),
        "it enters the container, it is not handed to the desktop: {inside}"
    );
}

/// And over a SYMLINK it navigates, as in the terminal.
///
/// The other half of the same divergence: the host treated it as "not a
/// directory", that is, as a file, so a link to a folder was handed to the
/// desktop instead of entering it.
#[tokio::test]
async fn entering_a_symlink_navigates_like_the_terminal() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"atajo".to_vec(), false)]);
    fake.put_kind("mem:///casa/atajo", norte_proto::EntryKind::Symlink);
    fake.put("mem:///casa/atajo", vec![(b"dentro.txt".to_vec(), false)]);
    let (h, snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let first_one = listing(&snap);

    let ack = h
        .dispatch(UiAction::Activate {
            slot_id: first_one.slot_id,
            key: norte_ui_host::RowKey(0),
            generation: first_one.generation,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "ack fue {ack:?}");

    let inside = snapshot_until(&h, &mut sub, "el listado del enlace", |s| {
        let b = listing(s);
        b.path_display
            .ends_with("atajo")
            .then(|| b.path_display.clone())
    })
    .await;
    assert!(inside.ends_with("atajo"), "{inside}");
}

/// `[ui] confirm_quit` also asks in the WINDOW.
///
/// Closing it NEVER asked: the `CloseRequested` handler dumped the session
/// and closed. With `confirm_quit = "always"` the terminal keeps F10 and the
/// window would leave with a half-done copy without saying anything — and
/// `always` is exactly the value the guard asks for.
///
/// The decision of whether to ask is the SHARED one
/// (`settings::quit_needs_confirm`), whose rustdoc already named a window
/// `confirm_quit_should_open` that did not exist.
#[tokio::test]
async fn closing_the_window_asks_if_the_config_says_so() {
    let mut cfg = test_settings();
    cfg.common.ui_confirm_quit = norte_config::ConfirmQuit::Always;
    let (h, _snap) = host_en_con(fake_tree(), "mem:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    let ack = h.dispatch(UiAction::RequestQuit).await.expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let dialog = snapshot_until(&h, &mut sub, "the quit dialog", |s| {
        s.dialogs.first().cloned()
    })
    .await;
    assert_eq!(dialog.title_key, "modal-quit-title");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), effects.recv())
            .await
            .is_err(),
        "asking does NOT close: the close effect comes out on confirming"
    );

    // And on confirming, it does.
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), effects.recv())
        .await
        .expect("the effect comes out")
        .expect("channel alive");
    assert!(
        matches!(effect, norte_ui_host::dto::NativeEffect::CloseWindow),
        "{effect:?}"
    );
}

/// With `confirm_quit = "never"` there is no asking: it closes and that is
/// it.
#[tokio::test]
async fn without_confirmation_closing_opens_nothing() {
    let mut cfg = test_settings();
    cfg.common.ui_confirm_quit = norte_config::ConfirmQuit::Never;
    let (h, _snap) = host_en_con(fake_tree(), "mem:///casa", cfg).await;
    let mut effects = h.native_effects();

    h.dispatch(UiAction::RequestQuit).await.expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), effects.recv())
        .await
        .expect("the effect comes out")
        .expect("channel alive");
    assert!(
        matches!(effect, norte_ui_host::dto::NativeEffect::CloseWindow),
        "{effect:?}"
    );
}

/// `F10` — `app.quit` in the seven presets — closes the window through the
/// SAME path as the close button. It was classified as "does not apply to a
/// window", and the reader pressed the usual quit key without anything
/// happening.
#[tokio::test]
async fn the_quit_key_closes_the_window() {
    let mut cfg = test_settings();
    cfg.common.ui_confirm_quit = norte_config::ConfirmQuit::Never;
    let (h, _snap) = host_en_con(fake_tree(), "mem:///casa", cfg).await;
    let mut effects = h.native_effects();

    let ack = h.dispatch(press("F10")).await.expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), effects.recv())
        .await
        .expect("the effect comes out")
        .expect("channel alive");
    assert!(
        matches!(effect, norte_ui_host::dto::NativeEffect::CloseWindow),
        "{effect:?}"
    );
}

/// `[ui] quick_search` also picks the mode in the WINDOW.
///
/// The host was starting the incremental search hardwired to `Filter`, so
/// `quick_search = "jump"` moved the cursor in `ntc` and narrowed the
/// listing in the window: the same key with two behaviors. The DTO already
/// knew how to say both modes; what was missing was reading the key.
#[tokio::test]
async fn the_quick_search_mode_comes_from_the_config() {
    let mut cfg = test_settings();
    cfg.quick_search_mode = norte_frontend::nav::Mode::Jump;
    let (h, _snap) = host_en_con(fake_tree(), "mem:///casa", cfg).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.quick-search").await;
    let modo = snapshot_until(&h, &mut sub, "the search open", |s| {
        listing(s).quick.as_ref().map(|q| q.mode.clone())
    })
    .await;
    assert_eq!(modo, "jump", "the mode is set by the configuration");
}

/// `[ui.columns]` also styles the columns in the WINDOW (#108).
///
/// The window was asking for the style with `ColumnStyle::default_for_id`,
/// that is, the FACTORY one, in both header and cells. So the list of
/// columns and their order came from the configuration and everything else
/// — its own `header`, `format`, `align`, `width` — was dead: a whole
/// configuration block alive in the terminal and with no effect here.
///
/// Both doors are checked at once, since both were wrong: the header with
/// its own label, and the cell with `format = "iso"`.
#[tokio::test]
async fn per_column_style_governs_in_the_window() {
    let mut fake = Fake::default();
    fake.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".to_owned(), "mtime".to_owned()]),
        specs: [(
            "mtime".to_owned(),
            norte_config::ColumnSpec {
                width: None,
                align: None,
                format: Some("iso".to_owned()),
                header: Some("When".to_owned()),
            },
        )]
        .into_iter()
        .collect(),
        ..norte_config::ColumnsConfig::default()
    };
    let columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns,
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    let header = listing(&snap)
        .columns
        .iter()
        .find(|c| c.id == "mtime")
        .expect("the column is there")
        .clone();
    assert_eq!(
        header.label, "When",
        "the custom label overrides the factory one"
    );

    // The other half of the fix — a CELL's `format` — is checked in
    // `norte-gui-tauri/tests/local_cells.rs`: here the fake backend does
    // not carry a date in the listing (#52, the listing is lazy) and
    // hydrating it would need mounting half a probe just to test a format.
    // Over there are real files, which is where that question answers
    // itself.
    let _ = (&h, &mut sub);
}

/// `openers.toml` also governs in the WINDOW (#28).
///
/// The openers table was only read by the terminal: the window handed
/// everything to the desktop handler, so a rule saying "PDFs with zathura"
/// held in `ntc` and not in `norte-gui`. It is a fully documented feature
/// honored by only one surface.
#[tokio::test]
async fn a_declared_opener_governs_in_the_window() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let mut cfg = test_settings();
    cfg.openers = norte_frontend::openers::OpenersConfig::parse(
        "[[opener]]\nmime = \"text/*\"\ncommand = [\"cat\", \"%f\"]\ndetached = false\n",
    )
    .expect("test config");
    let (h, _snap) = host_en_con(Arc::new(fake), "file:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    run_by_palette(&h, &mut sub, "pane.open").await;
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), effects.recv())
        .await
        .expect("the effect comes out")
        .expect("channel alive");
    let norte_ui_host::dto::NativeEffect::RunProgram { argv, cwd, .. } = effect else {
        panic!("with a declared rule THAT program is run, not the desktop's: {effect:?}");
    };
    let as_text: Vec<String> = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(
        as_text[0].ends_with("/cat"),
        "the program is resolved to an absolute path BEFORE giving it a cwd (ADR 0082): {as_text:?}"
    );
    assert!(
        as_text[1].ends_with("/casa/notas.txt"),
        "and `%f` is the pointed-at file: {as_text:?}"
    );
    assert_eq!(
        cwd.map(|c| String::from_utf8_lossy(&c).into_owned()),
        Some("/casa".to_owned()),
        "the child opens in the directory being looked at (#144)"
    );
}

/// With no rule for that mimetype, the DESKTOP handler is left.
///
/// The last resort is the usual one: writing configuration cannot be a
/// requirement for opening a PDF.
#[tokio::test]
async fn with_no_declared_rule_it_opens_with_the_desktop() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let (h, _snap) = host_en(Arc::new(fake), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    run_by_palette(&h, &mut sub, "pane.open").await;
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), effects.recv())
        .await
        .expect("the effect comes out")
        .expect("channel alive");
    assert!(
        matches!(effect, norte_ui_host::dto::NativeEffect::OpenPath { .. }),
        "with no rule, the desktop: {effect:?}"
    );
}

/// `[ui] editor` governs on F4, and if there is none, the desktop handler.
///
/// The window was ALWAYS sending `pane.edit` to the same place as
/// `pane.open`. The deliberate part of that decision is not launching
/// `$EDITOR` — a terminal editor inside a window that has no terminal — and
/// that stands. What was not deliberate was ignoring `[ui] editor`, which
/// names an explicit program and can perfectly well be graphical: its
/// sibling key `[ui] diff` IS honored by this window.
#[tokio::test]
async fn the_configured_editor_governs_in_the_window() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let mut cfg = test_settings();
    cfg.common.ui_editor = Some(vec!["cat".to_owned(), "%f".to_owned()]);
    let (h, _snap) = host_en_con(Arc::new(fake), "file:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    run_by_palette(&h, &mut sub, "pane.edit").await;
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), effects.recv())
        .await
        .expect("the effect comes out")
        .expect("channel alive");
    let norte_ui_host::dto::NativeEffect::RunProgram { argv, .. } = effect else {
        panic!("with `[ui] editor` set, THAT editor is run: {effect:?}");
    };
    let as_text: Vec<String> = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(as_text[0].ends_with("/cat"), "{as_text:?}");
    assert!(as_text[1].ends_with("/casa/notas.txt"), "{as_text:?}");
}

/// #312: comparing two files from the window. The operand and the program
/// are the decisions shared with the TUI (`diffpair`, `[ui] diff`, `diff -u`
/// by default); what changes is that the host runs the program WAITING FOR
/// IT — the argv comes out resolved and interpolated, in bytes — and what it
/// printed comes back as an action and is shown in its panel until it
/// closes.
#[tokio::test]
async fn comparing_two_files_runs_the_comparator_and_shows_its_output() {
    let mut fake = Fake::default();
    fake.put(
        "file:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(fake);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    // With only ONE under the cursor and none opposite, the command SAYS so.
    // `alt+C` is its shortcut in the orthodox preset.
    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "C".to_owned(),
            ctrl: false,
            alt: true,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-compare-files-need-two"),
        "was {ack:?}"
    );

    // With both marked: the effect comes out with the default argv,
    // resolved.
    by_palette(&h, &mut sub, "mark.all").await;
    run_by_palette(&h, &mut sub, "pane.compare-files").await;
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), effects.recv())
        .await
        .expect("the effect comes out")
        .expect("channel alive");
    let norte_ui_host::dto::NativeEffect::RunProgram {
        title_key,
        argv,
        cwd,
        detached,
    } = effect
    else {
        panic!("expected to run a program: {effect:?}");
    };
    assert_eq!(title_key, "program-output-compare");
    assert!(
        !detached,
        "`diff -u` is expected to be waited for: its output is what is shown"
    );
    let as_text: Vec<String> = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(
        as_text[0].ends_with("/diff"),
        "the program is resolved to an absolute path (ADR 0082): {as_text:?}"
    );
    assert_eq!(
        &as_text[1..],
        ["-u", "/casa/a.txt", "/casa/b.txt"],
        "the TWO native paths, interpolated through `%F`"
    );
    assert_eq!(cwd.as_deref(), Some(b"/casa".as_slice()));

    // What it printed comes back as an action and is shown, by lines and
    // masked; Esc closes it.
    h.dispatch(UiAction::ProgramFinished {
        title_key,
        command: as_text.join(" "),
        output: b"--- a.txt\n+++ b.txt\n-hola\x1b[31m\n+adios\n".to_vec(),
        truncated: false,
        failed: false,
    })
    .await
    .expect("host alive");
    let with_output = snapshot_until(&h, &mut sub, "the program's output", |s| {
        s.program_output.clone()
    })
    .await;
    assert_eq!(with_output.title_key, "program-output-compare");
    assert_eq!(with_output.lines.len(), 4, "{with_output:?}");
    assert_eq!(with_output.lines[0], "--- a.txt");
    assert!(
        with_output.text_hostile,
        "the third line's escape was marked"
    );
    assert!(!with_output.lines[2].contains('\x1b'));
    assert!(!with_output.failed);
    h.dispatch(press("Escape")).await.expect("host alive");
    snapshot_until(&h, &mut sub, "the closed panel", |s| {
        s.program_output.is_none().then_some(())
    })
    .await;
}

/// The plan is REVIEWED before anything else: it arrives, gets painted pair
/// by pair, and the core's verdict arrives AFTERWARD, on its own trip.
#[tokio::test]
async fn a_plan_is_reviewed_before_being_applied() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "ep02.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;

    let first = next_revision(&mut sub).await.expect("opens");
    assert_eq!(first.total, 2, "both pairs");
    assert_eq!(first.pairs[0].from.text, "ep1.mkv");
    assert_eq!(first.pairs[0].to.text, "ep01.mkv");
    assert!(
        backend.batches.lock().expect("lotes").is_empty(),
        "reviewing applies nothing"
    );

    // The verdict arrives on its own trip, and until then it cannot be
    // approved.
    let mut v = first;
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }
    assert!(v.confirmable, "the core said it is applicable");
    assert!(
        v.real_steps_note.contains('2'),
        "and how many it REALLY renames, said and translated: {}",
        v.real_steps_note
    );
    assert!(!v.status.is_empty() && !v.status.starts_with("modal-"));
}

/// Approving sends ONE task for the batch, with the `plan_hash` the core
/// returned: EXACTLY what was shown gets executed.
#[tokio::test]
async fn approving_sends_the_batch_with_the_cores_hash() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let mut v = next_revision(&mut sub).await.expect("opens");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }

    // The FIRST key only acknowledges the screen: it opened on its own and
    // the keyboard was left in place, so the key that was already on its
    // way cannot be a response. The second one does approve.
    h.dispatch(press("y")).await.expect("host alive");
    assert!(
        backend.batches.lock().expect("lotes").is_empty(),
        "the first key approves nothing"
    );
    h.dispatch(press("y")).await.expect("host alive");
    let batches = anotados(&backend, "the batch approved", 1, |f| {
        f.batches.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(batches.len(), 1, "ONE task for the whole batch");
    let (dir, pairs, hash) = &batches[0];
    assert_eq!(dir.to_wire(), "mem:///casa");
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].from.as_bytes(), b"ep1.mkv");
    assert_eq!(pairs[0].to.as_bytes(), b"ep01.mkv");
    assert_eq!(hash, &test_hash(), "the hash is the one the core gave");
}

/// A plan the core does NOT accept cannot be approved, and it is said.
#[tokio::test]
async fn a_non_applicable_plan_is_not_approved() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut v = verdict_ok(&pares);
    v.executable = false;
    v.steps.clear();
    v.collisions = vec![norte_proto::methods::RenameCollision {
        pair_index: 0,
        kind: norte_proto::methods::RenameCollisionKind::External,
        name: norte_proto::Segment::new(b"ep01.mkv".to_vec()).expect("seg"),
    }];
    let backend = fake_with_plan(&pares, Some(v));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let mut r = next_revision(&mut sub).await.expect("opens");
    for _ in 0..40 {
        if !r.detail.is_empty() {
            break;
        }
        r = next_revision(&mut sub).await.expect("still open");
    }
    assert!(!r.confirmable, "the core said no");
    assert!(
        !r.detail.is_empty(),
        "and the collision IS READABLE: {:?}",
        r.detail
    );

    // The first key acknowledges the screen; the second tries to approve.
    h.dispatch(press("y")).await.expect("host alive");
    let ack = h.dispatch(press("y")).await.expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-plan-not-applicable".to_owned()
        },
        "{ack:?}"
    );
    assert!(backend.batches.lock().expect("lotes").is_empty());
}

/// A pair that is not a legal name brings down the WHOLE plan: applying
/// "whatever's still good" from a tampered plan is what this belt exists to
/// prevent.
#[tokio::test]
async fn an_invalid_pair_brings_down_the_whole_plan() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "../fuera")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&[("ep1.mkv", "ep01.mkv")])));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;

    // The model's plan has already come back: what is checked is what the
    // host does WITH it, not that it has not arrived yet.
    until(&backend, "the model's plan, already served", |f| {
        f.en_calma().then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(
        snapshot.ai_rename.is_none(),
        "the review is not even opened: {:?}",
        snapshot.ai_rename
    );
    assert!(
        backend
            .verdicts_pedidos
            .lock()
            .expect("veredictos")
            .is_empty(),
        "the core is not even asked for a verdict on a tampered plan"
    );
    assert!(snapshot.status.message.is_some(), "and it is said");
}

/// A plan that arrives LATE, after the reader closed the review, does not
/// reopen it.
///
/// The model takes its time, and in that window the reader can discard. Without
/// bumping the epoch on close, the plan was landing on top of a screen its
/// owner had already dismissed — with the keys already resting on it.
#[tokio::test]
async fn a_plan_that_arrives_late_does_not_reopen_what_was_closed() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.verdict = Some(verdict_ok(&pares));
    f.retraso_ia_ms = 150;
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;

    // Before the model answers, it is discarded. And it waits for the late
    // plan to HAVE arrived: without that, the test would pass by not having
    // waited long enough, which is the most silent way of testing nothing.
    h.dispatch(press("Escape")).await.expect("host alive");
    until(&backend, "the late plan, already served", |f| {
        f.en_calma().then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(
        snapshot.ai_rename.is_none(),
        "the late plan does not reopen what was closed: {:?}",
        snapshot.ai_rename
    );
    assert!(backend.batches.lock().expect("lotes").is_empty());
}

/// Discarding applies nothing, and the review stays closed.
#[tokio::test]
async fn discarding_closes_and_applies_nothing() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    next_revision(&mut sub).await.expect("opens");

    h.dispatch(press("Escape")).await.expect("host alive");
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(snapshot.ai_rename.is_none(), "it closed and stays closed");
    assert!(backend.batches.lock().expect("lotes").is_empty());
}

/// The plan's names are proposed by a MODEL over names anyone could have
/// written: they get masked and it is SAID.
#[tokio::test]
async fn a_hostile_name_from_the_plan_is_marked() {
    // From the canonical corpus, not hand-written: a name invented in the
    // test proves what the test believes, and the corpus proves what is
    // really there.
    let bytes = hostile("rtl_override");
    let altered = String::from_utf8(bytes).expect("the corpus one is UTF-8");
    let pares = [("ep1.mkv", altered.as_str())];
    let backend = fake_with_plan(&pares, None);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let r = next_revision(&mut sub).await.expect("opens");
    assert!(
        !r.pairs[0].to.text.contains('\u{202E}'),
        "masked: {:?}",
        r.pairs[0].to
    );
    assert!(r.pairs[0].to.hostile, "and marked: {:?}", r.pairs[0].to);
    assert!(!r.pairs[0].from.hostile, "the source one is not");
}

/// A plan longer than the window gets scrolled through in full.
#[tokio::test]
async fn a_long_plan_is_scrolled_through() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = fake_with_plan(&refs, None);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let r = next_revision(&mut sub).await.expect("opens");
    assert_eq!(r.total, 12);
    assert!(
        r.pairs.len() < 12,
        "only the window travels: {}",
        r.pairs.len()
    );
    assert_eq!(r.first_visible, 0);

    // The first key acknowledges the screen; scrolling happens from there.
    h.dispatch(press("PageDown")).await.expect("host alive");
    h.dispatch(press("PageDown")).await.expect("host alive");
    // The core's verdict travels over the SAME ordered channel, so one of
    // its updates can arrive ahead of the scroll's.
    let mut bajado = next_revision(&mut sub).await.expect("still open");
    for _ in 0..10 {
        if bajado.first_visible > 0 {
            break;
        }
        bajado = next_revision(&mut sub).await.expect("still open");
    }
    assert!(bajado.first_visible > 0, "it scrolled: {bajado:?}");

    for _ in 0..10 {
        h.dispatch(press("PageDown")).await.expect("host alive");
    }
    let cap = next_revision(&mut sub).await.expect("still open");
    assert!(
        cap.first_visible + cap.pairs.len() as u64 <= cap.total,
        "the window does not go past the plan: {cap:?}"
    );
}

/// In read-only, nobody is asked for a plan.
#[tokio::test]
async fn in_read_only_no_plan_is_requested() {
    let backend = fake_with_plan(&[("a", "b")], None);
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    // Not even the palette offers it: a read-only window does not list what
    // mutates. And even if it arrived through another door, the guard
    // refuses it.
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    let p = next_palette(&mut sub).await.expect("the palette opens");
    assert!(
        !p.rows.iter().any(|r| r.text == "pane.ai-rename"),
        "a read-only window does not offer requesting a plan"
    );
    asentar().await;
    assert!(
        backend
            .instrucciones
            .lock()
            .expect("instrucciones")
            .is_empty()
    );
}

/// A plan opens over the directory it was REQUESTED for, even if the reader
/// navigated away while the model was thinking.
///
/// Reading the slot's directory on landing promised to rename what is
/// visible — which is already something else — and would have renamed the
/// old one.
#[tokio::test]
async fn a_plan_opens_over_the_directory_it_was_planned_for() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"ep1.mkv".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.retraso_ia_ms = 150;
    let backend = Arc::new(f);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listing(&snap);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("it is there");
    let (key, generation) = (docs.key, b.generation);
    request_plan(&h, &mut sub).await;

    // And while the model thinks, the reader goes to another directory.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");

    let r = next_revision(&mut sub).await.expect("opens just the same");
    assert!(
        r.dir.text.ends_with("/casa"),
        "the plan is for the directory it was planned for, not the one seen now: {:?}",
        r.dir
    );
}

/// TWO live requests at once: the first one's response cannot kill the
/// second one.
///
/// `Option::take` emptied the slot BEFORE the filter looked, so an old
/// response would sweep away the live request and BOTH would be left
/// unopened — without saying anything, and indistinguishable from a dead
/// daemon. And the sequence is the normal one: request, see nothing, request
/// again.
#[tokio::test]
async fn two_requests_at_once_and_the_second_still_opens() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.verdict = Some(verdict_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Two requests in a row, without waiting for the first.
    request_plan(&h, &mut sub).await;
    request_plan(&h, &mut sub).await;

    let r = next_revision(&mut sub).await.expect("the second one opens");
    assert_eq!(r.total, 1);
    assert_eq!(
        backend.instrucciones.lock().expect("instrucciones").len(),
        2,
        "both were requested"
    );
}

/// With a plan in flight, `Escape` closes the PALETTE and does not kill the
/// plan.
///
/// The branch that abandons the plan sat above the overlay dispatch, so a
/// single key did two things wrong: it left the palette open and swept away
/// the plan the reader actually wanted.
#[tokio::test]
async fn with_a_plan_in_flight_escape_closes_the_palette() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.verdict = Some(verdict_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    let _ = next_palette(&mut sub).await.expect("opens");
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(snapshot.palette.is_none(), "Escape closed the palette");

    // And the plan is still alive: it arrives and opens.
    let r = next_revision(&mut sub).await.expect("the plan survived");
    assert_eq!(r.total, 1);
}

/// The same with quick filter: `Escape` cancels it, and the plan continues.
#[tokio::test]
async fn with_a_plan_in_flight_escape_cancels_the_filter() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.verdict = Some(verdict_ok(&pares));
    // The plan is HELD until this test releases it, instead of taking a few
    // milliseconds. With a delay, "still thinking" lasted as long as the
    // clock did: under load the review would land in the middle of the
    // palette's fourteen keystrokes, the keyboard would stay put (it is
    // serviced BEFORE the filter and the palette, `input.rs`), `Escape`
    // would discard IT instead, and this test would wait fifteen seconds for
    // a review that had already come and gone. With the gate, what the test
    // asserts is a fact.
    let gate = Arc::new(crate::backend_fake::Gate::default());
    f.gate_ia = Some(Arc::clone(&gate));
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;

    // `pane.quick-search` is `ctrl+s` in the orthodox preset; it is reached
    // through the palette so as not to depend on the key.
    by_palette(&h, &mut sub, "quick-search").await;
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(
        listing(&snapshot).quick.is_none(),
        "Escape cancelled the filter"
    );

    // And the plan was still in flight meanwhile: now it is released and
    // lands. Through the SNAPSHOT and not by waiting for its patch:
    // `next_snapshot` consumes envelopes until it finds one, so it could
    // swallow the patch and then wait forever for an event that already
    // passed. A `Resync` re-sends the STATE.
    gate.open();
    let r = snapshot_until(&h, &mut sub, "the plan's review landed", |snapshot| {
        snapshot.ai_rename.clone()
    })
    .await;
    assert_eq!(r.total, 1);
}

/// Discarding a review does not kill a LATER request.
///
/// With a review open, the keyboard belongs to it — so the only way to have
/// two requests and one review at once is the real one: the first is
/// requested, the second's prompt opens while the model thinks, the first
/// review lands UNDERNEATH that dialog, the second request is confirmed, and
/// only then is the review that was left visible discarded. Releasing the
/// in-flight request there used to kill it silently.
#[tokio::test]
async fn discarding_a_review_does_not_kill_the_next_request() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.verdict = Some(verdict_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Request 1, and request 2's prompt open while the model thinks.
    request_plan(&h, &mut sub).await;
    by_palette(&h, &mut sub, "ai-rename").await;
    let id2 = next_dialogs(&mut sub).await[0].id;

    // Review 1 lands UNDERNEATH the dialog.
    let r1 = next_revision(&mut sub).await.expect("the first opens");
    assert_eq!(r1.total, 1);

    // Request 2 is confirmed and review 1 is discarded.
    h.dispatch(UiAction::DialogInput {
        id: id2,
        text: "something else".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: id2,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    h.dispatch(press("Escape")).await.expect("host alive");

    // And request 2 is still alive. The signal that CANNOT be confused with
    // a stray patch from review 1 is the core receiving a SECOND verdict:
    // only a plan that arrived and opened asks for one.
    anotados(&backend, "the second verdict", 2, |f| {
        f.verdicts_pedidos.lock().expect("veredictos").clone()
    })
    .await;
    assert_eq!(
        backend.instrucciones.lock().expect("instrucciones").len(),
        2
    );
}

/// The first key that reaches the review only ACKNOWLEDGES it.
///
/// The screen opens on its own, tens of seconds after the gesture that
/// requested it, and the keyboard stays put. Without this step, the `y` from
/// someone typing `yes.txt` in the quick filter would approve renaming the
/// entire directory.
#[tokio::test]
async fn the_first_key_only_acknowledges_the_review() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let mut v = next_revision(&mut sub).await.expect("opens");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }

    h.dispatch(press("y")).await.expect("host alive");
    asentar().await;
    assert!(
        backend.batches.lock().expect("lotes").is_empty(),
        "the key that was already on its way approves nothing"
    );
    h.dispatch(press("y")).await.expect("host alive");
    anotados(&backend, "the batch the second key approves", 1, |f| {
        f.batches.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(
        backend.batches.lock().expect("lotes").len(),
        1,
        "the second one does: it is already a response"
    );
}

/// `Escape` does NOT need acknowledgment: discarding is safe in both states,
/// and whoever does not want this must be able to get rid of it on the
/// first try.
#[tokio::test]
async fn escape_discards_the_review_on_the_first_try() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    next_revision(&mut sub).await.expect("opens");

    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(snapshot.ai_rename.is_none(), "it left on the first try");
}

/// A chord WITH a modifier is not a response to this screen.
#[tokio::test]
async fn a_chord_with_a_modifier_does_not_approve_the_plan() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    next_revision(&mut sub).await.expect("opens");
    // Acknowledged, so the only thing left standing is the modifier.
    h.dispatch(press("j")).await.expect("host alive");

    for (ctrl, shift) in [(true, false), (false, false)] {
        let ack = h
            .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
                key: "y".to_owned(),
                ctrl,
                alt: false,
                shift,
                meta: true,
            }))
            .await
            .expect("host alive");
        assert_eq!(
            ack,
            ActionAck::Unavailable {
                reason_key: "host-key-unmapped".to_owned()
            },
            "ctrl={ctrl}: {ack:?}"
        );
    }
    asentar().await;
    assert!(backend.batches.lock().expect("lotes").is_empty());
}

/// A plan that has not been scrolled through IN FULL is not approved.
///
/// The window holds five pairs out of up to two hundred fifty-six: without
/// this, pair two hundred would execute without anyone ever having painted
/// it, and the review is the whole defense there is against a plan a model
/// wrote from names controlled by whoever writes into the directory.
#[tokio::test]
async fn a_plan_is_not_approved_without_scrolling_through_it_in_full() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = fake_with_plan(&refs, Some(verdict_ok(&refs)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let mut v = next_revision(&mut sub).await.expect("opens");
    for _ in 0..40 {
        if !v.status.is_empty() && v.total == 12 && v.more_note.contains("12") {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }
    assert!(!v.seen_all, "it has not been seen in full yet");
    assert!(!v.confirmable, "and that is why it cannot be approved");

    // Acknowledge, and try to approve without having scrolled down.
    h.dispatch(press("y")).await.expect("host alive");
    let ack = h.dispatch(press("y")).await.expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-plan-unseen".to_owned()
        },
        "{ack:?}"
    );

    // It gets scrolled to the end and now it does.
    for _ in 0..6 {
        h.dispatch(press("PageDown")).await.expect("host alive");
    }
    asentar().await;
    h.dispatch(press("y")).await.expect("host alive");
    anotados(&backend, "the approved batch", 1, |f| {
        f.batches.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(backend.batches.lock().expect("lotes").len(), 1);
}

/// A name altered OUTSIDE the window is also said.
#[tokio::test]
async fn an_altered_name_that_is_not_visible_is_also_said() {
    let hostile =
        String::from_utf8(hostile("control_escape")).unwrap_or_else(|_| "\u{1b}x".to_owned());
    let mut pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    // At position ELEVEN: outside the first window of five.
    pares[11].1 = hostile;
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = fake_with_plan(&refs, None);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let r = next_revision(&mut sub).await.expect("opens");
    assert!(
        r.pairs.iter().all(|p| !p.to.hostile),
        "none of the visible ones is altered"
    );
    assert!(
        r.hidden_hostile,
        "and even so it says there is one that is not visible: {r:?}"
    );
}

/// The "how much is visible" line is TRANSLATED, not an unsubstituted
/// pattern.
#[tokio::test]
async fn the_how_much_is_visible_line_is_translated() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = fake_with_plan(&refs, None);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let r = next_revision(&mut sub).await.expect("opens");
    assert!(
        !r.more_note.contains('$') && !r.more_note.contains('{'),
        "no unsubstituted patterns: {}",
        r.more_note
    );
    assert!(
        r.more_note.contains("12"),
        "and with the total: {}",
        r.more_note
    );
}

/// Creating a directory also does not write the character the screen made
/// up.
#[tokio::test]
async fn creating_a_directory_with_fffd_is_rejected() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F7")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "caf\u{FFFD}".to_owned(),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-transfer-name-fffd".to_owned()
        },
        "{ack:?}"
    );
    asentar().await;
    assert!(
        backend.creados.lock().expect("creados").is_empty(),
        "a directory is not created with the U+FFFD the screen made up"
    );
}

/// `Enter` does NOT approve the plan.
///
/// It breaks parity with the TUI on purpose: there, the plan is opened by
/// one reader keystroke and the next one is a response. Here the screen
/// opens on its own tens of seconds later, and `Enter` is exactly the key
/// that was being used to walk the tree while the model was thinking — two
/// in a row entering nested directories are normal.
#[tokio::test]
async fn enter_does_not_approve_the_plan() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let mut v = next_revision(&mut sub).await.expect("opens");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }
    for _ in 0..3 {
        h.dispatch(press("Enter")).await.expect("host alive");
    }
    asentar().await;
    assert!(
        backend.batches.lock().expect("lotes").is_empty(),
        "no Enter approves a batch"
    );
}

/// A click on the button DOES respond on the first try: it is a gesture
/// aimed at this screen, not a key that was going somewhere else.
#[tokio::test]
async fn the_button_approves_without_prior_acknowledgment() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let mut v = next_revision(&mut sub).await.expect("opens");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }
    let ack = h
        .dispatch(UiAction::AiRenameDecide { approve: true })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    anotados(&backend, "the batch the button approves", 1, |f| {
        f.batches.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(backend.batches.lock().expect("lotes").len(), 1);
}

/// And discarding with the button closes without applying anything.
#[tokio::test]
async fn the_discard_button_closes_without_applying() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = fake_with_plan(&pares, Some(verdict_ok(&pares)));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    next_revision(&mut sub).await.expect("opens");
    h.dispatch(UiAction::AiRenameDecide { approve: false })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(snapshot.ai_rename.is_none());
    assert!(backend.batches.lock().expect("lotes").is_empty());
}

/// A plan name that STARTS with the arrow cannot pretend to be another
/// pair's destination.
///
/// Corpus fixture `arrow_leading_row_spoof`: `arrow_join_spoof` puts the
/// arrow in the middle and forges ONE pair; this one puts it at the start
/// and forges the row's ROLE. The role is carried by the field — `from` or
/// `to` — not the text, so the host sends both separately and the renderer
/// puts them in different elements.
#[tokio::test]
async fn a_name_that_starts_with_the_arrow_does_not_pretend_to_be_a_destination() {
    let bytes = hostile("arrow_leading_row_spoof");
    let trap = String::from_utf8(bytes).expect("the corpus one is UTF-8");
    let pares = [(trap.as_str(), "ep02.mkv")];
    let backend = fake_with_plan(&pares, None);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_plan(&h, &mut sub).await;
    let r = next_revision(&mut sub).await.expect("opens");
    // The name travels WHOLE and in the field it belongs to: the arrow it
    // carries inside does not turn it into a destination, because the role
    // is not in the text.
    assert!(
        r.pairs[0].from.text.starts_with('\u{2192}'),
        "the real name starts with the arrow: {:?}",
        r.pairs[0].from
    );
    assert_eq!(r.pairs[0].to.text, "ep02.mkv");
    assert_eq!(r.pairs.len(), 1, "one pair, not two: {:?}", r.pairs);
}

/// A name whose display projection does NOT fit on screen cannot be edited
/// here, and it is said.
///
/// The truncation attaches an ellipsis, and `…` is a legal character in a
/// name: it is neither masked nor marked. Editing the field and confirming
/// would write the truncation to disk as part of the name. Fixture
/// `display_expansion_over_clamp`.
#[tokio::test]
async fn a_name_that_does_not_fit_on_screen_is_not_edited() {
    let bytes = hostile("display_expansion_over_clamp");
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(bytes, false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-name-not-editable".to_owned()
        },
        "{ack:?}"
    );
}

/// Injects a FOREIGN task with its own canceller, and returns where to send
/// it progress.
///
/// Each one carries a canceller that points at ITS id: a shared counter says
/// that something was cancelled, not WHICH ONE, and "which one" is exactly
/// what a board with a cursor has to get right.
pub(super) fn inyectar_task(
    tx: &tokio::sync::mpsc::UnboundedSender<norte_ui_host::backend::HostTask>,
    id: u64,
    canceladas: &Arc<std::sync::Mutex<Vec<u64>>>,
) -> tokio::sync::watch::Sender<norte_proto::TaskProgress> {
    let progress = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(id),
        kind: norte_proto::TaskKind::Copy,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (ptx, prx) = tokio::sync::watch::channel(progress);
    let canceladas = Arc::clone(canceladas);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(id),
        progress: prx,
        cancel: Arc::new(move || canceladas.lock().expect("canceladas").push(id)),
        pause: None,
        cola: None,
        foreign: true,
    })
    .expect("the host is listening");
    ptx
}

/// Waits for the next notice, whatever its key.
pub(super) async fn next_notice(sub: &mut norte_ui_host::controller::UiSubscription) -> String {
    for _ in 0..40 {
        match tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("arrives")
            .expect("the host is still alive")
        {
            Update::Message(m) => {
                if let UiUpdate::Notice(UiNotice::Message { key, .. }) = &m.payload {
                    return key.clone();
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no notice ever arrived");
}

/// `Ctrl+K` stops the live task: until now the catalogue bound the key and
/// the host answered `NotHere`, so a copy launched from the window could
/// only be stopped by killing the window.
#[tokio::test]
async fn the_cancel_key_stops_the_live_task() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _p = inyectar_task(&tx, 11, &canceladas);
    next_tasks(&mut sub).await;

    let ack = h
        .dispatch(key_mod("k", true, false))
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![11],
        "the live task was asked to stop"
    );
    assert_eq!(next_notice(&mut sub).await, "msg-cancelling");
}

/// With nothing running, cancelling is neither an error nor silence: it is
/// said.
#[tokio::test]
async fn cancel_without_tasks_says_so() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(key_mod("k", true, false))
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(next_notice(&mut sub).await, "msg-no-tasks");
}

/// A task that has already FINISHED stays on the board, and cancelling it
/// cancels nothing: a live one is looked for, and if there is none it is
/// said.
#[tokio::test]
async fn a_finished_task_is_not_the_one_cancelled() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let p = inyectar_task(&tx, 11, &canceladas);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    next_tasks(&mut sub).await;

    h.dispatch(key_mod("k", true, false))
        .await
        .expect("host alive");
    assert!(
        canceladas.lock().expect("canceladas").is_empty(),
        "a finished task is not asked to stop"
    );
    assert_eq!(next_notice(&mut sub).await, "msg-no-tasks");
}

/// With the processes panel focused, the one under the CURSOR is cancelled,
/// not the last one.
///
/// It is the same rule the panel already had for moving: if the list on
/// screen has a cursor and the key cancels something else, the board paints
/// a selection that does not govern.
#[tokio::test]
async fn with_the_panel_focused_the_cursors_task_is_cancelled() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_con_layout(Arc::new(fake), "full", (200, 60)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _a = inyectar_task(&tx, 11, &canceladas);
    let _b = inyectar_task(&tx, 12, &canceladas);
    // Both on the board before touching the cursor.
    for _ in 0..2 {
        if next_tasks(&mut sub).await.len() == 2 {
            break;
        }
    }

    h.dispatch(UiAction::FocusSlot { slot_id: 7 })
        .await
        .expect("host alive");
    // The board is ordered by id, so the second row is 12.
    h.dispatch(press("Down")).await.expect("host alive");
    h.dispatch(key_mod("k", true, false))
        .await
        .expect("host alive");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![12],
        "the cursor's, not the last one"
    );
}

/// Waits for the next board PATCH, with its cursor.
///
/// Only the patch: the whole snapshot is no good for what this test checks,
/// which is exactly what the renderer knows when NOBODY sends it a
/// snapshot.
pub(super) async fn next_dashboard_patch(
    sub: &mut norte_ui_host::UiSubscription,
) -> (Vec<norte_ui_host::dto::TaskView>, Option<u64>) {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("a board patch, not a hang")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Tasks { tasks, cursor } = c {
                    return (tasks.clone(), *cursor);
                }
            }
        }
    }
    panic!("no board patch ever arrived");
}

/// A task that EXPIRES takes its row with it, and the panel's cursor
/// travels with it.
///
/// The board shrinks on its own — a finished one leaves after ten seconds —
/// and that shifts the rest. Two bugs, and the test covers both:
///
/// - The cursor only travelled in the whole snapshot, so the renderer kept
///   highlighting row N while the cancel key acted on the one the host has
///   bounded to.
/// - And what was stored was the POSITION. When the one leaving is ABOVE the
///   chosen one, bounding is not enough: row 1 starts naming a different
///   task without the reader touching anything, and cancel stops a copy
///   nobody chose.
///
/// VIRTUAL clock, advanced BY HAND for the same reason as in
/// [`una_task_terminada_se_va_del_tablero_sola`]: tokio's automatic jump goes
/// to the nearest timer, which here would be the helpers' deadline.
#[tokio::test(start_paused = true)]
async fn a_task_that_expires_drags_the_panels_cursor() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_con_layout(Arc::new(fake), "full", (200, 60)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let first = inyectar_task(&tx, 11, &canceladas);
    let _b = inyectar_task(&tx, 12, &canceladas);
    let _c = inyectar_task(&tx, 13, &canceladas);
    let _d = inyectar_task(&tx, 14, &canceladas);
    for _ in 0..5 {
        if next_tasks(&mut sub).await.len() == 4 {
            break;
        }
    }

    // Cursor on the SECOND row of four, which is task 12. Neither the first
    // nor the last, and that is the edge case: the one about to expire sits
    // ABOVE it, so an implementation that stores the POSITION and bounds it
    // ends up at 1 — that is, task 13 — while one that stores IDENTITY drops
    // to 0 with 12. With the cursor on the last row, both give the same
    // result and the test would tell nothing apart.
    h.dispatch(UiAction::FocusSlot { slot_id: 7 })
        .await
        .expect("host alive");
    h.dispatch(press("Down")).await.expect("host alive");

    // The first one finishes and, ten seconds later, leaves the board.
    first.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    for _ in 0..4 {
        if next_tasks(&mut sub)
            .await
            .iter()
            .any(|t| t.state == norte_ui_host::dto::TaskStateView::Done)
        {
            break;
        }
    }
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let mut seen = None;
    for _ in 0..5 {
        let (tasks, cursor) = next_dashboard_patch(&mut sub).await;
        if tasks.len() == 3 {
            seen = Some(cursor);
            break;
        }
    }
    assert_eq!(
        seen,
        Some(Some(0)),
        "the chosen one is still 12, now the first row: the patch that \
         removes the row has to say where the cursor lands"
    );

    // And what would get cancelled is that same one. The highlight and the
    // key cannot point at different rows, which is the whole bug; a bounded
    // position here would stop 13.
    h.dispatch(key_mod("k", true, false))
        .await
        .expect("host alive");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![12],
        "the highlighted one and the one that stops are the same"
    );
}

/// Same as [`inyectar_task`], but choosing the CLASS: a batch's report is
/// only requested for a batch.
pub(super) fn inyectar_task_de(
    tx: &tokio::sync::mpsc::UnboundedSender<norte_ui_host::backend::HostTask>,
    id: u64,
    kind: norte_proto::TaskKind,
) -> tokio::sync::watch::Sender<norte_proto::TaskProgress> {
    let progress = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(id),
        kind,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (ptx, prx) = tokio::sync::watch::channel(progress);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(id),
        progress: prx,
        cancel: Arc::new(|| {}),
        pause: None,
        cola: None,
        foreign: false,
    })
    .expect("el host escucha");
    ptx
}

/// A clean report: N applied and nothing else.
pub(super) fn report_clean(n: u64) -> norte_proto::methods::FsRenameBatchReportResult {
    norte_proto::methods::FsRenameBatchReportResult {
        applied: n,
        rolled_back: 0,
        failed_pair: None,
        stuck: None,
        uncertain: None,
        compensations_lost: 0,
    }
}

/// Waits for the board to bring a task with `detail` set.
pub(super) async fn task_detail(sub: &mut norte_ui_host::controller::UiSubscription) -> String {
    for _ in 0..40 {
        let tasks = next_tasks(sub).await;
        if let Some(d) = tasks.first().and_then(|t| t.detail.clone()) {
            return d;
        }
    }
    panic!("no task ever carried a detail");
}

/// A batch that finishes ASKS FOR its report: it is the only signal that the
/// directory was left halfway, and until now nobody asked for it (#272).
#[tokio::test]
async fn a_finished_batch_asks_for_its_report() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    *fake.report.lock().expect("informe") = Some(report_clean(3));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 31, norte_proto::TaskKind::RenameBatch);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let detail = task_detail(&mut sub).await;
    assert_eq!(
        *backend.informes_pedidos.lock().expect("informes"),
        vec![31],
        "the batch's report was requested"
    );
    assert!(detail.contains('3'), "the board says how many: {detail}");
    // A clean batch does not interrupt: there is nothing to decide or look
    // for.
    assert!(!was_dialogs(&mut sub).await, "a clean batch opens nothing");
}

/// `true` if some dialog arrives in what is left to read.
pub(super) async fn was_dialogs(sub: &mut norte_ui_host::controller::UiSubscription) -> bool {
    while let Ok(Some(u)) =
        tokio::time::timeout(std::time::Duration::from_millis(150), sub.recv()).await
    {
        if let Update::Message(m) = u
            && let UiUpdate::Patch(p) = &m.payload
            && p.changes
                .iter()
                .any(|c| matches!(c, norte_ui_host::dto::ViewChange::Dialogs { .. }))
        {
            return true;
        }
    }
    false
}

/// A copy that finishes does NOT ask for a batch report: the report belongs
/// to batches.
#[tokio::test]
async fn a_copy_does_not_ask_for_a_batch_report() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 32, norte_proto::TaskKind::Copy);
    next_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    next_tasks(&mut sub).await;
    asentar().await;
    assert!(
        backend
            .informes_pedidos
            .lock()
            .expect("informes")
            .is_empty()
    );
}

/// A STUCK batch opens a surface that says so, and says WHAT THE FILE IS
/// CALLED NOW: without that name, "it was left halfway" cannot be acted on.
#[tokio::test]
async fn a_stuck_batch_says_so_and_gives_the_current_name() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    *fake.report.lock().expect("informe") = Some(norte_proto::methods::FsRenameBatchReportResult {
        applied: 4,
        rolled_back: 2,
        failed_pair: Some(2),
        stuck: Some(norte_proto::methods::RenameStuckStep {
            from: VPath::parse("mem:///casa/viejo.txt").expect("vpath"),
            to: VPath::parse("mem:///casa/nuevo.txt").expect("vpath"),
            pair_index: 2,
            error: norte_proto::Error::Io { retryable: false },
            journalled: true,
            still_applied: 2,
        }),
        uncertain: None,
        compensations_lost: 1,
    });
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 33, norte_proto::TaskKind::RenameBatch);
    next_tasks(&mut sub).await;
    p.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Io { retryable: false },
        };
    });

    let dialogs = next_dialogs(&mut sub).await;
    let body: String = dialogs[0]
        .body
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("nuevo.txt"),
        "it says what it is called NOW: {body}"
    );
    // And the lost-compensations mark is not kept quiet: a session undo is
    // going to stop right there.
    assert!(body.contains('1'), "{body}");
}

/// A daemon that does NOT know how to report a failed batch does not
/// degrade in silence: it says the outcome was left unchecked.
#[tokio::test]
async fn a_report_that_cannot_be_requested_is_said() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.ajenas.lock().expect("ajenas") = Some(rx);
    // No report: the fake answers `Unsupported`.
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 34, norte_proto::TaskKind::RenameBatch);
    next_tasks(&mut sub).await;
    p.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Io { retryable: false },
        };
    });

    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs[0].title_key, "modal-batch-report-title");
    // And it says EXACTLY that the daemon does not know how to report:
    // "could not be requested" and "this daemon does not know how" are two
    // different things, and confusing them is degrading in silence with
    // more words.
    assert_eq!(
        dialogs[0].body[0].text,
        norte_i18n::t_in(norte_i18n::Lang::Es, "modal-batch-unsupported"),
        "{:?}",
        dialogs[0].body
    );
}

/// Waits for the next bar state carrying persistent notices.
pub(super) async fn next_banners(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Vec<norte_ui_host::dto::BannerView> {
    for _ in 0..40 {
        match tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("arrives")
            .expect("the host is still alive")
        {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Status(s) = c
                            && !s.banners.is_empty()
                        {
                            return s.banners.clone();
                        }
                    }
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no persistent notice ever arrived");
}

/// A session that travels UNENCRYPTED leaves a persistent notice that NAMES
/// it.
///
/// An ephemeral message is no good: the next keystroke erases it, and this
/// is a fact about the whole session. The window was not painting it at
/// all — the channel existed in the SDK and the host was not taking it — so
/// a plaintext FTP read the same as an SFTP.
#[tokio::test]
async fn a_plaintext_session_leaves_a_persistent_notice() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.degradadas.lock().expect("degradadas") = Some(rx);
    let (h, _snap) = host_tree(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionDegraded {
        scheme: "ftp".to_owned(),
        host: "archivo.example".to_owned(),
        reason: "ftp-plaintext".to_owned(),
        detail: None,
    })
    .expect("the host is listening");

    let banners = next_banners(&mut sub).await;
    assert!(
        banners.iter().any(|b| b
            .subject
            .as_ref()
            .is_some_and(|s| s.host == "archivo.example")),
        "the notice names the connection, in its own field: {banners:?}"
    );
}
