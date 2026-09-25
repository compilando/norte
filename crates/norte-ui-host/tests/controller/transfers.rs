use super::*;

// ---------------------------------------------------------------------------
// Copy and move (task 5.1 of phase 5).
//
// The renderer NEVER names a file: it sends `pane.copy` and the host derives
// the source from the active slot's marks and the destination from the slot
// with the `Target` role. Not one path crosses from the webview.
// ---------------------------------------------------------------------------

/// The listing of ONE specific slot in a snapshot.
pub(super) fn listing_of(
    snap: &norte_ui_host::ViewSnapshot,
    slot_id: u32,
) -> &norte_ui_host::dto::BrowserSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Browser(b) if b.slot_id == slot_id => Some(b),
            _ => None,
        })
        .unwrap_or_else(|| panic!("slot {slot_id} is a listing"))
}

/// The setup of two panes with a separate destination, BOXED.
///
/// Twelve tests wait on it, and an `async fn`'s future travels whole across
/// every `await`: as the snapshot grew it went over the 16 KB clippy
/// tolerates, and all twelve call sites turned red at once. The box fixes
/// them in one go, and in the right place — the helper — instead of
/// scattering twelve `Box::pin`s across the tests that merely call it.
pub(super) fn two_panes_with_separate_destination(
    backend: Arc<Fake>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = (UiHost, norte_ui_host::ViewSnapshot)>>> {
    Box::pin(two_panes_with_separate_destination_inner(backend))
}

/// Two panes, with the DESTINATION already in another directory: the real
/// scenario for a copy. Returns the snapshot afterward.
async fn two_panes_with_separate_destination_inner(
    backend: Arc<Fake>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let (h, snap) = host_con_layout(backend, "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    let b2 = listing_of(&snap, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    // The landing snapshot: without waiting for it, F5 would see the
    // destination still in the starting directory and the test would be
    // testing something else.
    //
    // It is ASKED FOR (`wait_snapshot` sends `Resync`) instead of staying and
    // listening: with a large listing, the landing travels in PATCHES and
    // the snapshot that would count it may have already gone by, so a
    // looped `next_snapshot` would sit waiting for one that never comes
    // out again — hung, not red.
    let after = wait_snapshot(&h, &mut sub, "the destination lands in /casa/docs", |f| {
        listing_of(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    // The SOURCE's cursor, on a file that is not the destination directory:
    // with the cursor on `docs`, source and destination are written the
    // same, and an assertion on the dialog's text would not tell which of
    // the two it is looking at.
    let b1 = listing_of(&after, 1);
    let notes = b1
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("the file is there");
    let (key, generation) = (notes.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    (h, after)
}

/// The copy dialog says if it does NOT FIT and if the destination fails to
/// confine.
///
/// The two lines have been in the terminal since #149 and #164 and the
/// window had neither: you found out through a failed task, or you did not
/// find out. Both questions are I/O, so the dialog opens without them and
/// they get filled in when they come back — the same split the terminal
/// does in its loop.
///
/// They fail DIFFERENTLY, and it is deliberate: space swallows the failure
/// ("I don't know" is said by staying silent) and confinement does not,
/// because there silence MEANS "this destination holds onto its writes" and
/// swallowing it would be asserting it without
// TODO(translation): review — this doc comment appears interleaved with the
// next function's; the tail "... without knowing so." is the orphaned
// "/// saberlo." above `the_copy_dialog_warns_about_space_and_confinement`.
/// ADR 0149: the queue switch decides where what gets launched AFTERWARD
/// enters, and that reaches all the way to the backend request.
#[tokio::test]
async fn the_queue_switch_travels_with_the_transfer() {
    let (h, mut sub, backend) = Box::pin(two_panes_on_disk(Vec::new(), unconfined())).await;
    marks_the_files(&h, &mut sub, 1).await;
    // `ctrl+alt+q` in orthodox: the same path as a real keystroke.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "q".to_owned(),
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    settle().await;
    assert!(
        *backend.queued.lock().expect("encoladas"),
        "the copy went out asking for the queue"
    );
}

/// knowing so.
#[tokio::test]
async fn the_copy_dialog_warns_about_space_and_confinement() {
    let (h, mut sub, _b) = Box::pin(two_panes_on_disk(volume_full(), unconfined())).await;
    marks_the_files(&h, &mut sub, 1).await;

    h.dispatch(press("F5")).await.expect("host alive");
    let _ = next_dialogs(&mut sub).await;
    settle().await;

    let notices = snapshot_until(&h, &mut sub, "the dialog with its warnings", |s| {
        // The LAST one, which is the one the renderer paints: with only one
        // they coincide, and they keep coinciding the day they stack up.
        match &s.dialogs.last()?.dest_check {
            norte_ui_host::dto::DestCheckView::Done { warnings } if !warnings.is_empty() => {
                Some(warnings.clone())
            }
            _ => None,
        }
    })
    .await;
    assert_eq!(notices.len(), 2, "both lines: {notices:?}");
    assert!(
        notices[0].contains("libres"),
        "the space one carries both numbers: {notices:?}"
    );
    assert!(
        notices[1].contains("symlink"),
        "the confinement one says what it protects: {notices:?}"
    );
}

/// And a destination that DOES fit and DOES confine says nothing.
///
/// The half of the contract that gets forgotten: a line on every copy is
/// noise, and noise teaches people to skip the line exactly the day it says
/// something.
#[tokio::test]
async fn a_destination_that_fits_and_stays_confined_says_nothing() {
    let (h, mut sub, backend) = Box::pin(two_panes_on_disk(spare_volume(), confining())).await;
    marks_the_files(&h, &mut sub, 1).await;

    h.dispatch(press("F5")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1, "the dialog opens just the same");
    settle().await;

    let d = snapshot_until(&h, &mut sub, "the destination already checked", |s| {
        let d = s.dialogs.last()?;
        matches!(d.dest_check, norte_ui_host::dto::DestCheckView::Done { .. }).then(|| d.clone())
    })
    .await;
    assert_eq!(
        d.dest_check,
        norte_ui_host::dto::DestCheckView::Done {
            warnings: Vec::new()
        },
        "nothing to warn about, so nothing to say"
    );
    // And it WAS ASKED. Without this the test stays green if someone deletes
    // the probe: staying quiet for having nothing to say and staying quiet
    // for never having checked look the same, which is exactly what this
    // field tells apart.
    assert!(
        backend
            .volumes_requests
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0,
        "silence is a RESPONSE, not an omission"
    );
}

/// A volume that does not answer is not a full volume.
///
/// `free_bytes: None` means "did not answer in time", NEVER zero: confusing
/// them turns every slow mount into a false alarm. The confinement line does
/// come out, since it does not depend on this.
#[tokio::test]
async fn a_volume_that_does_not_answer_does_not_invent_an_alarm() {
    let (h, mut sub, _b) = Box::pin(two_panes_on_disk(volume_mudo(), unconfined())).await;
    marks_the_files(&h, &mut sub, 1).await;

    h.dispatch(press("F5")).await.expect("host alive");
    let _ = next_dialogs(&mut sub).await;
    settle().await;

    let notices = snapshot_until(&h, &mut sub, "the dialog with its warning", |s| {
        // The LAST one, which is the one the renderer paints: with only one
        // they coincide, and they keep coinciding the day they stack up.
        match &s.dialogs.last()?.dest_check {
            norte_ui_host::dto::DestCheckView::Done { warnings } if !warnings.is_empty() => {
                Some(warnings.clone())
            }
            _ => None,
        }
    })
    .await;
    assert_eq!(
        notices.len(),
        1,
        "only the confinement one: nothing is known about space ({notices:?})"
    );
    assert!(notices[0].contains("symlink"), "{notices:?}");
}

pub(super) fn disk_volume(free: Option<u64>) -> Vec<norte_proto::methods::Volume> {
    vec![norte_proto::methods::Volume {
        mount: VPath::parse("file:///").expect("wire"),
        label: None,
        fs_type: "ext4".to_owned(),
        kind: norte_proto::methods::VolumeKind::Fixed,
        total_bytes: Some(1_000_000),
        free_bytes: free,
        read_only: false,
    }]
}

/// A disk with no room: the double's files each take up one byte, so zero
/// free is "does not fit" with no need to fabricate gigabytes.
pub(super) fn volume_full() -> Vec<norte_proto::methods::Volume> {
    disk_volume(Some(0))
}

pub(super) fn spare_volume() -> Vec<norte_proto::methods::Volume> {
    disk_volume(Some(1_000_000))
}

pub(super) fn volume_mudo() -> Vec<norte_proto::methods::Volume> {
    disk_volume(None)
}

pub(super) fn unconfined() -> norte_proto::Capabilities {
    norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::CASE_SENSITIVE,
        max_path: None,
    }
}

pub(super) fn confining() -> norte_proto::Capabilities {
    norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::CASE_SENSITIVE
            | norte_proto::CapabilityFlags::CONFINED_WRITES,
        max_path: None,
    }
}

/// Two listings over `file://`, the only scheme that hangs off a volume on
/// this machine: a `mem://` has no free space to look at, so these tests
/// cannot be written over the usual tree.
pub(super) async fn two_panes_on_disk(
    volumes: Vec<norte_proto::methods::Volume>,
    caps_dest: norte_proto::Capabilities,
) -> (UiHost, norte_ui_host::UiSubscription, Arc<Fake>) {
    let mut f = Fake::default();
    f.put(
        "file:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"uno".to_vec(), false),
            (b"dos".to_vec(), false),
        ],
    );
    f.put("file:///casa/docs", Vec::new());
    f.volumes = volumes;
    f.capabilities
        .insert("file:///casa/docs".to_owned(), caps_dest);
    let backend = Arc::new(f);
    let (h, snap) = orthodox_host_in(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    // Slot 2 goes down into `docs`, which is the role's destination.
    let b2 = listing_of(&snap, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key: docs.key,
        generation: b2.generation,
    })
    .await
    .expect("host alive");
    wait_snapshot(&h, &mut sub, "the destination lands in docs", |f| {
        listing_of(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    (h, sub, backend)
}

/// Marks a slot's FILES and leaves the directory out: with a directory
/// inside there is no total to add up (it does not say how much it takes
/// up) and the space question never gets asked.
pub(super) async fn marks_the_files(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    slot: u32,
) {
    let snapshot = snapshot(h, sub).await;
    let b = listing_of(&snapshot, slot);
    let keys: Vec<_> = b
        .rows
        .iter()
        .filter(|r| r.display_name == "uno" || r.display_name == "dos")
        .map(|r| r.key)
        .collect();
    assert!(!keys.is_empty(), "there are files to mark");
    for key in keys {
        h.dispatch(UiAction::ToggleMark {
            slot_id: slot,
            key,
            generation: b.generation,
        })
        .await
        .expect("host alive");
    }
}

/// Like [`host_con_layout`] with `orthodox`, but starting wherever is said:
/// volumes only answer over `file://`.
pub(super) async fn orthodox_host_in(
    backend: Arc<Fake>,
    start: &str,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse(start).expect("vpath"),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts")
}

/// F5 does not copy: it opens the confirmation, and that confirmation SAYS
/// where it is going.
///
/// In a window with two listings the destination is not obvious — there is
/// no "the other pane" when there are three — so the dialog is the only
/// place it can be read before accepting.
#[tokio::test]
async fn copying_asks_for_confirmation_and_says_where_to() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1, "ONE dialog opens");
    assert_eq!(dialogs[0].title_key, "modal-copy-title");
    let body = &dialogs[0].body;
    assert!(
        body.iter().any(|l| l.text.ends_with("/casa/notas.txt")),
        "the body is what gets transferred: {body:?}"
    );
    let dest = dialogs[0]
        .destination
        .as_ref()
        .expect("a transfer says where it is going");
    assert!(
        dest.text.ends_with("/casa/docs"),
        "and the destination goes in ITS OWN field: {dest:?}"
    );
    assert!(
        body.iter().all(|l| !l.text.contains("/casa/docs")),
        "not repeated among the body's lines: {body:?}"
    );
    assert!(
        backend.transfers.lock().expect("transferencias").is_empty(),
        "opening the dialog copies nothing"
    );
}

/// **Splitting reads the size in BINARY** (#132, #290): `10M` is 10 MiB,
/// which is what it means in a file manager, not ten million.
#[tokio::test]
async fn splitting_reads_the_size_in_binary() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    // The cursor, on a FILE: it starts in `docs/`, which is also the
    // destination directory, and there the check below would tell nothing
    // apart.
    h.dispatch(press("Down")).await.expect("host alive");

    run_by_palette(&h, &mut sub, "pane.split-file").await;
    let id = next_dialogs(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "10M".to_owned(),
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
    let ps = annotated(&backend, "the split queued", 1, |f| {
        f.split.lock().expect("partidos").clone()
    })
    .await;
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].part_bytes, 10 * 1024 * 1024, "MiB, not millions");
    // Whatever entry is under the cursor does not matter — the listing
    // sorts and the corpus throws in a hostile name — what this test pins
    // down is WHICH pane each thing comes out of.
    assert_eq!(
        ps[0].path.parent().map(|p| p.to_wire()).as_deref(),
        Some("mem:///casa"),
        "the file comes out of the ACTIVE pane"
    );
    assert_eq!(
        ps[0].dest_dir.to_wire(),
        "mem:///casa/docs",
        "and the parts go to the DESTINATION pane: splitting a huge one where \
         it already is usually does not fit"
    );
}

/// A size that makes no sense is refused and splits nothing. Zero belongs
/// here: zero-byte parts never finish.
#[tokio::test]
async fn splitting_refuses_a_size_that_is_not_valid() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;

    run_by_palette(&h, &mut sub, "pane.split-file").await;
    let id = next_dialogs(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "0".to_owned(),
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
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-split-bad-size"),
        "{ack:?}"
    );
    settle().await;
    assert!(backend.split.lock().expect("partidos").is_empty());
}

/// **Joining only from the FIRST part** (#132, #290): starting from `.007`
/// would join half a thing, and the core only looks forward.
#[tokio::test]
async fn joining_requires_starting_with_the_first_chunk() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"pelicula.mkv.001".to_vec(), false),
            (b"pelicula.mkv.007".to_vec(), false),
        ],
    );
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // The cursor starts on the first row: the `.001`.
    run_by_palette(&h, &mut sub, "pane.combine-files").await;
    {
        let js = annotated(&backend, "the join queued", 1, |f| {
            f.joined.lock().expect("juntados").clone()
        })
        .await;
        assert_eq!(js.len(), 1, "from the .001 it does");
        assert_eq!(
            js[0].dest.to_wire(),
            "mem:///casa/pelicula.mkv",
            "the destination is the name WITHOUT the part suffix"
        );
    }

    // Move down to `.007` and ask for it again: not from there.
    h.dispatch(press("Down")).await.expect("host alive");
    let ack = execute_via_palette_ack(&h, &mut sub, "pane.combine-files").await;
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-combine-needs-first"),
        "not from another part: {ack:?}"
    );
    assert_eq!(
        backend.joined.lock().expect("juntados").len(),
        1,
        "and nothing new is requested"
    );
}

/// **Packing takes the FORMAT from the typed name** (#132, #290), and the
/// base is the pane's directory: whoever unpacks expects to see what was on
/// screen, not absolute paths.
#[tokio::test]
async fn packaging_derives_the_format_from_the_name() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.pack").await;
    let id = next_dialogs(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "cosas.tar.gz".to_owned(),
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
    let ps = annotated(&backend, "the archiving queued", 1, |f| {
        f.packed.lock().expect("empaquetados").clone()
    })
    .await;
    assert_eq!(ps.len(), 1, "un gesto, una task");
    assert_eq!(
        ps[0].format,
        norte_proto::methods::ArchiveFormat::TarGz,
        "`.tar.gz` no es `.tar`: el sufijo compuesto se mira ANTES"
    );
    assert_eq!(ps[0].dest.to_wire(), "mem:///casa/cosas.tar.gz");
    assert_eq!(
        ps[0].base.to_wire(),
        "mem:///casa",
        "la base es el directorio del panel"
    );
}

/// A name whose format is NOT known how to write is refused, instead of
/// archiving into something else. `.rar` is the real case: it is read by
/// delegation and not written.
#[tokio::test]
async fn packaging_refuses_a_format_that_cannot_be_written() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.pack").await;
    let id = next_dialogs(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "cosas.rar".to_owned(),
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
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-pack-unknown-format"),
        "{ack:?}"
    );
    settle().await;
    assert!(
        backend.packed.lock().expect("empaquetados").is_empty(),
        "and nothing gets archived"
    );
}

/// Testing only applies to a CONTAINER, and it is decided by the same
/// function `Enter` uses to enter one: two extension tables would be two
/// places where one gets forgotten.
#[tokio::test]
async fn checking_a_file_requires_it_to_be_one() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // The cursor starts on `docs/`, which is a directory.
    let ack = execute_via_palette_ack(&h, &mut sub, "pane.test-archive").await;
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-unpack-not-archive"),
        "a directory is not a container: {ack:?}"
    );
    assert!(
        backend.checked.lock().expect("comprobados").is_empty(),
        "and nothing gets asked to be tested"
    );
}

/// A slot that is waiting says WHERE it is going.
///
/// The body keeps showing the PREVIOUS listing until the new one arrives —on
/// purpose: if the connection fails, the reader stays where they were — and
/// without the destination that mix cannot be read. The window was only
/// setting `aria-busy="true"`, with not one rule painting it: against a slow
/// SFTP it gave no signal at all.
#[tokio::test]
async fn a_waiting_slot_says_where_it_is_going() {
    let mut f = Fake::default();
    f.tree.clone_from(&fake_tree().tree);
    // With a delay: without it, the listing lands before the state can be
    // looked at, and the test would be checking the later `Ready`.
    f.delay_ms = 50;
    let (h, snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();
    let b = listing_of(&snap, 1);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");

    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    let state = snapshot_until(&h, &mut sub, "the waiting slot", |s| {
        match &listing_of(s, 1).state {
            norte_ui_host::dto::SlotState::Loading { target_display, .. }
                if !target_display.is_empty() =>
            {
                Some(target_display.clone())
            }
            _ => None,
        }
    })
    .await;
    assert!(
        state.ends_with("/casa/docs"),
        "it says where it is going, not where it is: {state}"
    );
}

/// The window paints in ITS OWN language, not the process's.
///
/// `norte-ui-host` was clean — its calls pass `self.lang` — and all the
/// leaks came from SHARED helpers that translated using the global. The
/// worst one was the date: every listing cell came out in the process's
/// language under a header in the host's, and it could not be dodged with
/// configuration because the window ignores `time-format`, so the relative
/// branch is always alive.
///
/// The window's settings come out WHOLE in the host's language.
///
/// Section titles already carried their own, and each option's name and
/// description carried the PROCESS's, so the screen came out split between
/// two languages. Checked from the outside — what crosses the bridge — and
/// not by calling the helper: what got fixed is that the window passes it
/// its `lang`, and a test on the helper would still stay green if it
/// stopped passing it.
#[tokio::test]
async fn settings_come_out_whole_in_the_hosts_language() {
    // The PROCESS in English and the host in Spanish: whatever leaks comes
    // out in English and shows up here.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "app.settings").await;
    let settings =
        snapshot_until(&h, &mut sub, "the settings screen", |s| s.settings.clone()).await;
    let the_rows: Vec<norte_ui_host::dto::SettingRowView> = settings
        .sections
        .iter()
        .filter_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => Some(rows.clone()),
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .flatten()
        .collect();
    assert!(!the_rows.is_empty(), "there are options to show");

    let in_spanish = norte_i18n::t_in(norte_i18n::Lang::Es, "setting-ui-theme-name");
    let in_english = norte_i18n::t_in(norte_i18n::Lang::En, "setting-ui-theme-name");
    assert_ne!(
        in_spanish, in_english,
        "the premise: the key gets translated"
    );
    let row = the_rows
        .iter()
        .find(|r| r.name == in_spanish || r.name == in_english)
        .expect("the option is in the catalogue");
    assert_eq!(
        row.name, in_spanish,
        "the row came out in the PROCESS's language, not the host's"
    );
}

/// With no trash, the delete SAYS so and becomes permanent.
///
/// "⚠ NO trash: this cannot be undone" was terminal-only. The window
/// compensated with a destructive button, which says that response deletes —
/// not that there is no going back. They are two different things, and the
/// second one is what decides whether anyone presses it.
///
/// The answer comes from the slot's capabilities cache, which arrives with
/// the listing: without it, it is assumed there is NO trash, which is the
/// direction where being wrong only costs a scare.
#[tokio::test]
async fn without_a_trash_deletion_warns_there_is_no_going_back() {
    // The third case is the one that loses data: NOT KNOWN. Capabilities
    // arrive behind the listing and on their own schedule, so there is a
    // whole window — and the whole session, if the request fails — with no
    // answer. Treating that as "there is no trash" really deletes in a
    // place that does have one.
    for (trash, warns) in [(Some(true), false), (Some(false), true), (None, false)] {
        let mut f = Fake::default();
        f.tree.clone_from(&fake_tree().tree);
        if let Some(hay) = trash {
            let mut flags = norte_proto::CapabilityFlags::CASE_SENSITIVE;
            if hay {
                flags |= norte_proto::CapabilityFlags::TRASH;
            }
            f.capabilities.insert(
                "mem:///casa".to_owned(),
                norte_proto::Capabilities {
                    flags,
                    max_path: None,
                },
            );
        } else {
            // It does not even answer: the slot is left with no
            // capabilities.
            f.capabilities_error = true;
        }
        let (h, _snap) = host_tree(Arc::new(f)).await;
        let mut sub = h.subscribe();
        settle().await;

        h.dispatch(press("F8")).await.expect("host alive");
        let d = next_dialogs(&mut sub).await;
        let deleted = d.last().expect("the delete dialog");
        let norte_ui_host::dto::DestCheckView::Done { warnings } = &deleted.dest_check else {
            panic!("a delete waits on nobody: {:?}", deleted.dest_check)
        };
        assert_eq!(
            !warnings.is_empty(),
            warns,
            "with papelera={trash:?} the warnings were {warnings:?}"
        );
        if warns {
            assert!(warnings[0].contains('⚠'), "{warnings:?}");
            assert_eq!(
                deleted.title_key, "modal-delete-permanent-title",
                "and the title says so too: with no trash, this is permanent"
            );
        } else {
            assert_eq!(
                deleted.title_key, "modal-delete-title",
                "with trash — or not knowing — this is NOT a permanent delete"
            );
        }
    }
}

/// The collision dialog does NOT lose the badge from an altered name.
///
/// It was masking over `display_lossy()`, which had ALREADY put in the
/// U+FFFD: `display_name` then received flawless UTF-8 and declared the name
/// FAITHFUL. Meaning that on the only screen where OVERWRITING a file gets
/// approved, the name that differs from the bytes was painted as if it did
/// not differ.
#[tokio::test]
async fn the_collision_dialog_marks_the_altered_name() {
    let backend = fake_tree();
    let (h, _snap) = Box::pin(two_panes_with_separate_destination(Arc::clone(&backend))).await;
    let mut sub = h.subscribe();
    // The cursor, on the entry whose name is not UTF-8.
    let snapshot = snapshot(&h, &mut sub).await;
    let b = listing_of(&snapshot, 1);
    let odd = b
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("the tree carries an altered name");
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key: odd.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = next_tasks(&mut sub).await;
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });

    let dialogs = next_dialogs(&mut sub).await;
    let collision = dialogs.last().expect("the collision dialog");
    let dest = collision
        .destination
        .as_ref()
        .expect("says which file it is asking about");
    assert!(
        dest.hostile,
        "what is painted is not the bytes, and this is what gets approved: {dest:?}"
    );
}

/// **A transfer that COLLIDES has a way out** (#274).
///
/// The window always sends `CollisionPolicy::Fail`, the safe default, but
/// had nowhere to make the decision: a failed task was left on the board
/// with no way forward, while the TUI does offer the four. What is checked
/// is what really matters: that the second transfer GOES OUT, with the
/// chosen policy and the same verb.
#[tokio::test]
async fn a_copy_that_collides_can_be_retried_with_another_policy() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = next_tasks(&mut sub).await;

    // The daemon says the destination already exists.
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });

    // And now there IS a question to answer.
    let dialogs = next_dialogs(&mut sub).await;
    let collision = dialogs.last().expect("the collision dialog");
    let options: Vec<&str> = collision.choices.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        options,
        vec!["overwrite", "newer", "rename", "skip", "cancel"],
        "the TUI's four ways out, plus cancel"
    );
    assert!(
        collision
            .choices
            .iter()
            .any(|c| c.id == "overwrite" && c.destructive),
        "overwrite is marked destructive: it destroys what is there"
    );

    // It opened on its own, so the first response only acknowledges it.
    let cid = collision.id;
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "overwrite".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "overwrite".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = annotated(&backend, "the original and the retry", 2, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 2, "the original and the retry: {ts:?}");
    let (from, to, mover, collision) = &ts[1];
    assert_eq!(
        *collision,
        norte_proto::CollisionPolicy::Overwrite,
        "the retry goes with the chosen policy"
    );
    assert!(!mover, "and with the SAME verb: a copy retry does not move");
    assert_eq!(from.to_wire(), ts[0].0.to_wire(), "same source");
    assert_eq!(to.to_wire(), ts[0].1.to_wire(), "and same destination");
}

/// Cancelling the collision relaunches nothing: not choosing is a response,
/// and the failed task stays as it was.
#[tokio::test]
async fn cancelling_a_collision_does_not_retry() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = next_tasks(&mut sub).await;
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            },
        };
    });
    let cid = next_dialogs(&mut sub)
        .await
        .last()
        .expect("there is one")
        .id;

    // Cancel is EXEMPT from acknowledgment: shrugging off something one
    // never asked for goes through on the first try.
    h.dispatch(UiAction::Dialog {
        id: cid,
        choice: "cancel".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    annotated(&backend, "the original transfer", 1, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;
    settle().await;
    assert_eq!(
        backend.transfers.lock().expect("transferencias").len(),
        1,
        "cancel does not relaunch"
    );
}

/// Confirmed, the copy goes out with the destination COMPOSED in Rust: the
/// destination slot's directory plus the entry's name, byte for byte.
#[tokio::test]
async fn copying_composes_the_destination_in_rust() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = annotated(&backend, "the transfer queued", 1, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 1, "one entry under the cursor, one task");
    let (from, to, mover, collision) = &ts[0];
    assert_eq!(
        *collision,
        norte_proto::CollisionPolicy::Fail,
        "the wire's SAFE default: a busy destination fails, it is not overwritten"
    );
    assert!(!mover, "F5 copies");
    assert_eq!(from.to_wire(), "mem:///casa/notas.txt");
    assert_eq!(
        to.to_wire(),
        "mem:///casa/docs/notas.txt",
        "the destination is the other slot's DIRECTORY plus the source's name"
    );
}

/// F6 uses the same path, but it is a different verb: on the wire it is two
/// methods, on the board two task classes, and in the journal two entries.
#[tokio::test]
async fn move_is_a_different_verb_and_says_so() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F6")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs[0].title_key, "modal-move-title");
    let id = dialogs[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = annotated(&backend, "the move queued", 1, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;
    assert!(ts[0].2, "F6 moves");
}

/// With a single listing there is nowhere to copy to **inside the window**,
/// so it is asked outside (#284) — and until the answer arrives nothing
/// gets transferred. What this test asserts is that a destination is not
/// MADE UP: not the directory itself, not the last one used.
///
/// Before #284 this was refused with `host-no-other-slot`. The full chain —
/// effect, response and confirmation — is covered by
/// `with_one_pane_the_desktop_chooses_the_destination`.
#[tokio::test]
async fn with_no_other_slot_the_destination_is_asked_outside() {
    let backend = fake_tree();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "simple", (120, 40)).await;
    let mut native = h.native_effects();
    let ack = h.dispatch(press("F5")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the gesture is accepted and it asks: {ack:?}"
    );
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("the picker comes out")
        .expect("channel alive");
    assert!(matches!(
        effect,
        norte_ui_host::dto::NativeEffect::PickDirectory { .. }
    ));
    assert!(
        backend.transfers.lock().expect("transferencias").is_empty(),
        "and nothing moves until there is a destination"
    );
}

/// Both listings in the SAME directory: copying there is copying onto
/// itself, and no dialog opens suggesting it.
#[tokio::test]
async fn copying_onto_its_own_directory_is_rejected() {
    let backend = fake_tree();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let ack = h.dispatch(press("F5")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "the destination is the source directory: {ack:?}"
    );
    assert!(backend.transfers.lock().expect("transferencias").is_empty());
}

/// In read-only, F5 opens nothing: the key existing in the preset is not
/// permission.
#[tokio::test]
async fn in_read_only_copying_opens_nothing() {
    let backend = fake_tree();
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let ack = h.dispatch(press("F5")).await.expect("host alive");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    settle().await;
    assert!(backend.transfers.lock().expect("transferencias").is_empty());
}

/// The marks are CONSUMED by sending, like in the TUI: a half-consumed
/// selection would mean different things depending on which task finished.
#[tokio::test]
async fn marks_are_consumed_on_send() {
    let backend = fake_tree();
    let (h, snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let b1 = listing_of(&snap, 1);
    let (generation, keys): (u64, Vec<_>) = (
        b1.generation,
        b1.rows
            .iter()
            .filter(|r| r.display_name != "docs")
            .map(|r| r.key)
            .collect(),
    );
    let mut sub = h.subscribe();
    for key in keys.iter().take(2) {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: *key,
            generation,
        })
        .await
        .expect("host alive");
    }
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    annotated(&backend, "the two tasks from the two marks", 2, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(
        backend.transfers.lock().expect("transferencias").len(),
        2,
        "two marks, two tasks"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(
        listing_of(&snapshot, 1).marks,
        0,
        "sending consumed the marks"
    );
}

/// When the copy finishes, the DESTINATION slot is re-listed: the new entry
/// is there and a screen that does not show it lies.
#[tokio::test]
async fn finishing_a_copy_relists_the_destination() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    next_tasks(&mut sub).await;
    let before = backend.listings();

    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    until(&backend, "el relistado del destino tras la copia", |f| {
        (f.listings() > before).then_some(())
    })
    .await;
}

/// A collision is not an exception from the host: it is the task's TYPED
/// outcome, and it reaches the board as such.
#[tokio::test]
async fn a_collision_arrives_at_the_board_as_a_failure() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"notas.txt".to_vec(), false)]);
    f.state_transfer = Some(norte_proto::TaskState::Failed {
        error: norte_proto::Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        },
    });
    let backend = Arc::new(f);
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let tasks = next_tasks(&mut sub).await;
    assert_eq!(
        tasks[0].state,
        norte_ui_host::dto::TaskStateView::Failed,
        "the destination already existed, and the board says so"
    );
}

/// A task that IS BORN terminal — the daemon completed it before the call
/// returned — also re-lists the destination.
///
/// It is the real race: the progress channel never changes, so nobody ever
/// looks at it, and without checking the state ON REGISTERING the copy was
/// left done on disk and absent on screen forever.
#[tokio::test]
async fn a_copy_born_terminal_also_relists() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.state_transfer = Some(norte_proto::TaskState::Completed);
    let backend = Arc::new(f);
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    let before = backend.listings();
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    until(&backend, "the refresh from an already-finished copy", |f| {
        (f.listings() > before).then_some(())
    })
    .await;
}

/// A destination that does not accept writes refuses ON QUEUEING, before
/// there is a task: there is no row on the board to look at, so the bar
/// says it — with the error's typed phrase, not with a "something failed".
#[tokio::test]
async fn a_read_only_destination_says_so_when_queuing() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.transfer_rejected = Some(norte_proto::Error::Unsupported);
    let backend = Arc::new(f);
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snapshot = next_snapshot(&mut sub).await;
        if let Some(m) = &snapshot.status.message {
            assert!(
                !m.starts_with("err-"),
                "the bar says the TRANSLATED error, not its key: {m}"
            );
            assert!(snapshot.tasks.is_empty(), "no task ever came to be");
            return;
        }
    }
    panic!("a queueing rejection got lost in silence");
}

/// A transfer in progress is cancelled through the same path as any other
/// task: there is only one board.
#[tokio::test]
async fn a_copy_in_progress_can_be_canceled() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let tasks = next_tasks(&mut sub).await;
    assert_eq!(tasks.len(), 1);
    let ack = h
        .dispatch(UiAction::CancelTask {
            task_id: tasks[0].task_id,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(backend.cancellations.load(Ordering::SeqCst), 1);
}

/// A refresh NEVER steps on a navigation in flight.
///
/// The refresh reserves a new token, so the navigation's response would
/// arrive with an old one and be discarded: the pane would be left in the
/// directory the reader had just left, without saying anything. A slightly
/// stale screen is acceptable; the application moving on its own is not.
#[tokio::test]
async fn a_refresh_does_not_stomp_on_an_in_flight_navigation() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.put("mem:///casa/docs/hondo", vec![(b"z.md".to_vec(), false)]);
    f.tree
        .get_mut("mem:///casa/docs")
        .expect("is there")
        .push((b"hondo".to_vec(), true));
    // The listing's response TAKES A WHILE: that is what opens the window
    // where the refresh could sneak in.
    f.delay_ms = 120;
    f.state_transfer = Some(norte_proto::TaskState::Completed);
    let backend = Arc::new(f);
    let (h, snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b2 = listing_of(&snap, 2);
    let hondo = b2
        .rows
        .iter()
        .find(|r| r.display_name == "hondo")
        .expect("the subdirectory is there");
    let (key, generation) = (hondo.key, b2.generation);

    // A copy toward `home/docs`, which finishes as soon as it is queued.
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    // And, BEFORE confirming, the destination navigates elsewhere: the
    // navigation is left in flight during the fake's 120 ms.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    let before = backend.listings();
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    // The navigation reaches its destination and NOBODY sends it back to
    // `home/docs`. It waits for `hondo`'s listing to have been REQUESTED and
    // for no response to be left in flight — including the refresh the
    // finished copy triggers, which is the one that could step on it —
    // only then is the screen looked at, ONCE.
    until(&backend, "hondo's listing, already served", |f| {
        (f.listings() > before && f.en_calma()).then_some(())
    })
    .await;
    settle().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(
        listing_of(&snapshot, 2)
            .path_display
            .ends_with("/docs/hondo"),
        "the navigation survived the refresh: {}",
        listing_of(&snapshot, 2).path_display
    );
}

// ---------------------------------------------------------------------------
// What the three 5.1 reviews found.
// ---------------------------------------------------------------------------

/// A corpus name, by its id.
pub(super) fn hostile(id: &str) -> Vec<u8> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("the corpus has {id}"))
        .bytes
}

/// A destination directory named `a → mem_b.txt` CANNOT simulate two paths
/// in the confirmation.
///
/// The arrow is legitimate (U+2192), it is not a terminal hazard and is
/// therefore neither masked nor marked. With the destination as the body's
/// first line and an arrow as its label, whoever reads `→ …/a → mem_b.txt`
/// could understand their files are going to `mem_b.txt`. It is labeled OUT
/// OF BAND: the destination has its own field. Fixture `arrow_join_spoof`
/// from the canonical corpus.
#[tokio::test]
async fn a_destination_with_one_arrow_does_not_fake_two_paths() {
    let trap = hostile("arrow_join_spoof");
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(trap.clone(), true), (b"notas.txt".to_vec(), false)],
    );
    let vp = norte_proto::VPath::parse("mem:///casa")
        .expect("root")
        .join(norte_proto::Segment::new(trap.clone()).expect("segment"));
    f.put(vp.to_wire().as_str(), vec![(b"a.md".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // The destination slot enters the trap directory.
    let b2 = listing_of(&snap, 2);
    let row = b2
        .rows
        .iter()
        .find(|r| r.display_name.contains('→'))
        .expect("the trap is painted");
    let (key, generation) = (row.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    let mut after = next_snapshot(&mut sub).await;
    while listing_of(&after, 2).path_display == listing_of(&snap, 2).path_display {
        after = next_snapshot(&mut sub).await;
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    // The SOURCE's cursor, on the file: the trap directory is also listed
    // here, and what is checked is the destination.
    let b1 = listing_of(&after, 1);
    let notes = b1
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("the file is there");
    let (key, generation) = (notes.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");

    h.dispatch(press("F5")).await.expect("host alive");
    let d = next_dialogs(&mut sub).await[0].clone();
    let dest = d.destination.expect("says where it is going");
    assert!(
        dest.text.contains('\u{2192}'),
        "the real name carries the arrow: {dest:?}"
    );
    assert_eq!(d.body.len(), 1, "one entry, one line: {:?}", d.body);
    assert!(
        d.body[0].text.ends_with("/casa/notas.txt") && !d.body[0].text.contains('\u{2192}'),
        "the body is ONLY the source; the destination does not appear there: {:?}",
        d.body
    );
}

/// A batch bigger than what fits in the dialog SAYS so.
///
/// Marking forty, seeing sixteen and confirming is approving something
/// else: this is the last screen where it can still be said no.
#[tokio::test]
async fn a_trimmed_batch_says_so() {
    let mut names: Vec<(Vec<u8>, bool)> =
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)];
    names.extend((0..40).map(|i| (format!("f{i:03}.txt").into_bytes(), false)));
    let mut f = Fake::default();
    f.put("mem:///casa", names);
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;

    let b1 = listing_of(&snap, 1);
    let generation = b1.generation;
    let keys: Vec<_> = b1
        .rows
        .iter()
        .filter(|r| r.display_name != "docs")
        .map(|r| r.key)
        .collect();
    assert!(
        keys.len() > 16,
        "there are more than what fits: {}",
        keys.len()
    );
    for key in &keys {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: *key,
            generation,
        })
        .await
        .expect("host alive");
    }
    // Listening starts AFTER marking: forty marks are forty patches, and the
    // helper waiting for a dialog only looks at the first updates that
    // reach it.
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let d = next_dialogs(&mut sub).await[0].clone();
    assert!(
        d.body.len() < keys.len(),
        "the body is capped: {}",
        d.body.len()
    );
    assert!(
        !d.overflow_note.is_empty(),
        "and it SAYS so, in its own field: {d:?}"
    );
    assert!(
        !d.overflow_note.starts_with("dialog-"),
        "translated, not the Fluent key: {}",
        d.overflow_note
    );
}

/// A confirmation's body SAYS which line is painted differently from what
/// it is. It is the only surface where a foreign name gets approved.
#[tokio::test]
async fn the_body_of_a_confirmation_marks_what_it_masks() {
    for id in ["control_newline", "control_escape", "arrow_join_spoof"] {
        let bytes = hostile(id);
        let altera = norte_frontend::display_name(&bytes).1;
        let mut f = Fake::default();
        f.put(
            "mem:///casa",
            vec![(b"docs".to_vec(), true), (bytes.clone(), false)],
        );
        f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
        let backend = Arc::new(f);
        let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
        let mut sub = h.subscribe();
        // The cursor, on the hostile entry.
        let b1 = listing_of(&snap, 1);
        let row = b1
            .rows
            .iter()
            .find(|r| r.display_name != "docs")
            .expect("is there");
        let (key, generation) = (row.key, b1.generation);
        h.dispatch(UiAction::SelectRow {
            slot_id: 1,
            key,
            generation,
        })
        .await
        .expect("host alive");
        // F8 is enough: the delete's body and the transfer's are built with
        // the SAME function.
        h.dispatch(press("F8")).await.expect("host alive");
        let d = next_dialogs(&mut sub).await[0].clone();
        for l in &d.body {
            harmless(&l.text, id, "a line from a dialog's body");
        }
        assert_eq!(
            d.body.iter().any(|l| l.hostile),
            altera,
            "[{id}] the body's mark says exactly what `display_name` says: {:?}",
            d.body
        );
    }
}

/// The destination is composed from the source's BYTES, even when they are
/// not UTF-8. The path that crosses the wire never went through the screen.
#[tokio::test]
async fn the_destination_is_composed_byte_by_byte() {
    let backend = fake_tree();
    let (h, snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // `caf\xC3(`: not UTF-8, and on screen it carries a U+FFFD.
    let b1 = listing_of(&snap, 1);
    let row = b1
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("the tree carries a name that is not UTF-8");
    let (key, generation) = (row.key, b1.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let ts = annotated(&backend, "the transfer queued", 1, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;
    let (from, to, _, _) = &ts[0];
    assert!(
        from.to_wire().starts_with("mem:///casa/caf"),
        "the source is the entry that is not UTF-8: {}",
        from.to_wire()
    );
    let name = from
        .to_wire()
        .strip_prefix("mem:///casa/")
        .expect("hangs off casa")
        .to_owned();
    assert_eq!(
        to.to_wire(),
        format!("mem:///casa/docs/{name}"),
        "the name's bytes arrive intact"
    );
    assert!(
        !to.to_wire().contains("%EF%BF%BD"),
        "and without the U+FFFD the screen paints: {}",
        to.to_wire()
    );
}

/// Moving ALSO re-lists the source pane: entries disappear from it.
#[tokio::test]
async fn moving_also_relists_the_source() {
    let backend = fake_tree();
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F6")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    next_tasks(&mut sub).await;
    let before = backend.listings();
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    // BOTH: the source (`home`, where it leaves from) and the destination
    // (`home/docs`, where it arrives).
    until(&backend, "both panes re-listed", |f| {
        (f.listings() >= before + 2).then_some(())
    })
    .await;
}

/// A refresh keeps the cursor BY PATH, not by index.
///
/// Per-directory memory stores an index, and an index does not survive the
/// operation removing an entry: whoever was looking at one file would find
/// the cursor on another without having touched a key, and the next key
/// could be F8.
#[tokio::test]
async fn the_cursor_survives_a_refresh() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"c.txt".to_vec(), false),
            (b"d.txt".to_vec(), false),
        ],
    );
    // Delete REMOVES the entry: without that the listing that arrives is
    // identical and the cursor's index keeps naming the same file by
    // accident — a green test proving nothing.
    f.delete_for_real = true;
    let backend = Arc::new(f);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listing(&snap);
    // Neither the first nor the LAST one: on the last, deleting an entry
    // ahead of it leaves the old, trimmed index landing right on the same
    // file, and the test would pass with no anchor by pure coincidence.
    let middle = &b.rows[2];
    let (key, generation, name) = (middle.key, b.generation, middle.display_name.clone());
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");

    // The FIRST one is marked and deleted, not the cursor's: the listing
    // arrives with one entry fewer AHEAD of it, so the old index points at a
    // different file while the path stays the same.
    let first = b.rows.first().expect("there are rows").key;
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: first,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(press("F8")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    next_tasks(&mut sub).await;
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snapshot = next_snapshot(&mut sub).await;
        let b = listing(&snapshot);
        if b.generation == generation {
            continue;
        }
        let below = b
            .cursor
            .and_then(|k| b.rows.iter().find(|r| r.key == k))
            .map(|r| r.display_name.clone());
        assert_eq!(
            below.as_deref(),
            Some(name.as_str()),
            "the cursor is still on the SAME file after the refresh"
        );
        return;
    }
    panic!("the refresh never arrived");
}

/// With THREE listings, a hand-designated destination SURVIVES a focus
/// change, and with none designated, none is guessed.
///
/// The host was reassigning the role on every `FocusSlot` with its own rule
/// — "the first one that is not active" — overriding what a person had
/// chosen and only tie-breaking when there were several candidates. While
/// the destination was decoration that looked odd; since copy and move read
/// it, it means sending files to a place nobody chose. The rule is the
/// shared one (ADR 0058 D7), and with several candidates and none chosen
/// the role is left UNSET.
#[tokio::test]
async fn with_three_listings_the_destination_is_not_guessed() {
    use norte_ui_host::dto::SlotRole;
    const THREE: &str = r#"
[split]
dir = "vertical"
sizes = [{ weight = 1 }, { fixed = 1 }]

[[split.children]]
[split.children.split]
dir = "horizontal"
sizes = [{ weight = 1 }, { weight = 1 }, { weight = 1 }]

[[split.children.split.children]]
[split.children.split.children.slot]
id = 1
kind = "browser"

[[split.children.split.children]]
[split.children.split.children.slot]
id = 2
kind = "browser"

[[split.children.split.children]]
[split.children.split.children.slot]
id = 3
kind = "browser"

[[split.children]]
[split.children.slot]
id = 4
kind = "status"
"#;
    let tree_layout: norte_frontend::layout::Node =
        toml::from_str(THREE).expect("the layout parses");
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: tree_layout,
        viewport: (200, 60),
        settings: test_settings(),
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
    let rol = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    let _ = &snap;
    let mut sub = h.subscribe();

    // With none designated: F5 does not guess, it ASKS that one be chosen.
    let ack = h.dispatch(press("F5")).await.expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-no-target-designated".to_owned()
        },
        "with three panes the destination is chosen, not tie-broken: {ack:?}"
    );

    // No preset binds `layout.set-target`, so it is run through the
    // PALETTE — the catalogue's other door, and it works just as well.
    let mut placed = false;
    for _ in 0..4 {
        h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "p".to_owned(),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host alive");
        for c in ["s", "e", "t", "-", "t", "a", "r", "g", "e", "t"] {
            h.dispatch(key_for(c)).await.expect("host alive");
        }
        h.dispatch(press("Enter")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snapshot = next_snapshot(&mut sub).await;
        if rol(&snapshot.layout, 3) == Some(SlotRole::Target) {
            placed = true;
            break;
        }
    }
    assert!(placed, "se puede designar el tercero");

    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let after = next_snapshot(&mut sub).await;
    assert_eq!(
        rol(&after.layout, 3),
        Some(SlotRole::Target),
        "el destino ELEGIDO sobrevive al cambio de foco"
    );
}

/// The marks sending consumes are the SOURCE slot's, even if focus moved to
/// another one between the question and the answer.
///
/// `FocusSlot` is not barred while a dialog is open: only keys are. A click
/// on the other pane was erasing the foreign pane's marks and leaving the
/// ones that had just been sent intact.
#[tokio::test]
async fn the_marks_consumed_are_the_sources() {
    let backend = fake_tree();
    let (h, snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let b1 = listing_of(&snap, 1);
    let generation = b1.generation;
    let keys: Vec<_> = b1
        .rows
        .iter()
        .filter(|r| r.display_name != "docs")
        .map(|r| r.key)
        .collect();
    for key in &keys {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: *key,
            generation,
        })
        .await
        .expect("host alive");
    }
    let mut sub = h.subscribe();
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;

    // And NOW focus moves to the other pane, without closing the dialog.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    annotated(&backend, "the transfer that consumes the marks", 1, |f| {
        f.transfers.lock().expect("transferencias").clone()
    })
    .await;

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(
        listing_of(&snapshot, 1).marks,
        0,
        "the consumed marks are the slot's that sent them"
    );
}

/// A HIDDEN slot on the affected directory is not listed — what is not seen
/// is not fetched — but it is marked to reload as soon as it comes back.
///
/// Without this, a background tab on the destination directory showed a
/// listing from before the operation until someone navigated by hand, and a
/// key on one of its rows acted against that stale listing.
#[tokio::test]
async fn an_affected_hidden_slot_is_left_to_reload() {
    let backend = fake_tree();
    // Born WIDE, so the second listing really gets listed and ends up
    // `Ready`. If it were born hidden it would be `Loading` from the start
    // and would reload on return for that reason, not this one.
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (160, 40)).await;
    assert!(
        snap.slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count()
            >= 2,
        "both listings are visible"
    );
    let mut sub = h.subscribe();
    // And now it narrows until only one fits.
    h.dispatch(UiAction::SetViewport {
        width: 30,
        height: 10,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let narrow = next_snapshot(&mut sub).await;
    assert_eq!(
        narrow
            .slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count(),
        1,
        "with 30 columns only one listing fits"
    );
    h.dispatch(press("F8")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    next_tasks(&mut sub).await;
    let listings_before = backend.listings();
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    // The task's outcome arrives over its own channel: it is let run before
    // widening, so the hidden slot is already marked.
    settle().await;

    // The window widens: the slot that was hidden comes back, and since it
    // was left marked as LOADING, it gets listed.
    h.dispatch(UiAction::SetViewport {
        width: 160,
        height: 40,
    })
    .await
    .expect("host alive");
    until(&backend, "the re-listing of the slot that came back", |f| {
        (f.listings() > listings_before + 1).then_some(())
    })
    .await;
}

/// In read-only, the PALETTE also does not offer what mutates.
///
/// It was the only door that did not go through the effective keymap: it
/// was offering copy, move and delete, and the execution guard was
/// rejecting them. Offering what is going to be refused is promising
/// something that will not happen.
#[tokio::test]
async fn in_read_only_the_palette_does_not_offer_what_mutates() {
    let (h, _snap) = host_solo_read(fake_tree()).await;
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
    let solo_read =
        norte_ui_host::commands::implemented(norte_ui_host::commands::Effects::SoloRead);
    let no_inertes = norte_ui_host::commands::IMPLEMENTED
        .iter()
        .filter(|c| !solo_read.contains(c));
    for cmd in no_inertes {
        assert!(
            !p.rows.iter().any(|r| r.text == *cmd),
            "a read-only window's palette offers {cmd}"
        );
    }
}

/// And the key says so with ITS OWN reason, not just any one.
#[tokio::test]
async fn in_read_only_copy_says_why() {
    let (h, _snap) = host_solo_read(fake_tree()).await;
    let ack = h.dispatch(press("F5")).await.expect("host alive");
    let ActionAck::Unavailable { reason_key } = ack else {
        panic!("expected unavailable: {ack:?}");
    };
    assert!(
        reason_key == "cmd-not-here" || reason_key == "host-read-only",
        "and with a reason from the vocabulary, not a made-up one: {reason_key}"
    );
}

/// A refresh keeps the MARKS, by path.
///
/// `set_listing` clears them because the rows are different — correct for a
/// `cd`, and a punishment for whoever did not move: a copy's DESTINATION
/// pane gets re-listed when the copy finishes, and it was sweeping away a
/// selection its owner had made by hand and that nobody had sent.
#[tokio::test]
async fn marks_survive_a_refresh() {
    let backend = fake_tree();
    let (h, snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Marks on the DESTINATION pane: they are not the ones the copy
    // consumes, so the only thing that can remove them is the re-listing.
    let b2 = listing_of(&snap, 2);
    let generation2 = b2.generation;
    let keys: Vec<_> = b2.rows.iter().map(|r| r.key).collect();
    assert!(!keys.is_empty(), "the destination has rows");
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    for key in &keys {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 2,
            key: *key,
            generation: generation2,
        })
        .await
        .expect("host alive");
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");

    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    next_tasks(&mut sub).await;
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snapshot = next_snapshot(&mut sub).await;
        let b = listing_of(&snapshot, 2);
        if b.generation == generation2 {
            continue;
        }
        assert_eq!(
            b.marks,
            keys.len() as u64,
            "re-listing the destination does not sweep away what its owner \
             had marked"
        );
        return;
    }
    panic!("the destination's refresh never arrived");
}

/// A move re-lists the SOURCE pane even if the provider writes its entries'
/// parent with a DIFFERENT spelling of the same directory.
///
/// An entry's parent is written by the provider; the pane's directory can
/// come from config, from the session or from a favorite. On macOS (NFD
/// against NFC) and against a case-insensitive server they are two strings
/// for the same place, and byte-for-byte comparison does not join them (ADR
/// 0061).
#[tokio::test]
async fn moving_relists_the_source_even_if_the_provider_spells_it_differently() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    // The provider hangs its entries off `⟨mem⟩/CASA`, not `⟨mem⟩/home`.
    f.padre_different = true;
    let backend = Arc::new(f);
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // The destination, in `docs`.
    let b2 = listing_of(&snap, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("is there");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    let mut after = next_snapshot(&mut sub).await;
    while listing_of(&after, 2).path_display == listing_of(&snap, 2).path_display {
        after = next_snapshot(&mut sub).await;
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");

    h.dispatch(press("F6")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    next_tasks(&mut sub).await;
    let before = backend.listings();
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    // BOTH panes. Without pinning the SOURCE slot's directory, the entry's
    // parent (`⟨mem⟩/CASA`) would not match what the pane shows
    // (`⟨mem⟩/home`) and the source would be left without a re-listing.
    until(&backend, "both panes re-listed", |f| {
        (f.listings() >= before + 2).then_some(())
    })
    .await;
}

// ---------------------------------------------------------------------------
// A BATCH of transfers: its caps and its count (#271).
// ---------------------------------------------------------------------------

/// Marks the active slot's first N rows, one by one.
pub(super) async fn mark_all(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
    slot: u32,
) {
    let snapshot = snapshot(h, sub).await;
    let b = listing_of(&snapshot, slot);
    let (generation, keys): (u64, Vec<_>) = (b.generation, b.rows.iter().map(|r| r.key).collect());
    for key in keys {
        h.dispatch(UiAction::ToggleMark {
            slot_id: slot,
            key,
            generation,
        })
        .await
        .expect("host alive");
    }
}

/// A batch whose queueing rejections are ALL of it: the bar says ONE phrase
/// with the count, not N phrases of which only the last survives (#271,
/// point 3).
#[tokio::test]
async fn a_batchs_rejections_are_reported_only_once() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
        ],
    );
    f.put("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    f.transfer_rejected = Some(norte_proto::Error::Unsupported);
    let backend = Arc::new(f);
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    mark_all(&h, &mut sub, 1).await;
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    // The summary says HOW MANY, meaning the count existed: without it the
    // bar would carry the last error's phrase and nothing else.
    let snapshot = wait_snapshot(&h, &mut sub, "the batch gets summarized", |f| {
        f.status.message.as_deref().is_some_and(|m| m.contains('4'))
    })
    .await;
    let msg = snapshot.status.message.clone().expect("there is a summary");
    assert!(
        msg.contains('4'),
        "the summary does not count the batch: {msg}"
    );
    assert!(snapshot.tasks.is_empty(), "none ever became a task");
}

/// And with the tasks queued: the summary counts the OUTCOMES, and only once
/// the whole batch is resolved (#271, point 2).
#[tokio::test]
async fn the_batch_says_how_many_finished_well_and_how_many_did_not() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (b"a.txt".to_vec(), false),
            (b"b.txt".to_vec(), false),
        ],
    );
    f.put("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    // They are born TERMINAL and well: the path where `progress` is never called.
    f.state_transfer = Some(norte_proto::TaskState::Completed);
    let backend = Arc::new(f);
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    mark_all(&h, &mut sub, 1).await;
    h.dispatch(press("F5")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    let snapshot = wait_snapshot(&h, &mut sub, "el lote se resume", |f| {
        f.status.message.is_some()
    })
    .await;
    let msg = snapshot.status.message.clone().expect("there is a summary");
    assert!(
        msg.contains('4') && msg.contains('0'),
        "the summary says 4 requested and 0 failed: {msg}"
    );
}

/// The batch cap (#271, point 4): `pane.copy` operates on the marks, and
/// marking has no ceiling. Without this cap the batch was queued whole and
/// the limit was discovered halfway, when the daemon started rejecting over
/// `MAX_LIVE_TASKS`: with half of it done and nothing saying where it cut
/// off.
#[tokio::test]
async fn a_batch_above_the_cap_is_rejected_whole() {
    const HOW_MANY: usize = norte_ui_host::MAX_TRANSFER_BATCH + 8;
    let mut f = Fake::default();
    let mut entries: Vec<(Vec<u8>, bool)> =
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)];
    for i in 0..HOW_MANY {
        entries.push((format!("f{i:04}.txt").into_bytes(), false));
    }
    f.put("mem:///casa", entries);
    f.put("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // The destination, by hand and not with the two-pane helper: with a
    // listing this size the drain MOVES the generation, and an `Activate`
    // with the startup one arrives stale. It waits for the listing to be
    // whole and reads THAT snapshot's generation.
    let settled = wait_snapshot(&h, &mut sub, "the drain finishes", |f| {
        listing_of(f, 2).total_rows.unwrap_or(0) >= HOW_MANY as u64 + 2
    })
    .await;
    let b2 = listing_of(&settled, 2);
    let docs = b2
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let (key, generation) = (docs.key, b2.generation);
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Activate {
        slot_id: 2,
        key,
        generation,
    })
    .await
    .expect("host alive");
    wait_snapshot(&h, &mut sub, "the destination lands in /casa/docs", |f| {
        listing_of(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    // And the source fully loaded: `invert_marks` marks what is LOADED, and
    // with the drain halfway it would mark a hundred and the cap would not
    // be touched.
    wait_snapshot(&h, &mut sub, "the source is whole", |f| {
        listing_of(f, 1).total_rows.unwrap_or(0) >= HOW_MANY as u64 + 2
    })
    .await;
    run_by_palette(&h, &mut sub, "mark.invert").await;
    let ack = h.dispatch(press("F5")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-batch-too-large"),
        "{ack:?}"
    );
    let snapshot = snapshot(&h, &mut sub).await;
    assert!(
        snapshot.dialogs.is_empty(),
        "no dialog opens promising something that will not happen"
    );
}

/// The canonical corpus against the APPROVAL dialog (#277).
///
/// It is the surface where lying costs the most: what is read there is the
/// only thing a human has to decide whether an agent deletes their files.
///
/// The paths arrive from the daemon as text already redacted, not as
/// `VPath`, so the test passes them first through `display_lossy` — which is
/// what `norte_core::engine::span_path` does, and cannot be called from here
/// because the host does not depend on the core (ADR 0066). That step IS
/// what makes the test mean something: feeding it raw bytes would light the
/// flag through a path that does not happen in production.
#[tokio::test]
async fn the_hostile_corpus_crosses_the_approval_dialog() {
    // The four the daemon's lossy conversion ALTERS, and `zwsp_twin` as
    // CONTRAST: the lossy conversion does not touch that one — it is valid
    // UTF-8 — and its flag has to turn on through the other path, the
    // masking one.
    let cases = [
        "lossy_collapse_ff",
        "lossy_collapse_fe",
        "rtl_override",
        "control_escape",
        "zwsp_twin",
    ];
    let corpus = norte_testkit::corpus::hostile_names();
    for id in cases {
        let n = corpus
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} is in the corpus"));
        let p = norte_proto::VPath::parse("mem:///casa")
            .expect("root")
            .join(norte_proto::Segment::new(n.bytes.clone()).expect("segment"));
        // The daemon's step: `span_path` is this for any authority with no
        // userinfo, which is the case for a `mem://`.
        let redacted = p.display_lossy().clone();

        let fake = tree_as_fake();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *fake.approvals.lock().expect("aprobaciones") = Some(rx);
        let (host, _snap) = host_tree(Arc::new(fake)).await;
        let mut sub = host.subscribe();
        tx.send(norte_proto::methods::PolicyApprovalRequired {
            approval_id: 7,
            session: Some("agente-1".to_owned()),
            op: "delete".to_owned(),
            paths: vec![redacted.clone()],
            paths_total: 1,
            ttl_ms: 30_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        })
        .expect("the host is listening");

        let dialogs = next_dialogs(&mut sub).await;
        let d = &dialogs[0];
        let line = d.body.first().expect("the path is there");
        harmless(&line.text, id, "a path from the approval dialog");
        assert!(
            line.hostile,
            "[{id}] the line is painted differently from what there is and does NOT say so: {:?}",
            line.text
        );
        // And the deadline goes in ITS OWN field, never among the paths:
        // among them a file name could impersonate it (`approval_ttl_line_spoof`).
        assert!(
            d.deadline.is_some(),
            "[{id}] the deadline has to have its own field"
        );
        assert!(
            d.body.iter().all(|l| Some(&l.text) != d.deadline.as_ref()),
            "[{id}] the deadline snuck in among the paths: {:?}",
            d.body
        );
    }
}

/// A broken plugin's directory is BYTES, and it was arriving already
/// converted (#265).
///
/// `PluginLoadError.dir` is a `String` the core was producing with a
/// `to_string_lossy` with NO mark, so a directory named `caf\xff` — fixture
/// `lossy_collapse_ff` from the corpus — crossed the wire already carrying
/// its `U+FFFD`. And `display_name` cannot recover it: it puts `lossy` only
/// when `from_utf8` fails and `masked` only in the face of a terminal
/// hazard, and `U+FFFD` is neither of those two things — it is Specials.
/// The row declared itself faithful.
///
/// The test is a PAIR, because a single row does not tell the fix apart
/// from the heuristic that came before:
///
/// - `caf\xff` (real non-UTF-8 bytes) → the row gets MARKED. The old
///   heuristic also marked it, so this half alone proves nothing.
/// - `caf\u{FFFD}` (a directory actually NAMED that, in valid UTF-8) → the
///   row is NOT marked. It is the heuristic's false positive — "the string
///   carries a replacement, so someone converted it" — and it is the half
///   that only passes with the bytes on hand.
///
/// What this fix does NOT do: tell `caf\xff` apart from `caf\xfe` when
/// painting. `display_name` maps every invalid byte to the same `U+FFFD`,
/// so the two keep being painted the same. What is recovered is the MARK,
/// not the spelling.
#[tokio::test]
async fn a_non_utf8_plugin_directory_arrives_flagged_and_without_a_false_positive() {
    let raw = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "lossy_collapse_ff")
        .expect("the fixture is there")
        .bytes;
    assert!(
        std::str::from_utf8(&raw).is_err(),
        "the premise: they are bytes that are NOT UTF-8"
    );
    // And the legitimate twin: a name that IS `U+FFFD` on disk, in valid
    // UTF-8. Nobody converted it, so marking it would be lying.
    let honest = "caf\u{FFFD}".as_bytes().to_vec();
    assert!(std::str::from_utf8(&honest).is_ok());

    let mut backend = tree_with_plugins(Vec::new(), &[]);
    {
        let f = std::sync::Arc::get_mut(&mut backend).expect("single reference");
        for bytes in [&raw, &honest] {
            // What the core sends: the string ALREADY converted, with the
            // bytes alongside. The two rows are told apart by their text;
            // what CANNOT be told apart by the text is which of the two got
            // converted, which is exactly the question.
            let converted = String::from_utf8_lossy(bytes).into_owned();
            f.load_errors
                .push((converted.clone(), "el manifiesto no parsea".to_owned()));
            f.payload_bytes.insert(converted, bytes.clone());
        }
    }
    let (h, _snap) = host_tree(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = next_extensions(&mut sub).await.expect("opens");
    let v = extensions_loaded(&mut sub).await;

    // The two strings collapse into a single key, so the fake ends up
    // sending ONE row: what is asserted is its flag, which with the honest
    // name's bytes has to be FALSE.
    assert_eq!(v.errors.len(), 2, "both rows arrive: {:?}", v.errors);

    // The one with raw bytes: it gets marked, and with the bytes on hand it
    // gets marked for the right reason — `display_name` saw they were not
    // UTF-8 — and not by the heuristic.
    let converted = v
        .errors
        .iter()
        .find(|e| e.dir == String::from_utf8_lossy(&raw))
        .expect("the raw-bytes row is there");
    assert!(
        converted.hostile,
        "what is painted differs from what there is and does NOT say so: {:?}",
        converted.dir
    );

    // And the honest one: NOT marked. This is the half that only passes
    // with the bytes on hand; with the string's heuristic it came out
    // over-marked.
    let row_clean = v
        .errors
        .iter()
        .find(|e| e.dir == "caf\u{FFFD}")
        .expect("the honest row is there");
    assert!(
        !row_clean.hostile,
        "a directory that IS NAMED `caf\u{FFFD}` was not converted: marking it \
         is the false positive the bytes exist to remove"
    );
    for c in row_clean.dir.chars() {
        assert!(
            !norte_encoding::is_terminal_hazard(c),
            "a hazard crossed unmasked: {:?}",
            row_clean.dir
        );
    }
}

/// And the other half, isolated: WITHOUT bytes — a 0.52 peer — the heuristic
/// marks that same honest row, and that is the false positive #265 removes.
#[tokio::test]
async fn without_the_bytes_an_honest_name_with_a_replacement_gets_over_flagged() {
    let honest = "caf\u{FFFD}".to_owned();
    let mut backend = tree_with_plugins(Vec::new(), &[]);
    std::sync::Arc::get_mut(&mut backend)
        .expect("single reference")
        .load_errors = vec![(honest, "el manifiesto no parsea".to_owned())];
    // Deliberately WITHOUT `payload_bytes`: it is a 0.52 daemon.
    let (h, _snap) = host_tree(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = next_extensions(&mut sub).await.expect("opens");
    let v = extensions_loaded(&mut sub).await;
    assert!(
        v.errors[0].hostile,
        "against an old peer the heuristic is all there is, and it marks \
         too much rather than too little"
    );
}

/// #268 — two marks that are ONE name at the destination are rejected as a
/// whole.
///
/// On an ext4, `README.txt` and `readme.txt` are two files; on NTFS or APFS
/// they are one. Queueing both lets one win — which one is not
/// deterministic — and the other fail with no explanation, on an arbitrary
/// member of the pair.
///
/// The test runs the THREE pairs from the canonical corpus, which are three
/// different folds: ASCII case, NFC/NFD normalization, and an ext4 `+F`'s
/// full fold. A fix that only looked at case would pass the first and fail
/// the other two.
#[tokio::test]
async fn two_marks_that_fold_to_the_same_name_do_not_queue() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_de = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} is there"))
            .bytes
            .clone()
    };
    let pairs = [
        ("ascii_case_twin_upper", "ascii_case_twin_lower"),
        ("nfc_e_acute", "nfd_e_acute"),
        ("ext4_full_fold_ss", "ext4_full_fold_es_zett"),
    ];
    for (a, b) in pairs {
        let (one, other) = (bytes_de(a), bytes_de(b));
        assert_ne!(
            one, other,
            "[{a}/{b}] the premise: they are different bytes"
        );

        let mut f = Fake::default();
        f.put(
            "mem:///casa",
            vec![
                (b"docs".to_vec(), true),
                (b"notas.txt".to_vec(), false),
                (one.clone(), false),
                (other.clone(), false),
            ],
        );
        f.put("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
        // The DESTINATION folds: an APFS, an NTFS or an ext4 `+F`. Without
        // this flag the case could not be written, which is what the issue
        // said.
        f.capabilities.insert(
            "mem:///casa/docs".to_owned(),
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::FULL_FOLD,
                max_path: None,
            },
        );
        let backend = Arc::new(f);
        let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
        let mut sub = h.subscribe();
        // That the destination's fold has arrived: it is requested on
        // landing, not in front of the dialog, so it has to be waited for.
        wait_snapshot(&h, &mut sub, "the destination says how it folds", |_| true).await;
        mark_all(&h, &mut sub, 1).await;
        let ack = h.dispatch(press("F5")).await.expect("host alive");

        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-batch-folds-to-one"),
            "[{a}/{b}] {ack:?}"
        );
        assert!(
            backend.transfers.lock().expect("transferencias").is_empty(),
            "[{a}/{b}] not one was queued: the batch is rejected AS A WHOLE"
        );
    }
}

/// And at a destination that does NOT fold, the same two marks are two files
/// and the batch goes out. The check must not cost the legitimate
/// operation.
#[tokio::test]
async fn two_box_twins_toward_a_sensitive_destination_if_queued() {
    let corpus = norte_testkit::corpus::hostile_names();
    let bytes_de = |id: &str| {
        corpus
            .iter()
            .find(|n| n.id == id)
            .expect("the fixture is there")
            .bytes
            .clone()
    };
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (bytes_de("ascii_case_twin_upper"), false),
            (bytes_de("ascii_case_twin_lower"), false),
        ],
    );
    f.put("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    // No flag = a regular ext4, which distinguishes case.
    let backend = Arc::new(f);
    let (h, _snap) = two_panes_with_separate_destination(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    wait_snapshot(&h, &mut sub, "the destination says how it folds", |_| true).await;
    mark_all(&h, &mut sub, 1).await;
    let ack = h.dispatch(press("F5")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Applied { .. }),
        "an ext4 distinguishes case: they are two files and the batch is legitimate: {ack:?}"
    );
}

/// #311: computing checksums in the window. The Task is queued with what is
/// marked, and when its REPORT arrives a dialog opens with one row per file
/// and the option to copy the list.
#[tokio::test]
async fn calculating_checksums_opens_the_dialog_with_its_rows() {
    let backend = fake_tree();
    // The report the fake daemon will return: one digest for `notes.txt`.
    *backend.checksums_report.lock().expect("informe") =
        norte_proto::methods::FsChecksumReportResult {
            entries: vec![norte_proto::methods::ChecksumEntry {
                path: norte_proto::VPath::parse("mem:///casa/notas.txt").expect("wire"),
                digest: Some("ab".repeat(32)),
                miss: None,
            }],
            algo: norte_proto::methods::ChecksumAlgo::Sha256,
            pending: 0,
        };
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "k".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");

    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1, "ONE dialog opens with the checksums");
    assert_eq!(dialogs[0].body.len(), 1, "one row per file");
    assert!(
        dialogs[0].body[0].text.contains("ababab"),
        "with its digest truncated: {:?}",
        dialogs[0].body[0].text
    );
    assert!(
        dialogs[0].choices.iter().any(|c| c.id == "confirm"),
        "and with the COPY option, which is the only thing done with a list of digests"
    );
    assert_eq!(
        backend.checksums_requested.lock().expect("sumas").len(),
        1,
        "ONE batch was requested"
    );
}

/// A PARTIAL report — a cancelled Task leaves `pending` above zero — is
/// compared to nothing: accusing files nobody ever read is the worst
/// possible mistake in the tool that exists to check.
#[tokio::test]
async fn a_partial_report_does_not_open_a_verdict() {
    let backend = fake_tree();
    *backend.checksums_report.lock().expect("informe") =
        norte_proto::methods::FsChecksumReportResult {
            entries: Vec::new(),
            algo: norte_proto::methods::ChecksumAlgo::Sha256,
            // What a cancellation leaves: the Task ended and work remains.
            pending: 3,
        };
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "k".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    // It waits for the report to HAVE come back: what is asserted is that
    // with it in hand nothing opens, not that it had not arrived yet.
    until(&backend, "the checksum report requested", |f| {
        (!f.checksums_informes_requests
            .lock()
            .expect("informes")
            .is_empty())
        .then_some(())
    })
    .await;
    settle().await;
    let f = snapshot(&host, &mut sub).await;
    assert!(
        f.dialogs.is_empty(),
        "a partial report opens no verdict: {:?}",
        f.dialogs
    );
}

/// `alt+A`, the chord the three native presets give to `pane.chmod`.
pub(super) fn alt_a() -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "A".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    })
}

/// #314: the window changes permissions. The dialog carries a text field —
/// the mode, in octal — says how many entries it applies to, and confirming
/// queues the Task with the mode that was typed.
///
/// The rule for what a valid mode is is the SHARED one
/// (`norte_frontend::chmod::parse_mode`), the same one the terminal uses:
/// two different readings of `755` in two frontends would be two different
/// permissions.
#[tokio::test]
async fn changing_permissions_types_and_queues() {
    let backend = fake_tree();
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(alt_a()).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs[0].id;
    assert!(
        dialogs[0].input.is_some(),
        "the permissions dialog says typing happens here"
    );

    host.dispatch(UiAction::DialogInput {
        id,
        text: "0750".to_owned(),
    })
    .await
    .expect("host alive");
    let _ = next_dialogs(&mut sub).await;
    host.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let batches = annotated(&backend, "the permissions batch queued", 1, |f| {
        f.permissions.lock().expect("permisos").clone()
    })
    .await;
    assert_eq!(batches.len(), 1, "ONE batch was queued");
    assert_eq!(batches[0].1, 0o750, "in OCTAL: 750, not decimal 750");
    assert_eq!(batches[0].0.len(), 1, "on what is under the cursor");
}

/// A mode that is not valid queues nothing, and it is said.
#[tokio::test]
async fn an_invalid_mode_changes_nothing() {
    let backend = fake_tree();
    let (host, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = host.subscribe();

    host.dispatch(alt_a()).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs[0].id;
    host.dispatch(UiAction::DialogInput {
        id,
        text: "899".to_owned(),
    })
    .await
    .expect("host alive");
    let _ = next_dialogs(&mut sub).await;
    let ack = host
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { .. }),
        "899 is not octal and it is said: {ack:?}"
    );
    settle().await;
    assert!(
        backend.permissions.lock().expect("permisos").is_empty(),
        "and nothing was queued"
    );
}
